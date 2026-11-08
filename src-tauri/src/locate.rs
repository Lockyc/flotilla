//! Sorting a handed document into the group whose Drive folder holds it: the document's folders
//! are asked of Drive (`google::drive::Locator`) on a thread of its own, since the HTTP client
//! blocks, and the answer comes back to the main thread to move it out of Unsorted
//! (`Workspace::sort_into`). A document that can't be sorted keeps the reason on its row
//! (`AppState::unsorted_reason`). Also the Group menu's Connect and Disconnect Google Account.

use crate::commands::mutate;
use crate::config::DEFAULT_STORE;
use crate::google::drive::{Located, Locator};
use crate::google::http::Http;
use crate::google::{accounts, oauth, Client};
use crate::state::AppState;
use crate::tabs;
use flotilla_core::identity::{doc_key, head_container};
use flotilla_core::model::{TabId, Workspace};
use std::collections::{HashMap, HashSet};
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

/// Looks Unsorted document `doc` up in Drive and, on the main thread once Drive answers, sorts it
/// into the group showing the nearest of its folders. A drag in the meantime wins: only a document
/// still in Unsorted moves. Its earlier reason goes now; a new one is kept when it can't be looked
/// up. Each account Google revoked is reported once, as it is forgotten.
pub fn after_handoff(app: &AppHandle, doc: &TabId) {
    let Some(client) = crate::google::client() else {
        return;
    };
    let state = app.state::<AppState>();
    let found = lookup(&state.ws.lock().unwrap(), doc);
    state.unsorted_reason.lock().unwrap().remove(doc);
    let Some(Lookup {
        provider,
        file_id,
        containers,
    }) = found
    else {
        return;
    };
    let (app, doc) = (app.clone(), doc.clone());
    std::thread::spawn(move || {
        let (located, dropped) = match locator(client) {
            Ok(l) => l.ancestors(&file_id, &|id| containers.contains(id)),
            Err(e) => (Located::Failed(e), Vec::new()),
        };
        let handle = app.clone();
        crate::defer(&app, move || {
            settle(&handle, &doc, &provider, located, &dropped)
        });
    });
}

/// The main thread's half of [`after_handoff`].
fn settle(app: &AppHandle, doc: &TabId, provider: &str, located: Located, dropped: &[String]) {
    for email in dropped {
        tabs::notice(
            app,
            format!(
                "Google account {email} was disconnected; connect it again from the Group menu"
            ),
        );
    }
    match &located {
        Located::Ancestors(ids) => {
            let live = tabs::live(app);
            mutate(app, |ws| {
                if let Some(head) = flotilla_core::route::route(ws, provider, ids) {
                    ws.sort_into(doc, &head, &live);
                }
            });
        }
        other => {
            if let Some(why) = reason(other) {
                let state = app.state::<AppState>();
                state
                    .unsorted_reason
                    .lock()
                    .unwrap()
                    .insert(doc.clone(), why);
                tabs::push_dto(app);
            }
        }
    }
    if !dropped.is_empty() {
        crate::menu::refresh(app);
    }
}

/// [`after_handoff`] for every document in every window's Unsorted: a newly connected account
/// may see documents no account could before.
pub fn all_unsorted(app: &AppHandle) {
    let docs: Vec<TabId> = app
        .state::<AppState>()
        .ws
        .lock()
        .unwrap()
        .windows
        .iter()
        .flat_map(|w| w.unsorted.iter().map(|t| t.id.clone()))
        .collect();
    for doc in &docs {
        after_handoff(app, doc);
    }
}

/// Connect Google Account…: consent in the default browser, on a thread of its own since it waits
/// for the browser. A connected account is listed in the Group menu and every Unsorted document is
/// looked up again; a consent left unanswered until it times out raises nothing.
pub fn connect(app: &AppHandle) {
    let Some(client) = crate::google::client() else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        let connected = Http::new().and_then(|http| oauth::connect(&client, &http, &accounts::APP));
        let handle = app.clone();
        crate::defer(&app, move || match connected {
            Ok(_) => {
                crate::menu::refresh(&handle);
                all_unsorted(&handle);
            }
            Err(e) if e == "timed out" => {}
            Err(e) => tabs::notice(&handle, format!("Couldn't connect a Google account: {e}")),
        });
    });
}

/// Disconnect Google Account ▸ `email`: forgets its grant; the Group menu drops it.
pub fn disconnect(app: &AppHandle, email: &str) {
    accounts::APP.forget(email);
    crate::menu::refresh(app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::testing::{new_group, one_window};
    use flotilla_core::model::Handed;

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
}
