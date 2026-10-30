//! Popped-out tabs: each `detached` tab lives in its own shell-core detached window, labelled from
//! the tab id ([`window_label`]) so shell-core's geometry keeps its size and position across
//! launches. The workspace's `detached` flags decide which windows exist ([`plan`]); [`reconcile`]
//! brings the windows in line after every change, and at launch reopens every popped-out tab.

use crate::state::AppState;
use crate::tabs::{self, Target};
use flotilla_core::model::{TabId, Workspace};
use shell_core::detach::{detach_token, detached_label, open_detached, wire_return, DetachSpec};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Manager};

/// A popped-out window's size the first time its tab pops out; geometry restores it after.
const FIRST_SIZE: (f64, f64) = (1100.0, 800.0);

/// The detached window for tab `id`. The token is the tab's content-webview label (curator's
/// scheme), stable because tab ids are persisted and never reused.
pub fn window_label(id: &str) -> String {
    detached_label(&tabs::label(id))
}

/// The tab a detached window's label belongs to.
pub fn tab_of(window_label: &str) -> Option<TabId> {
    tabs::tab_id(detach_token(window_label)?)
}

/// Marks tab `id` popped out. If it was the active tab, the nearest live visible tab takes over
/// the main window ([`tabs::after_unload`]), else nothing is shown there.
pub fn pop_out(ws: &mut Workspace, created: &HashSet<TabId>, id: &TabId) -> Result<(), String> {
    match ws.find(id) {
        None => return Err("unknown tab".into()),
        Some((_, t)) if t.detached => return Err("already popped out".into()),
        Some(_) => {}
    }
    if ws.active.as_ref() == Some(id) {
        let next = tabs::after_unload(ws, created, id);
        ws.active = None;
        if let Some(n) = next {
            ws.activate(&n);
        }
    }
    ws.set_detached(id, true);
    Ok(())
}

/// After an operation that may have raised a tab (selecting its row, opening its document again):
/// if the raised tab is popped out, the main window goes on showing `before` (when that is still
/// an open, docked tab, else nothing) and the raised tab's window is returned, to be raised.
pub fn divert(ws: &mut Workspace, before: Option<TabId>) -> Option<TabId> {
    let raised = ws.active.clone()?;
    if !ws.find(&raised).is_some_and(|(_, t)| t.detached) {
        return None;
    }
    ws.active = before.filter(|b| ws.find(b).is_some_and(|(_, t)| !t.detached));
    Some(raised)
}

/// Which detached windows to open and close, given the tabs whose window is `open` now.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WindowPlan {
    /// Popped-out tabs with no window (just popped out, or restored at launch).
    pub open: Vec<Target>,
    /// Windows whose tab is closed or docked.
    pub close: Vec<TabId>,
}

pub fn plan(ws: &Workspace, open: &HashSet<TabId>) -> WindowPlan {
    let out = |id: &TabId| ws.find(id).is_some_and(|(_, t)| t.detached);
    let mut close: Vec<TabId> = open.iter().filter(|id| !out(id)).cloned().collect();
    close.sort();
    let open = ws
        .groups
        .iter()
        .flat_map(|g| {
            std::iter::once(&g.head)
                .chain(g.members.iter())
                .filter(|t| t.detached && !open.contains(&t.id))
                .map(|t| Target {
                    id: t.id.clone(),
                    home_url: t.home_url.clone(),
                    store: g.store.clone(),
                })
        })
        .collect();
    WindowPlan { open, close }
}

/// Set once the app is going down (⌘Q, or the main window closing). Every detached window's
/// `Destroyed` follows, and docking their tabs then would save them as docked.
static QUITTING: AtomicBool = AtomicBool::new(false);

pub fn mark_quitting() {
    QUITTING.store(true, Ordering::SeqCst);
}

/// The tabs whose detached window exists.
fn open_windows(app: &AppHandle) -> HashSet<TabId> {
    app.windows().keys().filter_map(|l| tab_of(l)).collect()
}

