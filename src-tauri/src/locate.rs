//! Sorting a handed document into the group whose Drive folder holds it: the document's folders
//! are asked of Drive (`google::drive::Locator`) on a thread of its own, since the HTTP client
//! blocks, and the answer comes back to the main thread to move it out of Unsorted
//! (`Workspace::sort_into`). A document that can't be sorted keeps the reason on its row
//! (`AppState::unsorted_reason`). Also the Group menu's Connect and Disconnect Google Account.

use crate::commands::handed_front;
use crate::config::DEFAULT_STORE;
use crate::google::drive::{Located, Locator};
use crate::google::http::Http;
use crate::google::{accounts, oauth, Client};
use crate::state::AppState;
use crate::tabs::{self, Forward};
use flotilla_core::identity::{doc_key, head_container};
use flotilla_core::model::{Handed, TabId, Workspace};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use url::Url;

const NO_ACCOUNT: &str =
    "Connect a Google account (Group ▸ Connect Google Account…) to sort this into its folder's group";

/// The app's one locator, so its cached access tokens serve every lookup. Built by the first
/// lookup that runs.
static LOCATOR: OnceLock<Locator<Http>> = OnceLock::new();

fn locator(client: Client) -> Result<&'static Locator<Http>, String> {
    if let Some(l) = LOCATOR.get() {
        return Ok(l);
    }
    let http = Http::new()?;
    Ok(LOCATOR.get_or_init(|| Locator::new(http, accounts::APP, client)))
}

/// What a lookup of one Unsorted document needs, copied out of the workspace so no lock is held
/// over the network call: its provider and file id, and every group head's container for that
/// provider, where the walk up its folders can stop.
#[derive(Debug, PartialEq, Eq)]
struct Lookup {
    provider: String,
    file_id: String,
    containers: HashSet<String>,
}

/// `None` when `doc` is in no window's Unsorted or opens no known document.
fn lookup(ws: &Workspace, doc: &TabId) -> Option<Lookup> {
    let tab = ws
        .windows
        .iter()
        .flat_map(|w| &w.unsorted)
        .find(|t| &t.id == doc)?;
    let key = doc_key(DEFAULT_STORE, &Url::parse(&tab.home_url).ok()?)?;
    let containers = ws
        .windows
        .iter()
        .flat_map(|w| &w.groups)
        .filter_map(|g| head_container(&Url::parse(&g.head.home_url).ok()?))
        .filter(|(p, _)| *p == key.provider)
        .map(|(_, c)| c)
        .collect();
    Some(Lookup {
        provider: key.provider,
        file_id: key.id,
        containers,
    })
}

/// Why a document stays in Unsorted, for its row; `None` when there is nothing to do about it
/// (no account can see it, or no group shows any of its folders).
fn reason(located: &Located) -> Option<String> {
    match located {
        Located::NoAccount => Some(NO_ACCOUNT.into()),
        Located::Failed(e) => Some(format!("Couldn't look up its folder: {e}")),
        Located::NotVisible | Located::Ancestors(_) => None,
    }
}

/// Drops the reasons of documents no longer in any window's Unsorted.
pub fn prune(ws: &Workspace, reasons: &mut HashMap<TabId, String>) {
    reasons.retain(|id, _| {
        ws.windows
            .iter()
            .any(|w| w.unsorted.iter().any(|t| &t.id == id))
    });
}

/// The connected Google accounts the Group menu lists; none in a build without the OAuth client.
pub fn accounts() -> Vec<String> {
    match crate::google::client() {
        Some(_) => accounts::APP.list(),
        None => Vec::new(),
    }
}

/// The lookups in flight, by document: each begun lookup gets the next sequence number, and only
/// the latest of a document's lookups may settle it, so a slow earlier answer never overwrites a
/// newer one.
#[derive(Debug, Default)]
pub struct Lookups {
    next: u64,
    latest: HashMap<TabId, u64>,
}

