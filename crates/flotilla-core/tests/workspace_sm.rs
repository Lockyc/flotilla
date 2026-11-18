//! The Workspace model over random operation sequences: every rule the model states across
//! operations (open once, queued once, no detached active tab, the echo of a write and the session
//! round trip changing nothing) is checked after every step, whatever came before it.

use flotilla_core::identity::identity;
use flotilla_core::model::{
    ConfigGroup, ConfigTab, ConfigWindow, Dest, Handed, TabId, Workspace, DEFAULT_STORE,
};
use flotilla_core::session::Session;
use proptest::prelude::*;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use std::collections::HashSet;
use url::Url;

/// The config's windows: two in the default store, one in another.
const WINDOWS: &[(&str, &str)] = &[("A", DEFAULT_STORE), ("B", DEFAULT_STORE), ("C", "work")];

/// A handed link: a document, a folder, or text that is no web link, in one of the forms a
/// person or another app hands over.
#[derive(Clone, Debug)]
enum Link {
    Doc(u8, u8),
    Folder(u8, u8),
    NotWeb(u8),
}

const DOC_FORMS: u8 = 7;
const FOLDER_FORMS: u8 = 4;

fn doc_id(n: u8) -> String {
    format!("1AbCdEfGhIjKlMnOpQrSt{n:02}")
}

fn folder_id(n: u8) -> String {
    format!("1FoLdErAbCdEfGhIjKlMn{n:02}")
}

impl Link {
    fn text(&self) -> String {
        match *self {
            Link::Doc(n, form) => {
                let id = doc_id(n);
                let d = "https://docs.google.com/document";
                match form % DOC_FORMS {
                    0 => format!("{d}/d/{id}/edit"),
                    1 => format!("{d}/d/{id}/edit?usp=sharing"),
                    2 => format!("{d}/d/{id}/edit?tab=t.0#heading=h.x"),
                    3 => format!("{d}/u/0/d/{id}/edit"),
                    4 => format!("{d}/d/{id}/"),
                    5 => format!("https://drive.google.com/open?id={id}"),
                    _ => format!("  {d}/d/{id}/edit  "),
                }
            }
            Link::Folder(n, form) => {
                let id = folder_id(n);
                let d = "https://drive.google.com/drive";
                match form % FOLDER_FORMS {
                    0 => format!("{d}/folders/{id}"),
                    1 => format!("{d}/folders/{id}?usp=sharing"),
                    2 => format!("{d}/u/0/folders/{id}"),
                    _ => format!("{d}/folders/{id}/"),
                }
            }
            Link::NotWeb(n) => format!("note {n}"),
        }
    }

    fn url(&self) -> Option<Url> {
        Url::parse(self.text().trim()).ok()
    }
}

fn link() -> impl Strategy<Value = Link> {
    prop_oneof![
        4 => (0..6u8, any::<u8>()).prop_map(|(n, f)| Link::Doc(n, f)),
        2 => (0..3u8, any::<u8>()).prop_map(|(n, f)| Link::Folder(n, f)),
        1 => (0..2u8).prop_map(Link::NotWeb),
    ]
}

/// One operation. Indices pick a tab, window, Incoming item or Recent entry of whatever the
/// workspace holds when the step runs, modulo its count, so a shrunk sequence still means
/// something.
#[derive(Clone, Debug)]
enum Op {
    Receive(usize, Option<usize>, Link),
    Queue(usize, Link),
    NewGroup(usize, Link),
    OpenFrom(usize, Link),
    CloseTab(usize),
    CloseGroup(usize),
    MoveToGroup(usize, usize),
    MoveToUnsorted(usize, usize),
    SortInto(usize, usize),
    MakeOwnGroup(usize),
    SetDetached(usize, bool),
    Activate(usize),
    ReopenRecent(usize, usize),
    AddIncoming(usize, usize),
    IncomingAsGroup(usize),
    DismissIncoming(usize),
    /// The reload that follows the app's own write: the config the workspace writes, read back.
    Echo,
    /// A hand edit deletes a window from the config.
    DropWindow(usize),
    /// The config stops parsing: no window at all.
    DropAll,
    /// The config is fixed: every window it ever had is back.
    RestoreAll,
}