/// Opens and closes detached windows to match the workspace ([`plan`]). Main thread only; `ws` is
/// locked only to compute the plan. A window that cannot open docks its tab again.
pub fn reconcile(app: &AppHandle) {
    let open_now = open_windows(app);
    let state = app.state::<AppState>();
    let plan = plan(&state.ws.lock().unwrap(), &open_now);
    for id in &plan.close {
        // Its CloseRequested handler ends the page; `Destroyed` then finds no tab to dock.
        if let Some(w) = app.get_window(&window_label(id)) {
            let _ = w.close();
        }
    }
    let mut failed = false;
    for t in &plan.open {
        if let Err(e) = open(app, t) {
            state.ws.lock().unwrap().set_detached(&t.id, false);
            tabs::notice(app, format!("Couldn't pop out {}: {e}", t.home_url));
            failed = true;
        }
    }
    if failed {
        tabs::push_dto(app);
        crate::persist::mark_dirty(app);
    }
}

/// Opens tab `t`'s detached window with its page loaded from `home_url`, and docks the tab again
/// when the window closes.
fn open(app: &AppHandle, t: &Target) -> Result<(), String> {
    let title = {
        let state = app.state::<AppState>();
        let ws = state.ws.lock().unwrap();
        ws.find(&t.id).map(|(_, tab)| tab.title.clone())
    }
    .unwrap_or_default();
    let spec = DetachSpec {
        title,
        colour: None,
        width: FIRST_SIZE.0,
        height: FIRST_SIZE.1,
        panes: vec![],
        pane_band: None,
    };
    let label = open_detached(app, &tabs::label(&t.id), &spec, "flotilla", |win, size| {
        tabs::create(
            &win.as_ref().window(),
            t,
            shell_core::detach::birth_hole(size),
        )
        .map_err(|e| tauri::Error::Io(std::io::Error::other(e)))
    })
    .map_err(|e| e.to_string())?;
    let id = t.id.clone();
    let docking = app.clone();
    wire_return(app, &label, move || redock(&docking, &id));
    if let Some(w) = app.get_window(&label) {
        shell_core::web::content::wire_close_pages(&w);
    }
    Ok(())
}

/// Tab `id`'s window has closed: the tab returns to its group in the main window, as the active
/// tab. Nothing to do if the tab was closed (or docked) first, or the app is quitting.
fn redock(app: &AppHandle, id: &TabId) {
    if QUITTING.load(Ordering::SeqCst) {
        return;
    }
    {
        let state = app.state::<AppState>();
        let mut ws = state.ws.lock().unwrap();
        if !ws.find(id).is_some_and(|(_, t)| t.detached) {
            return;
        }
        ws.set_detached(id, false);
        ws.activate(id);
    }
    tabs::changed(app);
}

