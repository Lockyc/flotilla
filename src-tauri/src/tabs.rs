//! The main window's content webviews: one per shown tab, labelled `main:<TabId>`, created lazily
//! and brought in line with the workspace by [`sync`].

use crate::state::AppState;
use crate::MAIN_WINDOW;
use flotilla_core::model::{Tab, TabId, WindowWs, Workspace};
use flotilla_core::nav::{self, Decision, Origin};
use shell_core::compositing::HoleRect;
use shell_core::tabs::pick_live_neighbour;
use shell_core::web::content::{
    close_content_webview, create_content_webview, ContentHooks, ContentSpec, LinkOrigin,
};
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, Url, Window};

/// The loading bar's colour (sRGB, 0–1): the chrome's neutral blue.
const BAR_RGBA: (f64, f64, f64, f64) = (0.039, 0.518, 1.0, 1.0);

/// Narrows Google Drive's left navigation to an icon rail; a no-op on every other page.
const DRIVE_RAIL_JS: &str = include_str!("../../src/inject/drive-rail.js");

/// The content webview label for tab `id` in the main window.
pub fn label(id: &str) -> String {
    format!("{}:{id}", crate::MAIN_WINDOW)
}

/// The tab a main-window content webview label belongs to.
pub fn tab_id(label: &str) -> Option<TabId> {
    label
        .strip_prefix(crate::MAIN_WINDOW)?
        .strip_prefix(':')
        .map(str::to_string)
}

/// A tab whose webview is to be created.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub id: TabId,
    pub home_url: String,
    /// The group's login store: the webview's session seed.
    pub store: String,
}

/// What [`sync`] does to bring the main window's webviews in line with the workspace.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ViewPlan {
    /// Created webviews whose tab is no longer open here (closed, or popped out).
    pub close: Vec<TabId>,
    /// The active tab, when it has no webview yet. No other tab is ever created.
    pub create: Option<Target>,
    pub show: Option<TabId>,
    /// Every other remaining webview.
    pub hide: Vec<TabId>,
}

/// The workspace window the main window shows: the config's first.
pub fn shown(ws: &Workspace) -> Option<&WindowWs> {
    ws.windows.first()
}

/// The main window's active tab.
pub fn shown_active(ws: &Workspace) -> Option<TabId> {
    shown(ws).and_then(|w| w.active.clone())
}

/// Whether `id` is a docked tab of the window the main window shows.
fn shown_here(ws: &Workspace, id: &TabId) -> bool {
    ws.find(id).is_some_and(|(wi, _, t)| wi == 0 && !t.detached)
}

/// The tab to activate after unloading `id`: shell-core's `pick_live_neighbour` over the sidebar's
/// row order in `id`'s window, eligible = has a webview, is visible (not a member of a collapsed
/// group) and is not popped out. `None` leaves the hole blank; a cold tab is never woken to fill
/// it.
pub fn after_unload(ws: &Workspace, created: &HashSet<TabId>, id: &TabId) -> Option<TabId> {
    let (wi, _, _) = ws.find(id)?;
    let rows: Vec<(&Tab, bool)> = ws.windows[wi]
        .groups
        .iter()
        .flat_map(|g| {
            std::iter::once((&g.head, true)).chain(g.members.iter().map(move |m| (m, !g.collapsed)))
        })
        .collect();
    let at = rows.iter().position(|(t, _)| &t.id == id)?;
    let eligible: Vec<bool> = rows
        .iter()
        .map(|(t, visible)| *visible && !t.detached && &t.id != id && created.contains(&t.id))
        .collect();
    pick_live_neighbour(at, &eligible).map(|i| rows[i].0.id.clone())
}