fn op() -> impl Strategy<Value = Op> {
    let i = || 0..8usize;
    prop_oneof![
        6 => (i(), proptest::option::of(i()), link()).prop_map(|(w, g, l)| Op::Receive(w, g, l)),
        1 => (i(), link()).prop_map(|(w, l)| Op::Queue(w, l)),
        3 => (i(), link()).prop_map(|(w, l)| Op::NewGroup(w, l)),
        3 => (i(), link()).prop_map(|(t, l)| Op::OpenFrom(t, l)),
        3 => i().prop_map(Op::CloseTab),
        2 => i().prop_map(Op::CloseGroup),
        2 => (i(), i()).prop_map(|(t, g)| Op::MoveToGroup(t, g)),
        1 => (i(), i()).prop_map(|(t, w)| Op::MoveToUnsorted(t, w)),
        2 => (i(), i()).prop_map(|(t, g)| Op::SortInto(t, g)),
        1 => i().prop_map(Op::MakeOwnGroup),
        2 => (i(), any::<bool>()).prop_map(|(t, d)| Op::SetDetached(t, d)),
        2 => i().prop_map(Op::Activate),
        2 => (i(), i()).prop_map(|(w, r)| Op::ReopenRecent(w, r)),
        2 => (i(), i()).prop_map(|(q, g)| Op::AddIncoming(q, g)),
        1 => i().prop_map(Op::IncomingAsGroup),
        1 => i().prop_map(Op::DismissIncoming),
        2 => Just(Op::Echo),
        1 => i().prop_map(Op::DropWindow),
        2 => Just(Op::DropAll),
        3 => Just(Op::RestoreAll),
    ]
}

/// The config file the workspace's windows write: what `write::through` leaves on disk.
fn to_config(ws: &Workspace) -> Vec<ConfigWindow> {
    let tab = |t: &flotilla_core::model::Tab| ConfigTab {
        title: t.title.clone(),
        url: t.home_url.clone(),
        detached: t.detached,
        named: t.named,
        session: t.session.clone(),
    };
    ws.windows
        .iter()
        .map(|w| ConfigWindow {
            title: w.title.clone(),
            store: w.store.clone(),
            unsorted: w.unsorted.iter().map(tab).collect(),
            groups: w
                .groups
                .iter()
                .map(|g| ConfigGroup {
                    name: g.head.title.clone(),
                    url: g.head.home_url.clone(),
                    session: g.session.clone(),
                    collapsed: g.collapsed,
                    detached: g.head.detached,
                    named: g.head.named,
                    tabs: g.members.iter().map(tab).collect(),
                })
                .collect(),
        })
        .collect()
}

/// The workspace and the windows a hand edit took out of its config, to put back.
struct Sut {
    ws: Workspace,
    removed: Vec<ConfigWindow>,
}

impl Sut {
    fn tab(&self, i: usize) -> Option<TabId> {
        let n = self.ws.tabs().count();
        (n > 0).then(|| self.ws.tabs().nth(i % n).unwrap().id.clone())
    }

    fn head(&self, i: usize) -> Option<TabId> {
        let heads: Vec<_> = self
            .ws
            .windows
            .iter()
            .flat_map(|w| &w.groups)
            .map(|g| g.head.id.clone())
            .collect();
        (!heads.is_empty()).then(|| heads[i % heads.len()].clone())
    }

    /// A window title: one the workspace has, or (past them) one it does not.
    fn window(&self, i: usize) -> String {
        let n = self.ws.windows.len();
        match i % (n + 1) {
            k if k < n => self.ws.windows[k].title.clone(),
            _ => "Gone".into(),
        }
    }

    fn incoming(&self, i: usize) -> Option<String> {
        let ids: Vec<_> = self
            .ws
            .windows
            .iter()
            .flat_map(|w| &w.incoming)
            .map(|q| q.id.clone())
            .collect();
        (!ids.is_empty()).then(|| ids[i % ids.len()].clone())
    }

    /// Every tab is live: a successor may be any tab.
    fn live(&self) -> HashSet<TabId> {
        self.ws.tabs().map(|t| t.id.clone()).collect()
    }

    fn rebase(&mut self, cfg: &[ConfigWindow]) {
        self.ws.rebase(Workspace::from_config(cfg));
    }
}

struct WorkspaceMachine;

impl ReferenceStateMachine for WorkspaceMachine {
    type State = ();
    type Transition = Op;

    fn init_state() -> BoxedStrategy<()> {
        Just(()).boxed()
    }

