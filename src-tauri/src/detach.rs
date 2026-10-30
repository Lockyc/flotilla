//! Popped-out tabs: each `detached` tab lives in its own shell-core detached window, labelled from
//! the tab id ([`window_label`]) so shell-core's geometry keeps its size and position across
//! launches. The workspace's `detached` flags decide which windows exist ([`plan`]); [`reconcile`]
//! brings the windows in line after every change, and at launch reopens every popped-out tab. A
//! popped-out tab stays in its workspace window, so its origin is that window's title: it returns
//! to whichever window holds that title when it docks, under whatever label it has then.

use crate::state::AppState;
use crate::tabs::{self, Target};
use flotilla_core::identity::is_tab_id;
use flotilla_core::model::{TabId, Workspace};
use shell_core::detach::{detach_token, detached_label, open_detached, wire_return, DetachSpec};
use shell_core::windows::WindowSet;
use std::collections::HashSet;
use tauri::{AppHandle, Manager};

/// A popped-out window's size the first time its tab pops out; geometry restores it after.
const FIRST_SIZE: (f64, f64) = (1100.0, 800.0);

/// The detached window for tab `id`. The token is the tab id: the FNV of its document key, so the
/// same document pops out under the same label (and geometry) whichever window it is in.
pub fn window_label(id: &str) -> String {
    detached_label(id)
}

/// The tab a detached window's label belongs to.
pub fn tab_of(window_label: &str) -> Option<TabId> {
    detach_token(window_label)
        .filter(|t| is_tab_id(t))
        .map(str::to_string)
}

/// Marks tab `id` popped out. If it was its window's active tab, the nearest live visible tab
/// (`created`: the webviews in that window) takes over ([`tabs::after_unload`]), else nothing is
/// shown there.
pub fn pop_out(ws: &mut Workspace, created: &HashSet<TabId>, id: &TabId) -> Result<(), String> {
    match ws.find(id) {
        None => return Err("unknown tab".into()),
        Some((_, _, t)) if t.detached => return Err("already popped out".into()),
        Some(_) => {}
    }
    if ws.is_active(id) {
        if let Some(n) = tabs::after_unload(ws, created, id) {
            ws.activate(&n);
        }
    }
    // Clears `active` if it is still this tab.
    ws.set_detached(id, true);
    Ok(())
}