impl Lookups {
    /// Starts a lookup of `doc`, superseding any still in flight.
    pub fn begin(&mut self, doc: &TabId) -> u64 {
        self.next += 1;
        self.latest.insert(doc.clone(), self.next);
        self.next
    }

    /// Whether the answer to lookup `seq` settles `doc`: it is the latest begun; then it is done.
    pub fn settled(&mut self, doc: &TabId, seq: u64) -> bool {
        if self.latest.get(doc) != Some(&seq) {
            return false;
        }
        self.latest.remove(doc);
        true
    }
}

/// Sets `doc`'s reason to `why` (`None` clears it), and says whether its row changed.
fn record(reasons: &mut HashMap<TabId, String>, doc: &TabId, why: Option<String>) -> bool {
    match why {
        Some(why) => reasons.insert(doc.clone(), why.clone()).as_ref() != Some(&why),
        None => reasons.remove(doc).is_some(),
    }
}

/// Each email once, in first-seen order.
fn unique<'a>(emails: impl Iterator<Item = &'a String>) -> Vec<&'a String> {
    let mut seen = HashSet::new();
    emails.filter(|e| seen.insert(*e)).collect()
}

/// One begun lookup: what it asks, and the sequence number it settles under.
struct Job {
    doc: TabId,
    seq: u64,
    lookup: Lookup,
}

/// Drive's answer to a [`Job`], and the accounts forgotten on the way.
struct Answer {
    doc: TabId,
    seq: u64,
    provider: String,
    located: Located,
    dropped: Vec<String>,
}

/// Snapshots `doc` for a lookup and begins it; `None` when it is not an Unsorted document.
fn begin(app: &AppHandle, doc: &TabId) -> Option<Job> {
    let state = app.state::<AppState>();
    let lookup = lookup(&state.ws.lock().unwrap(), doc)?;
    let seq = state.lookups.lock().unwrap().begin(doc);
    Some(Job {
        doc: doc.clone(),
        seq,
        lookup,
    })
}

/// Asks Drive; blocks, so never on the main thread or an async runtime's.
fn run(client: Client, job: Job) -> Answer {
    let Lookup {
        provider,
        file_id,
        containers,
    } = job.lookup;
    let (located, dropped) = match locator(client) {
        Ok(l) => l.ancestors(&file_id, &|id| containers.contains(id)),
        Err(e) => (Located::Failed(e), Vec::new()),
    };
    Answer {
        doc: job.doc,
        seq: job.seq,
        provider,
        located,
        dropped,
    }
}

/// Runs `jobs` one after another on a thread of their own, then settles their answers together
/// on the main thread.
fn spawn(app: &AppHandle, client: Client, jobs: Vec<Job>) {
    if jobs.is_empty() {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let answers: Vec<Answer> = jobs.into_iter().map(|j| run(client, j)).collect();
        let handle = app.clone();
        crate::defer(&app, move || settle(&handle, answers));
    });
}

/// Looks Unsorted document `doc` up in Drive and, on the main thread once Drive answers, sorts it
/// into the group showing the nearest of its folders ([`settle`]).
pub fn after_handoff(app: &AppHandle, doc: &TabId) {
    let Some(client) = crate::google::client() else {
        return;
    };
    spawn(app, client, begin(app, doc).into_iter().collect());
}