/// `failed` tabs are not created again (see `AppState::failed`).
pub fn plan(ws: &Workspace, created: &HashSet<TabId>, failed: &HashSet<TabId>) -> ViewPlan {
    let here = |id: &TabId| shown_here(ws, id);
    let mut close: Vec<TabId> = created.iter().filter(|id| !here(id)).cloned().collect();
    close.sort();
    let show = shown_active(ws).filter(here);
    let create = show
        .as_ref()
        .filter(|id| !created.contains(*id) && !failed.contains(*id))
        .and_then(|id| ws.find(id))
        .map(|(_, g, t)| Target {
            id: t.id.clone(),
            home_url: t.home_url.clone(),
            store: g.store.clone(),
        });
    let mut hide: Vec<TabId> = created
        .iter()
        .filter(|id| here(id) && Some(*id) != show.as_ref())
        .cloned()
        .collect();
    hide.sort();
    ViewPlan {
        close,
        create,
        show,
        hide,
    }
}

/// Whether a page's reported title replaces the tab's stored one: only while the stored title is
/// still the tab's URL (what a new group or document is written with), so a name is written once
/// and never follows navigation. A page reports an empty title while it loads.
pub fn adopt_title(stored: &str, home_url: &str, reported: &str) -> bool {
    stored == home_url && !reported.trim().is_empty() && reported != stored
}

/// Gives tab `id` the page's reported title when [`adopt_title`] says so; whether it did.
pub fn retitled(ws: &mut Workspace, id: &TabId, reported: &str) -> bool {
    let adopt = ws
        .find(id)
        .is_some_and(|(_, _, t)| adopt_title(&t.title, &t.home_url, reported));
    if adopt {
        ws.set_title(id, reported.trim());
    }
    adopt
}

/// After any workspace change: bring the webviews, the popped-out windows and the sidebar in line,
/// and schedule a save. Main thread only. The main window goes first, so a tab just popped out has
/// released its webview's label before its window recreates it.
pub fn changed(app: &AppHandle) {
    sync(app);
    crate::detach::reconcile(app);
    crate::persist::mark_dirty(app);
}

/// Brings the main window's content webviews in line with the workspace ([`plan`]) and pushes the
/// sidebar DTO. Main thread only. `ws` is locked only to compute the plan.
pub fn sync(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(window) = app.get_window(MAIN_WINDOW) else {
        return;
    };
    let plan = {
        let ws = state.ws.lock().unwrap();
        let created = state.created.lock().unwrap();
        let mut failed = state.failed.lock().unwrap();
        failed.retain(|id| ws.find(id).is_some());
        plan(&ws, &created, &failed)
    };
    for id in &plan.close {
        if let Some(wv) = own_webview(&window, id) {
            close_content_webview(&wv);
        }
        state.created.lock().unwrap().remove(id);
    }
    if let Some(t) = &plan.create {
        let hole = *state.hole.lock().unwrap();
        match create(&window, t, hole) {
            Ok(()) => {
                state.created.lock().unwrap().insert(t.id.clone());
            }
            Err(e) => {
                state.failed.lock().unwrap().insert(t.id.clone());
                notice(app, format!("Couldn't open {}: {e}", t.home_url));
            }
        }
    }
    for id in &plan.hide {
        if let Some(wv) = own_webview(&window, id) {
            let _ = wv.hide();
        }
    }
    if let Some(wv) = plan.show.as_ref().and_then(|id| own_webview(&window, id)) {
        let _ = wv.show();
    }
    push_dto(app);
}

/// Sends the sidebar DTO to the chrome and rebuilds the menu bar, whose Group menu follows the
/// workspace.
pub fn push_dto(app: &AppHandle) {
    let state = app.state::<AppState>();
    let dto = {
        let ws = state.ws.lock().unwrap();
        let created = state.created.lock().unwrap();
        crate::sidebar::sidebar_dto(shown(&ws), &created)
    };
    let _ = app.emit_to(MAIN_WINDOW, "workspace", dto);
    crate::menu::refresh(app);
}

/// Shows `msg` in the sidebar's error bar, or holds it until the chrome mounts.
pub fn notice(app: &AppHandle, msg: String) {
    let now = app.state::<AppState>().notice.lock().unwrap().raise(msg);
    if let Some(msg) = now {
        let _ = app.emit_to(MAIN_WINDOW, "notice", msg);
    }
}