/// Applies workspace operation `op` and returns the popped-out tab it activated, whose window is
/// to be raised ([`raise`]); the model leaves `active` on a docked tab. Every shell mutation runs
/// through this, so any operation that activates a popped-out tab raises its window.
pub fn apply(ws: &mut Workspace, op: impl FnOnce(&mut Workspace)) -> Option<TabId> {
    ws.take_raise();
    op(ws);
    ws.take_raise()
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
    let out = |id: &TabId| ws.find(id).is_some_and(|(_, _, t)| t.detached);
    let mut close: Vec<TabId> = open.iter().filter(|id| !out(id)).cloned().collect();
    close.sort();
    let open = ws
        .windows
        .iter()
        .flat_map(|w| w.groups.iter())
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

/// The tabs whose detached window exists.
fn open_windows(app: &AppHandle) -> HashSet<TabId> {
    app.windows().keys().filter_map(|l| tab_of(l)).collect()
}

/// Opens and closes detached windows to match the workspace ([`plan`]), and tells the
/// `WindowSet` how many there are (they keep the home surface away). Main thread only; `ws` is
/// locked only to compute the plan. A window that cannot open docks its tab again.
pub fn reconcile(app: &AppHandle) {
    let open_now = open_windows(app);
    let state = app.state::<AppState>();
    let (plan, out) = {
        let ws = state.ws.lock().unwrap();
        (
            plan(&ws, &open_now),
            ws.tabs().filter(|t| t.detached).count(),
        )
    };
    app.state::<WindowSet>().set_detached_count(app, out);
    for id in &plan.close {
        // Its CloseRequested handler ends the page; `Destroyed` then finds no tab to dock.
        if let Some(w) = app.get_window(&window_label(id)) {
            let _ = w.close();
        }
    }
    let mut failed = false;
    for t in &plan.open {
        if let Err(e) = open(app, t) {
            crate::write::through(app, |ws| ws.set_detached(&t.id, false));
            tabs::notice(app, format!("Couldn't pop out {}: {e}", t.home_url));
            failed = true;
        }
    }
    if failed {
        tabs::push_dto(app);
    }
}

/// Opens tab `t`'s detached window with its page loaded from `home_url`, and docks the tab again
/// when the window closes.
fn open(app: &AppHandle, t: &Target) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (title, window) = {
        let ws = state.ws.lock().unwrap();
        let tab = ws.find(&t.id).map(|(_, _, tab)| tab.title.clone());
        (tab.unwrap_or_default(), tabs::window_title_of(&ws, &t.id))
    };
    let colour = window.and_then(|w| state.config.lock().unwrap().colour_of(&w));
    let spec = DetachSpec {
        title,
        colour,
        width: FIRST_SIZE.0,
        height: FIRST_SIZE.1,
        panes: vec![],
        pane_band: None,
    };
    let label = open_detached(app, &t.id, &spec, "flotilla", |win, size| {
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
        crate::menu::follow_focus(app, &w);
    }
    Ok(())
}

/// Tab `id`'s window has closed: the tab returns to its group in its workspace window, as that
/// window's active tab, reopening the window (found by its title) if it was closed meanwhile.
/// Nothing to do if the tab was closed (or docked) first, or the app is quitting.
fn redock(app: &AppHandle, id: &TabId) {
    let set = app.state::<WindowSet>();
    if set.is_quitting() {
        return;
    }
    let home = tabs::window_title_of(&app.state::<AppState>().ws.lock().unwrap(), id);
    let closed = home
        .and_then(|title| set.label_for_title(&title))
        .filter(|label| app.get_window(label).is_none());
    if let Some(label) = closed {
        set.open_or_focus(app, &label);
    }
    let docked = crate::write::through(app, |ws| {
        if !ws.find(id).is_some_and(|(_, _, t)| t.detached) {
            return false;
        }
        ws.set_detached(id, false);
        ws.activate(id);
        true
    });
    if docked {
        tabs::changed(app);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::testing::{new_group, one_window};
    use flotilla_core::model::Opened;
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    const DOC_A: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit";
    const DOC_B: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt02/edit";

    /// A folder head with two docs; the second doc is active.
    fn ws() -> (Workspace, TabId, TabId, TabId) {
        let mut ws = one_window();
        let Opened::Added(h) = new_group(&mut ws, "https://drive.google.com/drive/folders/f1")
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
        const TAB: &str = "t-0123456789abcdef";
        assert_eq!(window_label(TAB), format!("shell-detach:{TAB}"));
        assert_eq!(tab_of(&window_label(TAB)).as_deref(), Some(TAB));
        assert_eq!(tab_of("club"), None);
        assert_eq!(tab_of(&format!("club:{TAB}")), None);
        assert_eq!(tab_of("shell-detach:other"), None);
        // The tab's webview inside its pop-out is a child of the pop-out's label.
        assert_eq!(
            tabs::tab_of(&tabs::label(&window_label(TAB), TAB)),
            Some((window_label(TAB).as_str(), TAB.to_string()))
        );
    }

    #[test]
    fn popping_out_the_active_tab_hands_its_window_to_a_live_neighbour() {
        let (mut ws, h, _, b) = ws();
        pop_out(&mut ws, &set(&[&h, &b]), &b).unwrap();
        assert!(ws.find(&b).unwrap().2.detached);
        assert_eq!(ws.windows[0].active, Some(h));
    }

    #[test]
    fn popping_out_the_last_live_tab_leaves_its_window_blank() {
        let (mut ws, _, _, b) = ws();
        pop_out(&mut ws, &set(&[&b]), &b).unwrap();
        assert_eq!(ws.windows[0].active, None);
    }

    #[test]
    fn popping_out_another_tab_keeps_the_active_one() {
        let (mut ws, _, a, b) = ws();
        pop_out(&mut ws, &set(&[&b]), &a).unwrap();
        assert!(ws.find(&a).unwrap().2.detached);
        assert_eq!(ws.windows[0].active, Some(b));
    }

    #[test]
    fn a_tab_pops_out_once() {
        let (mut ws, _, a, _) = ws();
        pop_out(&mut ws, &HashSet::new(), &a).unwrap();
        assert!(pop_out(&mut ws, &HashSet::new(), &a).is_err());
        assert!(pop_out(&mut ws, &HashSet::new(), &"t-99".to_string()).is_err());
    }

    #[test]
    fn selecting_a_popped_out_row_raises_its_window_and_keeps_the_active_tab() {
        let (mut ws, _, a, b) = ws();
        ws.set_detached(&a, true);
        assert_eq!(apply(&mut ws, |ws| ws.activate(&a)), Some(a));
        assert_eq!(ws.windows[0].active, Some(b));
    }

    #[test]
    fn selecting_a_docked_row_raises_nothing() {
        let (mut ws, h, _, _) = ws();
        assert_eq!(apply(&mut ws, |ws| ws.activate(&h)), None);
        assert_eq!(ws.windows[0].active, Some(h));
    }

    #[test]
    fn a_menu_action_that_activates_a_popped_out_tab_raises_its_window() {
        // Group ▸ Make Own Group on a popped-out member, as `commands::mutate` runs it.
        let (mut ws, _, a, b) = ws();
        ws.set_detached(&a, true);
        assert_eq!(apply(&mut ws, |ws| ws.make_own_group(&a)), Some(a));
        assert_eq!(ws.windows[0].active, Some(b));
    }

    #[test]
    fn a_raise_left_by_an_earlier_operation_is_not_replayed() {
        let (mut ws, h, a, _) = ws();
        ws.set_detached(&a, true);
        ws.activate(&a);
        assert_eq!(apply(&mut ws, |ws| ws.activate(&h)), None);
    }

    #[test]
    fn opening_a_popped_out_doc_again_raises_its_window() {
        let (mut ws, h, a, _) = ws();
        ws.set_detached(&a, true);
        ws.activate(&h);
        let mut decision = None;
        let raised = apply(&mut ws, |ws| {
            decision = Some(flotilla_core::nav::decide(
                ws,
                &h,
                &url(DOC_A),
                flotilla_core::nav::Origin::Click,
            ))
        });
        assert_eq!(
            decision,
            Some(flotilla_core::nav::Decision::Open(Opened::Raised(
                a.clone()
            )))
        );
        assert_eq!(raised, Some(a));
        assert_eq!(ws.windows[0].active, Some(h));
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