    fn transitions(_: &()) -> BoxedStrategy<Op> {
        op().boxed()
    }

    fn apply(state: (), _: &Op) {
        state
    }
}

/// The links waiting in any window's Incoming or in the stray queues.
fn queued_links(ws: &Workspace) -> impl Iterator<Item = String> + '_ {
    ws.windows
        .iter()
        .flat_map(|w| &w.incoming)
        .chain(ws.stray.values().flatten())
        .map(|q| q.link.clone())
}

/// Whether queued `link` is the page `url` opens, by the store-free rule Incoming ids key by.
fn same_page(link: &str, url: &Url) -> bool {
    Url::parse(link).is_ok_and(|q| identity("", &q) == identity("", url))
}

/// Whether `url` is open as some tab in `store`: what `receive` raises instead of opening.
fn is_open(ws: &Workspace, store: &str, url: &Url) -> bool {
    let key = identity(store, url);
    ws.tabs().any(|t| t.key == key)
}

/// The id of every Incoming item, in every window and the stray queues.
fn queued_ids(ws: &Workspace) -> Vec<String> {
    ws.windows
        .iter()
        .flat_map(|w| &w.incoming)
        .chain(ws.stray.values().flatten())
        .map(|q| q.id.clone())
        .collect()
}

impl StateMachineTest for WorkspaceMachine {
    type SystemUnderTest = Sut;
    type Reference = WorkspaceMachine;

    fn init_test(_: &()) -> Sut {
        let cfg: Vec<ConfigWindow> = WINDOWS
            .iter()
            .map(|(title, store)| ConfigWindow {
                title: (*title).into(),
                store: (*store).into(),
                ..Default::default()
            })
            .collect();
        Sut {
            ws: Workspace::from_config(&cfg),
            removed: vec![],
        }
    }

    fn apply(mut s: Sut, _: &(), op: Op) -> Sut {
        let live = s.live();
        match op {
            Op::Receive(w, g, l) => {
                let window = s.window(w);
                let group = g
                    .and_then(|g| s.head(g))
                    .and_then(|h| s.ws.find(&h).map(|(_, _, t)| t.title.clone()));
                let before = queued_ids(&s.ws);
                let handed = s.ws.receive(&window, group.as_deref(), &l.text());
                // A link opened leaves the queue, wherever it waited; one queued is queued once.
                match handed {
                    Some(
                        Handed::Raised(id)
                        | Handed::Grouped(id)
                        | Handed::Joined(id)
                        | Handed::Unsorted(id),
                    ) => {
                        assert!(s.ws.find(&id).is_some(), "{id} handed but not open");
                        let ids = queued_ids(&s.ws);
                        assert!(
                            ids.iter().all(|i| before.contains(i)),
                            "opening {l:?} queued something: {before:?} -> {ids:?}"
                        );
                        let url = l.url().expect("an opened link is a URL");
                        assert!(
                            !queued_links(&s.ws).any(|q| same_page(&q, &url)),
                            "{l:?} opened but still queued"
                        );
                    }
                    Some(Handed::Queued(id) | Handed::AlreadyQueued(id)) => {
                        assert!(queued_ids(&s.ws).contains(&id), "{id} reported queued");
                    }
                    None => {}
                }
            }
            // The shell queues a link only once the write that would have opened it failed
            // (`commands::requeue`), so never one that is open.
            Op::Queue(w, l) => {
                let window = s.window(w);
                let store =
                    s.ws.window_index(&window)
                        .map(|i| s.ws.windows[i].store.clone());
                let open = l
                    .url()
                    .zip(store)
                    .is_some_and(|(u, store)| is_open(&s.ws, &store, &u));
                if !open {
                    s.ws.queue(&window, &l.text(), None);
                }
            }
            Op::NewGroup(w, l) => {
                if let Some(url) = l.url() {
                    let window = s.window(w);
                    s.ws.new_group(&window, &url);
                }
            }
            Op::OpenFrom(t, l) => {
                if let (Some(from), Some(url)) = (s.tab(t), l.url()) {
                    s.ws.open_from(&from, &url);
                }
            }
            Op::CloseTab(t) => {
                if let Some(id) = s.tab(t) {
                    s.ws.close_tab(&id, &live);
                }
            }
            Op::CloseGroup(g) => {
                if let Some(h) = s.head(g) {
                    s.ws.close_group(&h, &live);
                }
            }
            Op::MoveToGroup(t, g) => {
                if let (Some(id), Some(h)) = (s.tab(t), s.head(g)) {
                    s.ws.move_tab(&id, Dest::Group(&h), &live);
                }
            }
            Op::MoveToUnsorted(t, w) => {
                if let Some(id) = s.tab(t) {
                    let window = s.window(w);
                    s.ws.move_tab(&id, Dest::Unsorted(&window), &live);
                }
            }
            Op::SortInto(t, g) => {
                if let (Some(id), Some(h)) = (s.tab(t), s.head(g)) {
                    s.ws.sort_into(&id, &h, &live);
                }
            }
            Op::MakeOwnGroup(t) => {
                if let Some(id) = s.tab(t) {
                    s.ws.make_own_group(&id);
                }
            }
            Op::SetDetached(t, d) => {
                if let Some(id) = s.tab(t) {
                    s.ws.set_detached(&id, d);
                }
            }
            Op::Activate(t) => {
                if let Some(id) = s.tab(t) {
                    s.ws.activate(&id);
                    s.ws.take_raise();
                }
            }
            Op::ReopenRecent(w, r) => {
                let window = s.window(w);
                let n = s.ws.recent.len();
                if n > 0 {
                    s.ws.reopen_recent(&window, r % n);
                }
            }
            Op::AddIncoming(q, g) => {
                if let (Some(item), Some(h)) = (s.incoming(q), s.head(g)) {
                    s.ws.add_incoming(&item, &h);
                }
            }
            Op::IncomingAsGroup(q) => {
                if let Some(item) = s.incoming(q) {
                    s.ws.incoming_as_group(&item);
                }
            }
            Op::DismissIncoming(q) => {
                if let Some(item) = s.incoming(q) {
                    s.ws.dismiss_incoming(&item);
                }
            }
            Op::Echo => {
                let cfg = to_config(&s.ws);
                s.rebase(&cfg);
            }
            Op::DropWindow(w) => {
                let mut cfg = to_config(&s.ws);
                if !cfg.is_empty() {
                    let gone = cfg.remove(w % cfg.len());
                    s.removed.push(gone);
                    s.rebase(&cfg);
                }
            }
            Op::DropAll => {
                s.removed.extend(to_config(&s.ws));
                s.rebase(&[]);
            }
            Op::RestoreAll => {
                let mut cfg = to_config(&s.ws);
                cfg.append(&mut s.removed);
                // The config's own checks refuse a document twice; the model keeps the first.
                s.rebase(&cfg);
            }
        }
        s
    }