/// The main thread's half of a lookup. Each account Google revoked is reported once. An answer a
/// later lookup of its document superseded is dropped. A document whose folder a group shows moves
/// there ([`sort`]); a drag in the meantime wins, since only a document still in Unsorted moves. Otherwise
/// its row's reason becomes what it now is: why it couldn't be looked up, or none.
fn settle(app: &AppHandle, answers: Vec<Answer>) {
    for email in unique(answers.iter().flat_map(|a| &a.dropped)) {
        tabs::notice(
            app,
            format!(
                "Google account {email} was disconnected; connect it again from the Group menu"
            ),
        );
    }
    let state = app.state::<AppState>();
    let mut sorts = Vec::new();
    let mut changed = false;
    for a in &answers {
        if !state.lookups.lock().unwrap().settled(&a.doc, a.seq) {
            continue;
        }
        let why = match &a.located {
            Located::Ancestors(ids) => {
                let head = flotilla_core::route::route(&state.ws.lock().unwrap(), &a.provider, ids);
                if let Some(head) = head {
                    sorts.push((a.doc.clone(), head));
                }
                None
            }
            other => reason(other),
        };
        changed |= record(&mut state.unsorted_reason.lock().unwrap(), &a.doc, why);
    }
    if !sorts.is_empty() {
        let live = tabs::live(app);
        let (fronts, raised) = crate::write::through(app, |ws| {
            let mut fronts = Vec::new();
            let raised = crate::detach::apply(ws, |ws| fronts = sort(ws, &sorts, &live));
            (fronts, raised)
        });
        if let Some(id) = &raised {
            crate::detach::raise(app, id);
        }
        for front in &fronts {
            tabs::bring(app, Some(front));
        }
        tabs::changed(app);
    } else if changed {
        tabs::push_dto(app);
    }
    if answers.iter().any(|a| !a.dropped.is_empty()) {
        crate::menu::refresh(app);
    }
}

/// Sorts each `(document, head)` that still can be, and answers the windows to bring forward: a
/// document that left for another window shows there, so that window comes forward (reopened if
/// closed) as it does for a handed link ([`handed_front`]).
fn sort(ws: &mut Workspace, sorts: &[(TabId, TabId)], live: &HashSet<TabId>) -> Vec<Forward> {
    let mut fronts = Vec::new();
    for (doc, head) in sorts {
        let Some(from) = tabs::window_title_of(ws, doc) else {
            continue;
        };
        let Some(id) = ws.sort_into(doc, head, live) else {
            continue;
        };
        if tabs::window_title_of(ws, &id).as_ref() == Some(&from) {
            continue;
        }
        let front = handed_front(ws, &from, Some(&Handed::Grouped(id)), None);
        if let Some(front) = front.filter(|f| !fronts.contains(f)) {
            fronts.push(front);
        }
    }
    fronts
}

/// The Unsorted documents a lookup left with a reason, in sidebar order. A document moved to
/// Unsorted by hand has none, so a retry never takes it out of where the user put it.
fn retry(ws: &Workspace, reasons: &HashMap<TabId, String>) -> Vec<TabId> {
    ws.windows
        .iter()
        .flat_map(|w| &w.unsorted)
        .filter(|t| reasons.contains_key(&t.id))
        .map(|t| t.id.clone())
        .collect()
}

/// The Unsorted documents left with a reason ([`retry`]) looked up again, one after another on
/// one thread: a newly connected account may see documents no account could before.
fn retry_unsorted(app: &AppHandle) {
    let Some(client) = crate::google::client() else {
        return;
    };
    let docs = {
        let state = app.state::<AppState>();
        let ws = state.ws.lock().unwrap();
        let reasons = state.unsorted_reason.lock().unwrap();
        retry(&ws, &reasons)
    };
    let jobs = docs.iter().filter_map(|d| begin(app, d)).collect();
    spawn(app, client, jobs);
}

/// Whether a Connect is waiting on the browser.
static CONNECTING: AtomicBool = AtomicBool::new(false);