/// Tab `id`'s webview, only if it lives in `window` (a popped-out tab's would not).
pub(crate) fn own_webview(window: &Window, id: &str) -> Option<tauri::Webview> {
    window
        .get_webview(&label(id))
        .filter(|wv| wv.window().label() == window.label())
}

/// The page the mouse's back/forward buttons act on: the focused popped-out window's tab, else the
/// main window's active tab.
pub(crate) fn focused_webview(app: &AppHandle) -> Option<tauri::Webview> {
    let window = app.get_focused_window()?;
    if let Some(id) = crate::detach::tab_of(window.label()) {
        return window.get_webview(&label(&id));
    }
    if window.label() != MAIN_WINDOW {
        return None;
    }
    let active = shown_active(&app.state::<AppState>().ws.lock().unwrap())?;
    own_webview(&window, &active)
}

/// Creates tab `t`'s content webview in `window` (the main window, or its popped-out one).
pub(crate) fn create(window: &Window, t: &Target, hole: HoleRect) -> Result<(), String> {
    let url = Url::parse(&t.home_url).map_err(|e| e.to_string())?;
    let label = label(&t.id);
    let spec = ContentSpec {
        label: &label,
        url: &url,
        home_url: &t.home_url,
        session: &t.store,
        hole,
        bar_rgba: BAR_RGBA,
        extra_init: &[DRIVE_RAIL_JS],
    };
    create_content_webview(window, &spec, hooks())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn hooks() -> ContentHooks {
    ContentHooks {
        intercept: Some(Arc::new(intercept)),
        on_app_sentinel: None,
        on_title: Some(Arc::new(|wv, title| {
            let app = wv.app_handle().clone();
            let label = wv.label().to_string();
            let title = title.to_string();
            let _ = app
                .clone()
                .run_on_main_thread(move || retitle(&app, &label, &title));
        })),
        on_download: None,
    }
}

fn origin(o: LinkOrigin) -> Origin {
    match o {
        LinkOrigin::NewWindow => Origin::NewWindow,
        LinkOrigin::Click => Origin::Click,
        LinkOrigin::Navigation => Origin::Navigation,
    }
}

/// shell-core's `intercept`: runs inside WebKit's policy callback, so it only decides. On `Open` the
/// workspace already holds the new or raised tab; showing it is deferred to the main loop.
fn intercept(app: &AppHandle, label: &str, url: &Url, o: LinkOrigin) -> bool {
    let Some(id) = tab_id(label) else {
        return false;
    };
    let (decision, front, writes) = {
        let state = app.state::<AppState>();
        let mut ws = state.ws.lock().unwrap();
        let from_popout = ws.find(&id).is_some_and(|(_, _, t)| t.detached);
        let mut decision = Decision::PassThrough;
        let (raised, writes) = crate::write::change(&mut ws, |ws| {
            crate::detach::apply(ws, |ws| {
                decision = nav::decide(ws, &id, url, origin(o));
            })
        });
        (decision, forward(from_popout, raised), writes)
    };
    match decision {
        Decision::PassThrough => false,
        Decision::Open(_) => {
            let deferred = app.clone();
            let _ = app.run_on_main_thread(move || {
                crate::write::commit(&deferred, &writes);
                match &front {
                    Some(Forward::Detached(r)) => crate::detach::raise(&deferred, r),
                    Some(Forward::Main) => bring_main_forward(&deferred),
                    None => {}
                }
                changed(&deferred)
            });
            true
        }
    }
}

/// The window an opened (or raised) document is shown in, when it is not already in front.
#[derive(Debug, PartialEq, Eq)]
pub enum Forward {
    /// The popped-out tab's own window.
    Detached(TabId),
    /// The main window, behind the pop-out the link was clicked in.
    Main,
}

/// Which window to bring forward after a link in a tab opened a document: the popped-out tab the
/// open `raised`, else the main window when the link was clicked in a pop-out (`from_popout`),
/// else none (the main window is already in front).
pub fn forward(from_popout: bool, raised: Option<TabId>) -> Option<Forward> {
    match raised {
        Some(r) => Some(Forward::Detached(r)),
        None => from_popout.then_some(Forward::Main),
    }
}

/// Unminimizes and focuses the main window.
pub fn bring_main_forward(app: &AppHandle) {
    if let Some(w) = app.get_window(MAIN_WINDOW) {
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn retitle(app: &AppHandle, label: &str, title: &str) {
    let Some(id) = tab_id(label) else {
        return;
    };
    let (adopted, writes) = {
        let state = app.state::<AppState>();
        let mut ws = state.ws.lock().unwrap();
        crate::write::change(&mut ws, |ws| retitled(ws, &id, title))
    };
    if adopted {
        crate::write::commit(app, &writes);
        push_dto(app);
    }
}

/// A workspace of one empty window, `W`, and a way to head a group in it: what the shell's tests
/// build on.
#[cfg(test)]
pub(crate) mod testing {
    use flotilla_core::model::{ConfigWindow, Opened, Workspace};

    pub fn one_window() -> Workspace {
        Workspace::from_config(&[ConfigWindow {
            title: "W".into(),
            groups: vec![],
        }])
    }

    pub fn new_group(ws: &mut Workspace, url: &str) -> Opened {
        ws.new_group("W", &url::Url::parse(url).unwrap(), "default")
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{new_group, one_window};
    use super::*;
    use flotilla_core::model::{ConfigWindow, Opened};
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
    fn an_opened_document_brings_its_window_forward() {
        let t = || Some("t-3".to_string());
        // A popped-out tab the open raised: its window, wherever the click came from.
        assert_eq!(forward(false, t()), Some(Forward::Detached("t-3".into())));
        assert_eq!(forward(true, t()), Some(Forward::Detached("t-3".into())));
        // Opened or raised in the main window from a pop-out: the main window.
        assert_eq!(forward(true, None), Some(Forward::Main));
        // From the main window, which is already in front.
        assert_eq!(forward(false, None), None);
    }

    #[test]
    fn a_failed_tab_is_not_retried_until_cleared() {
        let (ws, _, _, b) = ws();
        let p = plan(&ws, &HashSet::new(), &set(&[&b]));
        assert_eq!(p.create, None);
        assert_eq!(p.show, Some(b));
    }

    #[test]
    fn unloading_the_active_tab_picks_the_nearest_live_tab_above() {
        let (mut ws, h, a, b) = ws();
        // b active; h and b live, a cold: skip a, land on h.
        assert_eq!(after_unload(&ws, &set(&[&h, &b]), &b), Some(h.clone()));
        // a live too: the nearest above wins.
        assert_eq!(after_unload(&ws, &set(&[&h, &a, &b]), &b), Some(a.clone()));
        // Unloading the head: nothing above, so the nearest live below.
        ws.activate(&h);
        assert_eq!(after_unload(&ws, &set(&[&h, &b]), &h), Some(b.clone()));
    }

    #[test]
    fn unloading_the_last_live_tab_wakes_nothing() {
        let (ws, _, _, b) = ws();
        assert_eq!(after_unload(&ws, &set(&[&b]), &b), None);
    }

    #[test]
    fn unload_skips_hidden_and_popped_out_tabs() {
        let (mut ws, h, a, b) = ws();
        let Opened::Added(h2) = new_group(&mut ws, "https://drive.google.com/drive/folders/f2")
        else {
            panic!()
        };
        // Group 1 collapsed: its members are not visible, so only its head can be picked.
        ws.set_collapsed(&h, true);
        ws.set_detached(&h, true);
        assert_eq!(after_unload(&ws, &set(&[&h, &a, &b, &h2]), &h2), None);
    }

    #[test]
    fn title_is_adopted_only_while_it_is_the_url() {
        // A new tab's stored title is its URL: the page's first real title replaces it.
        assert!(adopt_title(DOC_A, DOC_A, "Budget"));
        // A loading page reports nothing worth keeping.
        assert!(!adopt_title(DOC_A, DOC_A, ""));
        assert!(!adopt_title(DOC_A, DOC_A, "  "));
        // Once named, the page never renames it.
        assert!(!adopt_title("Budget", DOC_A, "Budget 2026"));
    }

    #[test]
    fn a_renamed_tab_keeps_its_name_after_navigation() {
        let (mut ws, h, _, b) = ws();
        let title = |ws: &Workspace, id: &TabId| ws.find(id).unwrap().2.title.clone();
        assert_eq!(title(&ws, &b), DOC_B);
        // The page loads: the URL title gives way to the page's.
        assert!(retitled(&mut ws, &b, "Budget"));
        // The user renames it; navigating inside it reports other titles, which are ignored.
        ws.set_title(&b, "Club budget");
        assert!(!retitled(&mut ws, &b, "Budget – Sheet 2"));
        assert_eq!(title(&ws, &b), "Club budget");
        // A head named by its page keeps that name while it browses subfolders.
        assert!(retitled(&mut ws, &h, "Auction 2026"));
        assert!(!retitled(&mut ws, &h, "Subfolder"));
        assert_eq!(title(&ws, &h), "Auction 2026");
    }

    #[test]
    fn labels_round_trip() {
        assert_eq!(label("t-3"), "main:t-3");
        assert_eq!(tab_id("main:t-3").as_deref(), Some("t-3"));
        assert_eq!(tab_id("main"), None);
        assert_eq!(tab_id("other:t-3"), None);
    }

    #[test]
    fn a_restored_workspace_creates_only_the_active_tab() {
        let (ws, _, _, b) = ws();
        let p = plan(&ws, &HashSet::new(), &HashSet::new());
        assert_eq!(
            p,
            ViewPlan {
                close: vec![],
                create: Some(Target {
                    id: b.clone(),
                    home_url: DOC_B.into(),
                    store: "default".into(),
                }),
                show: Some(b),
                hide: vec![],
            }
        );
    }

    #[test]
    fn selecting_a_created_tab_hides_the_others_without_creating() {
        let (mut ws, h, a, b) = ws();
        ws.activate(&a);
        let p = plan(&ws, &set(&[&h, &a, &b]), &HashSet::new());
        assert_eq!(p.create, None);
        assert_eq!(p.show, Some(a));
        let mut hide = vec![h, b];
        hide.sort();
        assert_eq!(p.hide, hide);
        assert!(p.close.is_empty());
    }

    #[test]
    fn closed_and_popped_out_tabs_lose_their_webview() {
        let (mut ws, h, a, b) = ws();
        ws.close_tab(&b);
        ws.set_detached(&a, true);
        let p = plan(&ws, &set(&[&h, &a, &b]), &HashSet::new());
        let mut close = vec![a, b];
        close.sort();
        assert_eq!(p.close, close);
        // Closing b activated a, which is now popped out: nothing here to show.
        assert_eq!(p.show, None);
        assert_eq!(p.hide, vec![h]);
    }

    #[test]
    fn the_main_window_shows_the_first_config_window_only() {
        let window = |t: &str| ConfigWindow {
            title: t.into(),
            groups: vec![],
        };
        let mut ws = Workspace::from_config(&[window("A"), window("B")]);
        let Some(Opened::Added(b)) = ws.new_group(
            "B",
            &url("https://drive.google.com/drive/folders/f2"),
            "default",
        ) else {
            panic!()
        };
        let p = plan(&ws, &set(&[&b]), &HashSet::new());
        assert_eq!(p.show, None);
        assert_eq!(p.close, vec![b]);
    }

    #[test]
    fn nothing_open_shows_nothing() {
        let (mut ws, h, a, b) = ws();
        ws.close_group(&h);
        let p = plan(&ws, &set(&[&h, &a, &b]), &HashSet::new());
        assert_eq!(p.show, None);
        assert_eq!(p.create, None);
        assert_eq!(p.close.len(), 3);
    }
}