    fn check_invariants(s: &Sut, _: &()) {
        let ws = &s.ws;

        // (a) Open once: each identity, in its store, is one tab across every window.
        let mut keys = HashSet::new();
        for t in ws.tabs() {
            assert!(keys.insert(t.key.clone()), "{:?} open twice", t.key);
        }

        // (b) Queued once: each Incoming id is one item across every window and the stray
        // queues.
        let ids = queued_ids(ws);
        let unique: HashSet<_> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "an Incoming item queued twice: {ids:?}"
        );

        // (c) A window shows a docked tab of its own, never a detached one.
        for w in &ws.windows {
            if let Some(a) = &w.active {
                let t = w.tabs().find(|t| &t.id == a);
                assert!(
                    t.is_some(),
                    "window {} shows {a}, which it does not hold",
                    w.title
                );
                assert!(
                    !t.unwrap().detached,
                    "window {} shows detached {a}",
                    w.title
                );
            }
        }

        // (e) The session round-trips: the config the workspace writes plus its session give the
        // same workspace back.
        let s2 = Session::of(ws);
        let mut back = Workspace::from_config(&to_config(ws));
        back.restore(s2.active, s2.recent, s2.incoming);
        assert_eq!(&back, ws, "the session did not round-trip");

        // (f) The echo of the app's own write changes nothing.
        let mut echoed = ws.clone();
        echoed.rebase(Workspace::from_config(&to_config(ws)));
        assert_eq!(
            &echoed, ws,
            "the echo of the workspace's own config changed it"
        );
    }
}

prop_state_machine! {
    // 64 cases keep `just test` quick; PROPTEST_CASES runs more.
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(64),
        ..ProptestConfig::default()
    })]

    #[test]
    fn workspace_rules_hold_over_any_operation_sequence(sequential 1..40 => WorkspaceMachine);
}