/// One run of a flag's work: held while it runs, and the flag clears when it drops, on every exit
/// of the thread holding it, a panic included.
struct Flight(&'static AtomicBool);

impl Flight {
    /// `None` while another run holds `flag`.
    fn begin(flag: &'static AtomicBool) -> Option<Flight> {
        (!flag.swap(true, Ordering::AcqRel)).then_some(Flight(flag))
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Connect Google Account…: consent in the default browser, on a thread of its own since it waits
/// for the browser. Choosing it again while one waits does nothing ([`Flight`]), so no second
/// consent page races the first. A connected account is listed in the Group menu and the Unsorted
/// documents left with a reason are looked up again; a consent left unanswered until it times out
/// raises nothing.
pub fn connect(app: &AppHandle) {
    let Some(client) = crate::google::client() else {
        return;
    };
    let Some(flight) = Flight::begin(&CONNECTING) else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        let connected = Http::new().and_then(|http| oauth::connect(&client, &http, &accounts::APP));
        drop(flight);
        let handle = app.clone();
        crate::defer(&app, move || match connected {
            Ok(_) => {
                crate::menu::refresh(&handle);
                retry_unsorted(&handle);
            }
            Err(e) if e == "timed out" => {}
            Err(e) => tabs::notice(&handle, format!("Couldn't connect a Google account: {e}")),
        });
    });
}

/// Disconnect Google Account ▸ `email`: asks Google to revoke its grant, on a thread of its own and
/// best effort ([`oauth::revoke`]), and forgets it whatever Google answers; the Group menu drops it.
pub fn disconnect(app: &AppHandle, email: &str) {
    if let Some(refresh) = accounts::APP.refresh_token(email) {
        std::thread::spawn(move || {
            if let Ok(http) = Http::new() {
                oauth::revoke(&http, &refresh);
            }
        });
    }
    accounts::APP.forget(email);
    crate::menu::refresh(app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::testing::{new_group, one_window};

    const DOC: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit";

    #[test]
    fn a_lookup_names_the_file_and_every_heads_folder() {
        let mut ws = one_window();
        new_group(
            &mut ws,
            "https://drive.google.com/drive/folders/1FoLdErFoLdErFoLdEr01",
        );
        new_group(&mut ws, "https://example.com/");
        let Some(Handed::Unsorted(d)) = ws.receive("W", None, DOC, "default") else {
            panic!()
        };
        assert_eq!(
            lookup(&ws, &d),
            Some(Lookup {
                provider: "google".into(),
                file_id: "1AbCdEfGhIjKlMnOpQrSt01".into(),
                containers: HashSet::from(["1FoLdErFoLdErFoLdEr01".to_string()]),
            })
        );
        // A group head is not in Unsorted.
        let head = ws.windows[0].groups[0].head.id.clone();
        assert_eq!(lookup(&ws, &head), None);
    }

    #[test]
    fn only_a_missing_account_or_a_failure_is_a_reason() {
        assert_eq!(reason(&Located::NoAccount).as_deref(), Some(NO_ACCOUNT));
        assert_eq!(
            reason(&Located::Failed("timeout".into())).as_deref(),
            Some("Couldn't look up its folder: timeout")
        );
        assert_eq!(reason(&Located::NotVisible), None);
        assert_eq!(reason(&Located::Ancestors(vec!["root".into()])), None);
    }

    #[test]
    fn reasons_go_with_their_documents_leaving_unsorted() {
        let mut ws = one_window();
        let Some(Handed::Unsorted(d)) = ws.receive("W", None, DOC, "default") else {
            panic!()
        };
        let gone = TabId::from("t-gone");
        let mut reasons = HashMap::from([(d.clone(), "x".into()), (gone, "y".into())]);
        prune(&ws, &mut reasons);
        assert_eq!(reasons.into_keys().collect::<Vec<_>>(), [d]);
    }

    /// An answer counts only while no later lookup of its document has begun, and once.
    #[test]
    fn only_the_latest_lookup_of_a_document_settles() {
        let (a, b) = (TabId::from("t-a"), TabId::from("t-b"));
        let mut l = Lookups::default();
        let first = l.begin(&a);
        let other = l.begin(&b);
        let second = l.begin(&a);
        assert!(!l.settled(&a, first));
        assert!(l.settled(&b, other));
        assert!(l.settled(&a, second));
        assert!(!l.settled(&a, second));
    }

    /// Whether the row changed: a reason set, replaced or cleared.
    #[test]
    fn recording_a_reason_says_whether_the_row_changed() {
        let d = TabId::from("t-d");
        let mut reasons = HashMap::new();
        assert!(!record(&mut reasons, &d, None));
        assert!(record(&mut reasons, &d, Some("x".into())));
        assert!(!record(&mut reasons, &d, Some("x".into())));
        assert!(record(&mut reasons, &d, Some("y".into())));
        assert!(record(&mut reasons, &d, None));
        assert!(reasons.is_empty());
    }

    /// A document sorted into another window brings that window forward, as a handed link's does;
    /// one sorted where it already is brings nothing.
    #[test]
    fn a_sort_into_another_window_brings_it_forward() {
        use flotilla_core::model::{opened_id, ConfigWindow};
        let window = |t: &str| ConfigWindow {
            unsorted: vec![],
            title: t.into(),
            groups: vec![],
        };
        let mut ws = Workspace::from_config(&[window("W"), window("V")]);
        let folder = |n: u32| Url::parse(&format!("https://drive.google.com/drive/folders/f{n}"));
        let here = opened_id(ws.new_group("W", &folder(1).unwrap(), "default").unwrap());
        let there = opened_id(ws.new_group("V", &folder(2).unwrap(), "default").unwrap());
        let doc2 = DOC.replace("01/", "02/");
        let Some(Handed::Unsorted(a)) = ws.receive("W", None, DOC, "default") else {
            panic!()
        };
        let Some(Handed::Unsorted(b)) = ws.receive("W", None, &doc2, "default") else {
            panic!()
        };
        let live = HashSet::new();
        assert_eq!(sort(&mut ws, &[(a, here)], &live), []);
        assert_eq!(
            sort(&mut ws, &[(b.clone(), there.clone())], &live),
            [Forward::Window("V".into())]
        );
        assert_eq!(ws.group_of(&there).unwrap().members[0].id, b);
        // Already sorted: nothing moves, nothing comes forward.
        assert_eq!(sort(&mut ws, &[(b, there)], &live), []);
    }

    /// Only a handed document left in Unsorted with a reason is looked up again; one moved to
    /// Unsorted by hand carries none and stays where the user put it.
    #[test]
    fn a_retry_takes_only_documents_left_with_a_reason() {
        let mut ws = one_window();
        let doc2 = DOC.replace("01/", "02/");
        let Some(Handed::Unsorted(left)) = ws.receive("W", None, DOC, "default") else {
            panic!()
        };
        let Some(Handed::Unsorted(by_hand)) = ws.receive("W", None, &doc2, "default") else {
            panic!()
        };
        let gone = TabId::from("t-gone");
        let reasons = HashMap::from([(left.clone(), "x".into()), (gone, "y".into())]);
        assert_eq!(retry(&ws, &reasons), [left]);
        assert!(!retry(&ws, &reasons).contains(&by_hand));
    }

    /// A second Connect while one runs does nothing; once it ends, however, Connect works again.
    #[test]
    fn one_connect_at_a_time() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let first = Flight::begin(&FLAG).expect("nothing in flight");
        assert!(Flight::begin(&FLAG).is_none());
        drop(first);
        let again = Flight::begin(&FLAG);
        assert!(again.is_some());
        let panicked = std::thread::spawn(move || {
            let _held = again;
            panic!("connect failed");
        })
        .join();
        assert!(panicked.is_err());
        assert!(Flight::begin(&FLAG).is_some());
    }

    #[test]
    fn a_dropped_account_is_reported_once_per_pass() {
        let dropped = [vec!["a@x".to_string(), "b@x".into()], vec!["a@x".into()]];
        assert_eq!(unique(dropped.iter().flatten()), ["a@x", "b@x"]);
    }
}