/// Brings tab `id`'s detached window to the front.
pub fn raise(app: &AppHandle, id: &TabId) {
    if let Some(w) = app.get_window(&window_label(id)) {
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Docks tab `id`: closes its window, whose `Destroyed` docks it ([`redock`]).
pub fn pop_in(app: &AppHandle, id: &TabId) {
    match app.get_window(&window_label(id)) {
        Some(w) => {
            let _ = w.close();
        }
        None => redock(app, id),
    }
}

/// Applies `op`, which may raise a tab ([`divert`]): a popped-out one has its window raised.
pub fn raising(app: &AppHandle, op: impl FnOnce(&mut Workspace)) {
    let raised = {
        let state = app.state::<AppState>();
        let mut ws = state.ws.lock().unwrap();
        let before = ws.active.clone();
        op(&mut ws);
        divert(&mut ws, before)
    };
    if let Some(id) = &raised {
        raise(app, id);
    }
    tabs::changed(app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_core::model::Opened;
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    const DOC_A: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit";
    const DOC_B: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt02/edit";

    /// A folder head with two docs; the second doc is active.
    fn ws() -> (Workspace, TabId, TabId, TabId) {
        let mut ws = Workspace::default();
        let Opened::Added(h) =
            ws.new_group(&url("https://drive.google.com/drive/folders/f1"), "default")
        else {
            panic!()
        };
        let Some(Opened::Added(a)) = ws.open_from(&h, &url(DOC_A)) else {
            panic!()
        };
        let Some(Opened::Added(b)) = ws.open_from(&h, &url(DOC_B)) else {
            panic!()
        };
        (ws, h, a, b)
    }

    fn set(ids: &[&TabId]) -> HashSet<TabId> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn window_labels_are_stable_per_tab_and_round_trip() {
        assert_eq!(window_label("t-3"), "shell-detach:main:t-3");
        assert_eq!(tab_of(&window_label("t-3")).as_deref(), Some("t-3"));
        assert_eq!(tab_of("main"), None);
        assert_eq!(tab_of("main:t-3"), None);
        assert_eq!(tab_of("shell-detach:other:t-3"), None);
    }

    #[test]
    fn popping_out_the_active_tab_hands_the_main_window_to_a_live_neighbour() {
        let (mut ws, h, _, b) = ws();
        pop_out(&mut ws, &set(&[&h, &b]), &b).unwrap();
        assert!(ws.find(&b).unwrap().1.detached);
        assert_eq!(ws.active, Some(h));
    }

    #[test]
    fn popping_out_the_last_live_tab_leaves_the_main_window_blank() {
        let (mut ws, _, _, b) = ws();
        pop_out(&mut ws, &set(&[&b]), &b).unwrap();
        assert_eq!(ws.active, None);
    }

    #[test]
    fn popping_out_another_tab_keeps_the_active_one() {
        let (mut ws, _, a, b) = ws();
        pop_out(&mut ws, &set(&[&b]), &a).unwrap();
        assert!(ws.find(&a).unwrap().1.detached);
        assert_eq!(ws.active, Some(b));
    }

    #[test]
    fn a_tab_pops_out_once() {
        let (mut ws, _, a, _) = ws();
        pop_out(&mut ws, &HashSet::new(), &a).unwrap();
        assert!(pop_out(&mut ws, &HashSet::new(), &a).is_err());
        assert!(pop_out(&mut ws, &HashSet::new(), &"t-99".to_string()).is_err());
    }

    #[test]
    fn selecting_a_popped_out_row_raises_its_window_and_keeps_the_main_tab() {
        let (mut ws, _, a, b) = ws();
        ws.set_detached(&a, true);
        let before = ws.active.clone();
        ws.activate(&a);
        assert_eq!(divert(&mut ws, before), Some(a));
        assert_eq!(ws.active, Some(b));
    }

    #[test]
    fn selecting_a_docked_row_diverts_nothing() {
        let (mut ws, h, _, b) = ws();
        ws.activate(&h);
        assert_eq!(divert(&mut ws, Some(b)), None);
        assert_eq!(ws.active, Some(h));
    }

    #[test]
    fn opening_a_popped_out_doc_again_raises_its_window() {
        let (mut ws, h, a, b) = ws();
        ws.set_detached(&a, true);
        ws.activate(&h);
        let before = ws.active.clone();
        assert_eq!(
            flotilla_core::nav::decide(&mut ws, &h, &url(DOC_A), flotilla_core::nav::Origin::Click),
            flotilla_core::nav::Decision::Open(Opened::Raised(a.clone()))
        );
        assert_eq!(divert(&mut ws, before), Some(a));
        assert_eq!(ws.active, Some(h));
        // The main window keeps nothing it cannot show.
        ws.set_detached(&b, true);
        ws.activate(&b);
        assert_eq!(divert(&mut ws, Some(b.clone())), Some(b));
        assert_eq!(ws.active, None);
    }

    #[test]
    fn relaunch_opens_a_window_for_every_popped_out_tab() {
        let (mut ws, h, a, b) = ws();
        ws.set_detached(&h, true);
        ws.set_detached(&b, true);
        let p = plan(&ws, &HashSet::new());
        let ids: Vec<&str> = p.open.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, [h.as_str(), b.as_str()]);
        assert_eq!(p.open[1].home_url, DOC_B);
        assert_eq!(p.open[1].store, "default");
        assert!(p.close.is_empty());
        // Already open: nothing to do.
        assert_eq!(plan(&ws, &set(&[&h, &b])), WindowPlan::default());
        // a is docked.
        assert!(!p.open.iter().any(|t| t.id == a));
    }

    #[test]
    fn closed_and_docked_tabs_lose_their_window() {
        let (mut ws, _, a, b) = ws();
        ws.set_detached(&a, true);
        ws.set_detached(&b, true);
        ws.close_tab(&a);
        ws.set_detached(&b, false);
        let p = plan(&ws, &set(&[&a, &b]));
        let mut close = vec![a, b];
        close.sort();
        assert_eq!(p.close, close);
        assert!(p.open.is_empty());
    }
}
