//! Each window's content webviews: one per shown tab, labelled as a child of the window that hosts
//! it (`shell_core::label::child_label(window, tab_id)`), created lazily and brought in line with
//! the workspace by [`sync`]. A window's label comes from the `WindowSet`, which maps it to the
//! config window (and so the workspace window) by title.

use crate::state::AppState;
use flotilla_core::identity::{is_tab_id, page_title};
use flotilla_core::model::{Opened, Tab, TabId, WindowWs, Workspace};
use flotilla_core::nav::{self, Decision};
use shell_core::compositing::HoleRect;
use shell_core::label::{child_label, window_of};
use shell_core::tabs::pick_live_neighbour;
use shell_core::web::content::{
    close_content_webview, create_content_webview, ContentHooks, ContentSpec, LinkOrigin,
};
use shell_core::windows::WindowSet;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, Url, Window};

/// The loading bar's colour (sRGB, 0–1): the chrome's neutral blue.
const BAR_RGBA: (f64, f64, f64, f64) = (0.039, 0.518, 1.0, 1.0);

/// Narrows Google Drive's left navigation to an icon rail; a no-op on every other page.
const DRIVE_RAIL_JS: &str = include_str!("../../src/inject/drive-rail.js");

/// Tab `id`'s content webview label in the window labelled `window`.
pub fn label(window: &str, id: &str) -> String {
    child_label(window, id)
}

/// The window label and tab of a content webview label; `None` for any other label (a window's
/// own chrome, whose label may itself hold a `:`).
pub fn tab_of(label: &str) -> Option<(&str, TabId)> {
    let window = window_of(label)?;
    let id = label.strip_prefix(window)?.strip_prefix(':')?;
    is_tab_id(id).then(|| (window, id.to_string()))
}

/// A tab whose webview is to be created.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub id: TabId,
    pub home_url: String,
    /// The group's login store: the webview's session seed.
    pub store: String,
}

/// What [`sync`] does to bring one window's webviews in line with the workspace.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ViewPlan {
    /// Created webviews whose tab is no longer open here (closed, popped out, or moved to another
    /// window).
    pub close: Vec<TabId>,
    /// The active tab, when it has no webview yet.
    pub create: Option<Target>,
    /// The window's other `warm` tabs (loaded when the session was saved) without a webview yet,
    /// in row order: created hidden. No other tab is ever created.
    pub warm: Vec<Target>,
    pub show: Option<TabId>,
    /// Every other remaining webview.
    pub hide: Vec<TabId>,
    /// The active tab's label is still held by its previous webview, which is closing (the same
    /// document closed and opened again before the close landed): sync again on a later turn
    /// rather than create a second webview under that label.
    pub wait: bool,
}

/// The workspace window titled `title`.
pub fn window<'a>(ws: &'a Workspace, title: &str) -> Option<&'a WindowWs> {
    ws.windows.iter().find(|w| w.title == title.trim())
}

/// The active tab of the workspace window titled `title`.
pub fn active_in(ws: &Workspace, title: &str) -> Option<TabId> {
    window(ws, title).and_then(|w| w.active.clone())
}

/// The title of the workspace window holding tab `id`.
pub fn window_title_of(ws: &Workspace, id: &TabId) -> Option<String> {
    ws.find(id).map(|(wi, _, _)| ws.windows[wi].title.clone())
}

/// Every tab with a webview, across windows: `created`'s sets (tab ids are unique across windows).
pub fn live_in(created: &HashMap<String, HashSet<TabId>>) -> HashSet<TabId> {
    created.values().flatten().cloned().collect()
}

/// [`live_in`] of the app's state.
pub fn live(app: &AppHandle) -> HashSet<TabId> {
    live_in(&app.state::<AppState>().created.lock().unwrap())
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

/// The plan for the workspace window titled `title`, given the tabs whose webview exists in its
/// OS window (`created`) and those whose label is still held by a webview on its way out
/// (`closing`). `failed` tabs are not created again (see `AppState::failed`); `warm` ones are
/// created though not active (see `AppState::warm`).
pub fn plan(
    ws: &Workspace,
    title: &str,
    created: &HashSet<TabId>,
    failed: &HashSet<TabId>,
    closing: &HashSet<TabId>,
    warm: &HashSet<TabId>,
) -> ViewPlan {
    let here = |id: &TabId| {
        window(ws, title).is_some_and(|w| {
            w.groups
                .iter()
                .flat_map(|g| std::iter::once(&g.head).chain(&g.members))
                .any(|t| &t.id == id && !t.detached)
        })
    };
    let mut close: Vec<TabId> = created.iter().filter(|id| !here(id)).cloned().collect();
    close.sort();
    let show = active_in(ws, title).filter(here);
    let wanted = show
        .as_ref()
        .filter(|id| !created.contains(*id) && !failed.contains(*id));
    let wait = wanted.is_some_and(|id| closing.contains(id));
    let target = |(_, g, t): (usize, &flotilla_core::model::Group, &Tab)| Target {
        id: t.id.clone(),
        home_url: t.home_url.clone(),
        store: g.store.clone(),
    };
    let create = wanted
        .filter(|_| !wait)
        .and_then(|id| ws.find(id))
        .map(target);
    let warm: Vec<Target> = window(ws, title)
        .into_iter()
        .flat_map(|w| &w.groups)
        .flat_map(|g| std::iter::once(&g.head).chain(&g.members))
        .map(|t| &t.id)
        .filter(|id| {
            warm.contains(*id)
                && here(id)
                && Some(*id) != show.as_ref()
                && !created.contains(*id)
                && !failed.contains(*id)
                && !closing.contains(*id)
        })
        .filter_map(|id| ws.find(id).map(target))
        .collect();
    // While waiting, the webview under the active tab's label is the closing one: not shown.
    let show = show.filter(|_| !wait);
    let mut hide: Vec<TabId> = created
        .iter()
        .filter(|id| here(id) && Some(*id) != show.as_ref())
        .cloned()
        .collect();
    hide.sort();
    ViewPlan {
        close,
        create,
        warm,
        show,
        hide,
        wait,
    }
}

/// Whether a page's reported title replaces the tab's stored one: only while the stored title is
/// still the tab's URL (what a new group or document is written with), alone or suffixed unique in
/// its window ("<url> 2"), so a name is written once and never follows navigation. A page
/// reports an empty title while it loads.
pub fn adopt_title(stored: &str, home_url: &str, reported: &str) -> bool {
    let named_by_url = stored == home_url
        || stored
            .strip_prefix(home_url)
            .and_then(|rest| rest.strip_prefix(' '))
            .is_some_and(|n| n.parse::<u32>().is_ok());
    named_by_url && !reported.trim().is_empty() && reported != stored
}

/// Gives tab `id` the page's reported title when [`adopt_title`] says so; whether it did.
pub fn retitled(ws: &mut Workspace, id: &TabId, reported: &str) -> bool {
    let name = page_title(reported.trim());
    let adopt = ws
        .find(id)
        .is_some_and(|(_, _, t)| adopt_title(&t.title, &t.home_url, name));
    if adopt {
        ws.set_title(id, name);
    }
    adopt
}

/// The open windows the `WindowSet` holds, as (label, title).
pub fn open_windows(app: &AppHandle) -> Vec<(String, String)> {
    app.state::<WindowSet>()
        .entries()
        .into_iter()
        .filter(|e| e.open)
        .map(|e| (e.id, e.title))
        .collect()
}

/// The title of the window labelled `label`, open or closed.
pub fn title_of_label(app: &AppHandle, label: &str) -> Option<String> {
    app.state::<WindowSet>()
        .entries()
        .into_iter()
        .find(|e| e.id == label)
        .map(|e| e.title)
}

/// After any workspace change: bring every window's webviews, the popped-out windows and the
/// sidebars in line, and schedule a save. Main thread only. The windows go first, so a tab just
/// popped out has released its webview before its pop-out is opened.
pub fn changed(app: &AppHandle) {
    sync(app);
    crate::detach::reconcile(app);
    crate::persist::mark_dirty(app);
}

/// Brings every open window's content webviews in line with the workspace ([`plan`]) and pushes
/// the sidebar DTOs. Main thread only. `ws` is locked only to compute each plan.
pub fn sync(app: &AppHandle) {
    for (label, title) in open_windows(app) {
        if sync_window(app, &label, &title) {
            retry_window(app, label, title, 1);
        }
    }
    push_dto(app);
}

/// How many main-loop turns a window waits for a closing webview to release its tab's label
/// before giving up; the next change syncs it again.
const WAIT_TURNS: u32 = 20;

/// Syncs window `label` again on a later turn, until its active tab's label is free
/// ([`ViewPlan::wait`]) or [`WAIT_TURNS`] have passed.
fn retry_window(app: &AppHandle, label: String, title: String, turn: u32) {
    if turn > WAIT_TURNS {
        return;
    }
    let later = app.clone();
    crate::defer(app, move || {
        if sync_window(&later, &label, &title) {
            retry_window(&later, label, title, turn + 1);
        } else {
            push_dto(&later);
        }
    });
}

/// [`sync`] for the window labelled `label`; whether it must run again once a closing webview has
/// gone.
fn sync_window(app: &AppHandle, label: &str, title: &str) -> bool {
    let state = app.state::<AppState>();
    let Some(window) = app.get_window(label) else {
        return false;
    };
    let alive: HashSet<TabId> = app
        .webviews()
        .keys()
        .filter_map(|l| tab_of(l).filter(|(w, _)| *w == label).map(|(_, id)| id))
        .collect();
    let plan = {
        let ws = state.ws.lock().unwrap();
        let created_all = state.created.lock().unwrap();
        let created = created_all.get(label).cloned().unwrap_or_default();
        let closing: HashSet<TabId> = alive.difference(&created).cloned().collect();
        let mut warm = state.warm.lock().unwrap();
        warm.retain(|id| ws.find(id).is_some());
        let mut failed = state.failed.lock().unwrap();
        failed.retain(|id| ws.find(id).is_some());
        plan(&ws, title, &created, &failed, &closing, &warm)
    };
    let forget = |id: &TabId| {
        if let Some(c) = state.created.lock().unwrap().get_mut(label) {
            c.remove(id);
        }
    };
    for id in &plan.close {
        if let Some(wv) = own_webview(&window, id) {
            close_content_webview(&wv);
        }
        forget(id);
    }
    for t in plan.create.iter().chain(&plan.warm) {
        state.warm.lock().unwrap().remove(&t.id);
        let hole = hole_of(app, label);
        match create(&window, t, hole) {
            Ok(()) => {
                state
                    .created
                    .lock()
                    .unwrap()
                    .entry(label.to_string())
                    .or_default()
                    .insert(t.id.clone());
            }
            Err(e) => {
                state.failed.lock().unwrap().insert(t.id.clone());
                notice(app, format!("Couldn't open {}: {e}", t.home_url));
            }
        }
    }
    for id in plan.hide.iter().chain(plan.warm.iter().map(|t| &t.id)) {
        if let Some(wv) = own_webview(&window, id) {
            let _ = wv.hide();
        }
    }
    if let Some(wv) = plan.show.as_ref().and_then(|id| own_webview(&window, id)) {
        let _ = wv.show();
    }
    plan.wait
}

/// Window `label`'s content hole: its chrome's last report, else a first-run guess from its size.
fn hole_of(app: &AppHandle, label: &str) -> HoleRect {
    if let Some(h) = app.state::<AppState>().hole.lock().unwrap().get(label) {
        return *h;
    }
    let size = app
        .get_window(label)
        .and_then(|w| {
            let scale = w.scale_factor().ok()?;
            w.inner_size().ok().map(|s| s.to_logical::<f64>(scale))
        })
        .unwrap_or(tauri::LogicalSize::new(1400.0, 900.0));
    shell_core::compositing::initial_hole(size.width, size.height)
}

/// Sends each open window's sidebar DTO to its chrome and rebuilds the menu bar, whose Group menu
/// follows the workspace.
pub fn push_dto(app: &AppHandle) {
    let state = app.state::<AppState>();
    for (label, title) in open_windows(app) {
        let dto = {
            let ws = state.ws.lock().unwrap();
            let created = state.created.lock().unwrap();
            let none = HashSet::new();
            crate::sidebar::window_dto(&ws, &title, created.get(&label).unwrap_or(&none))
        };
        let _ = app.emit_to(label.as_str(), "workspace", dto);
    }
    crate::menu::refresh(app);
}

/// Shows `msg` in every open window's error bar, or holds it until a chrome mounts.
pub fn notice(app: &AppHandle, msg: String) {
    let now = app.state::<AppState>().notice.lock().unwrap().raise(msg);
    if let Some(msg) = now {
        for (label, _) in open_windows(app) {
            let _ = app.emit_to(label.as_str(), "notice", msg.clone());
        }
    }
}

/// Tab `id`'s webview, only if it lives in `window`.
pub(crate) fn own_webview(window: &Window, id: &str) -> Option<tauri::Webview> {
    window
        .get_webview(&label(window.label(), id))
        .filter(|wv| wv.window().label() == window.label())
}

/// The webview showing tab `id`, in whichever window hosts it.
pub(crate) fn webview_of(app: &AppHandle, id: &TabId) -> Option<tauri::Webview> {
    app.webviews()
        .into_iter()
        .find(|(l, _)| tab_of(l).is_some_and(|(_, t)| t == *id))
        .map(|(_, wv)| wv)
}

/// The page tab `id` shows now; `None` when it has no webview. A webview call: hold no lock.
pub(crate) fn current_url(app: &AppHandle, id: &TabId) -> Option<Url> {
    webview_of(app, id)?.url().ok()
}

/// The page the mouse's back/forward buttons act on: the focused popped-out window's tab, else the
/// focused window's active tab.
pub(crate) fn focused_webview(app: &AppHandle) -> Option<tauri::Webview> {
    let window = app.get_focused_window()?;
    if let Some(id) = crate::detach::tab_of(window.label()) {
        return own_webview(&window, &id);
    }
    let title = title_of_label(app, window.label())?;
    let active = active_in(&app.state::<AppState>().ws.lock().unwrap(), &title)?;
    own_webview(&window, &active)
}

/// Creates tab `t`'s content webview in `window` (its workspace window, or its popped-out one).
pub(crate) fn create(window: &Window, t: &Target, hole: HoleRect) -> Result<(), String> {
    let url = Url::parse(&t.home_url).map_err(|e| e.to_string())?;
    let label = label(window.label(), &t.id);
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
            crate::defer(&app.clone(), move || retitle(&app, &label, &title));
        })),
        on_download: None,
    }
}

/// shell-core's `intercept`: runs inside WebKit's policy callback, so it only decides and writes the
/// decision through to the config (under the gate, so no reload lands between the two). On `Open`
/// the workspace already holds the new or raised tab; showing it is queued for a later turn of the
/// main loop ([`crate::defer`]), after the policy decision returns.
fn intercept(app: &AppHandle, label: &str, url: &Url, _: LinkOrigin) -> bool {
    let Some((_, id)) = tab_of(label) else {
        return false;
    };
    let (decision, front) = crate::write::through(app, |ws| {
        let from = ws
            .find(&id)
            .map(|(wi, _, t)| (t.detached, ws.windows[wi].title.clone()));
        let mut decision = Decision::PassThrough;
        let raised = crate::detach::apply(ws, |ws| {
            decision = nav::decide(ws, &id, url);
        });
        let shown = match &decision {
            Decision::Open(Opened::Added(t) | Opened::Raised(t)) => Some(t.clone()),
            Decision::PassThrough => None,
        };
        let front = from.and_then(|(from_popout, from)| {
            front_for(ws, from_popout, &from, shown.as_ref(), raised)
        });
        (decision, front)
    });
    match decision {
        Decision::PassThrough => false,
        Decision::Open(_) => {
            let deferred = app.clone();
            crate::defer(app, move || {
                bring(&deferred, front.as_ref());
                changed(&deferred)
            });
            true
        }
    }
}

/// The window to bring forward once an operation started in window `from` (from a pop-out of
/// it when `from_popout`) has shown tab `shown`, or raised the pop-out of tab `raised`:
/// [`forward`] to `shown`'s window.
pub fn front_for(
    ws: &Workspace,
    from_popout: bool,
    from: &str,
    shown: Option<&TabId>,
    raised: Option<TabId>,
) -> Option<Forward> {
    if raised.is_some() {
        return forward(from_popout, from, from, raised);
    }
    let to = window_title_of(ws, shown?)?;
    forward(from_popout, from, &to, None)
}

/// Brings `front` forward: a pop-out, or a workspace window (reopened if closed).
pub fn bring(app: &AppHandle, front: Option<&Forward>) {
    match front {
        Some(Forward::Detached(r)) => crate::detach::raise(app, r),
        Some(Forward::Window(title)) => bring_forward(app, title),
        None => {}
    }
}

/// The window an opened (or raised) document is shown in, when it is not already in front.
#[derive(Debug, PartialEq, Eq)]
pub enum Forward {
    /// The popped-out tab's own window.
    Detached(TabId),
    /// The workspace window with this title.
    Window(String),
}

/// Which window to bring forward after a link in a tab of window `from` opened a document in
/// window `to`: the popped-out tab the open `raised`, else `to` when the link was clicked in a
/// pop-out or `to` is another window, else none (`to` is already in front).
pub fn forward(from_popout: bool, from: &str, to: &str, raised: Option<TabId>) -> Option<Forward> {
    match raised {
        Some(r) => Some(Forward::Detached(r)),
        None => (from_popout || from != to).then(|| Forward::Window(to.to_string())),
    }
}

/// Focuses the window titled `title`, reopening it if it is closed.
pub fn bring_forward(app: &AppHandle, title: &str) {
    let set = app.state::<WindowSet>();
    if let Some(label) = set.label_for_title(title) {
        set.open_or_focus(app, &label);
    }
}

fn retitle(app: &AppHandle, label: &str, title: &str) {
    let Some((_, id)) = tab_of(label) else {
        return;
    };
    if crate::write::through(app, |ws| retitled(ws, &id, title)) {
        push_dto(app);
    } else {
        // A new page title is a new page: Set Home to Current Page may have changed.
        crate::menu::refresh(app);
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
    use flotilla_core::model::ConfigWindow;
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    const DOC_A: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit";
    const DOC_B: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt02/edit";
    const TAB: &str = "t-0123456789abcdef";

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

    fn none() -> HashSet<TabId> {
        HashSet::new()
    }

    /// Windows A and B, each with a folder group and a document; each window's document active.
    fn two_windows() -> (Workspace, TabId, TabId) {
        let window = |t: &str| ConfigWindow {
            title: t.into(),
            groups: vec![],
        };
        let mut ws = Workspace::from_config(&[window("A"), window("B")]);
        let mut add = |w: &str, folder: &str, doc: &str| {
            let Some(Opened::Added(h)) = ws.new_group(w, &url(folder), "default") else {
                panic!()
            };
            let Some(Opened::Added(d)) = ws.open_from(&h, &url(doc)) else {
                panic!()
            };
            d
        };
        let a = add("A", "https://drive.google.com/drive/folders/f1", DOC_A);
        let b = add("B", "https://drive.google.com/drive/folders/f2", DOC_B);
        (ws, a, b)
    }

    #[test]
    fn an_opened_document_brings_its_window_forward() {
        let t = || Some(TAB.to_string());
        // A popped-out tab the open raised: its window, wherever the click came from.
        assert_eq!(
            forward(false, "A", "A", t()),
            Some(Forward::Detached(TAB.into()))
        );
        assert_eq!(
            forward(true, "A", "A", t()),
            Some(Forward::Detached(TAB.into()))
        );
        // Opened or raised in a window from a pop-out: that window.
        assert_eq!(
            forward(true, "A", "A", None),
            Some(Forward::Window("A".into()))
        );
        // Raised in another window: that window.
        assert_eq!(
            forward(false, "A", "B", None),
            Some(Forward::Window("B".into()))
        );
        // In the window the link was clicked in, which is already in front.
        assert_eq!(forward(false, "A", "A", None), None);
    }

    #[test]
    fn open_once_in_another_window_brings_that_window_forward() {
        let (mut ws, a, b) = two_windows();
        // New Group in A on B's document: B's tab is raised there, and B comes forward.
        let shown = match ws.new_group("A", &url(DOC_B), "default").unwrap() {
            Opened::Raised(id) => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(shown, b);
        assert_eq!(
            front_for(&ws, false, "A", Some(&shown), None),
            Some(Forward::Window("B".into()))
        );
        // Reopening, from A, a closed group whose folder is now open in B raises it in B.
        let folder_a = url("https://drive.google.com/drive/folders/f1");
        let head_a = ws.windows[0].groups[0].head.id.clone();
        ws.close_group(&head_a);
        let Some(Opened::Added(in_b)) = ws.new_group("B", &folder_a, "default") else {
            panic!()
        };
        let reopened = ws.reopen_recent("A", 0);
        assert_eq!(reopened, Some(in_b.clone()));
        assert_eq!(
            front_for(&ws, false, "A", reopened.as_ref(), None),
            Some(Forward::Window("B".into()))
        );
        // Shown in the window it was asked from: nothing to bring forward.
        assert_eq!(front_for(&ws, false, "B", Some(&in_b), None), None);
        // A popped-out tab raised: its pop-out.
        assert_eq!(
            front_for(&ws, false, "A", None, Some(a.clone())),
            Some(Forward::Detached(a))
        );
    }

    #[test]
    fn a_failed_tab_is_not_retried_until_cleared() {
        let (ws, _, _, b) = ws();
        let p = plan(&ws, "W", &none(), &set(&[&b]), &none(), &none());
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
        // A URL name suffixed unique in its window is still the URL.
        assert!(adopt_title(&format!("{DOC_A} 2"), DOC_A, "Budget"));
        assert!(!adopt_title(&format!("{DOC_A} two"), DOC_A, "Budget"));
    }

    #[test]
    fn a_renamed_tab_keeps_its_name_after_navigation() {
        let (mut ws, h, _, b) = ws();
        let title = |ws: &Workspace, id: &TabId| ws.find(id).unwrap().2.title.clone();
        assert_eq!(title(&ws, &b), DOC_B);
        // The page loads: the URL title gives way to the page's, without Google's app name.
        assert!(retitled(&mut ws, &b, "Budget - Google Docs"));
        assert_eq!(title(&ws, &b), "Budget");
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
        assert_eq!(label("club", TAB), format!("club:{TAB}"));
        assert_eq!(tab_of(&label("club", TAB)), Some(("club", TAB.to_string())));
        // A window label may hold a `:`; a tab never does.
        assert_eq!(tab_of(&label("a:b", TAB)), Some(("a:b", TAB.to_string())));
        // A chrome whose window label holds a `:` is not a tab.
        assert_eq!(tab_of("club:-interim"), None);
        assert_eq!(tab_of("club"), None);
    }

    #[test]
    fn a_restored_workspace_creates_only_the_active_tab() {
        let (ws, _, _, b) = ws();
        let p = plan(&ws, "W", &none(), &none(), &none(), &none());
        assert_eq!(
            p,
            ViewPlan {
                close: vec![],
                create: Some(Target {
                    id: b.clone(),
                    home_url: DOC_B.into(),
                    store: "default".into(),
                }),
                warm: vec![],
                show: Some(b),
                hide: vec![],
                wait: false,
            }
        );
    }

    #[test]
    fn a_restored_workspace_also_creates_its_loaded_tabs_hidden() {
        let (ws, h, a, b) = ws();
        // h and b were loaded; b is active, so only h is created warm.
        let warm = set(&[&h, &b]);
        let p = plan(&ws, "W", &none(), &none(), &none(), &warm);
        assert_eq!(p.create.map(|t| t.id), Some(b.clone()));
        assert_eq!(p.warm.iter().map(|t| &t.id).collect::<Vec<_>>(), [&h]);
        assert_eq!(p.show, Some(b.clone()));
        // Once created, a warm tab is not created again; a failed one is not retried.
        assert!(plan(&ws, "W", &set(&[&h]), &none(), &none(), &warm)
            .warm
            .is_empty());
        assert!(plan(&ws, "W", &none(), &set(&[&h]), &none(), &warm)
            .warm
            .is_empty());
        // A warm tab in another window, or popped out, is not created here.
        let mut ws = ws;
        ws.set_detached(&h, true);
        assert!(plan(&ws, "W", &none(), &none(), &none(), &set(&[&h, &a]))
            .warm
            .iter()
            .all(|t| t.id == a));
    }

    #[test]
    fn each_window_creates_only_its_active_tab() {
        let (ws, a, b) = two_windows();
        let pa = plan(&ws, "A", &none(), &none(), &none(), &none());
        let pb = plan(&ws, "B", &none(), &none(), &none(), &none());
        assert_eq!(pa.create.map(|t| t.id), Some(a.clone()));
        assert_eq!(pb.create.map(|t| t.id), Some(b.clone()));
        // A webview of B's tab in A's window (it moved there, say) is closed in A.
        let p = plan(&ws, "A", &set(&[&a, &b]), &none(), &none(), &none());
        assert_eq!(p.close, vec![b]);
        assert_eq!(p.show, Some(a));
    }

    #[test]
    fn a_tab_whose_label_is_still_closing_waits_to_be_recreated() {
        let (ws, _, _, b) = ws();
        let p = plan(&ws, "W", &none(), &none(), &set(&[&b]), &none());
        assert!(p.wait);
        assert_eq!(p.create, None);
        // The webview under its label is the closing one: it is not shown.
        assert_eq!(p.show, None);
        // Once the old webview has gone, it is created.
        let p = plan(&ws, "W", &none(), &none(), &none(), &none());
        assert!(!p.wait);
        assert_eq!(p.create.map(|t| t.id), Some(b));
    }

    #[test]
    fn a_doc_opened_from_a_popout_joins_its_origin_window() {
        let (mut ws, a, _) = two_windows();
        // A's document is popped out; a link in it opens another document.
        ws.set_detached(&a, true);
        let doc = url("https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt09/edit");
        let Some(Opened::Added(new)) = ws.open_from(&a, &doc) else {
            panic!()
        };
        assert_eq!(window_title_of(&ws, &new).as_deref(), Some("A"));
        assert_eq!(active_in(&ws, "A"), Some(new.clone()));
        // Window A comes forward to show it.
        assert_eq!(
            forward(true, "A", "A", None),
            Some(Forward::Window("A".into()))
        );
        let p = plan(&ws, "A", &none(), &none(), &none(), &none());
        assert_eq!(p.create.map(|t| t.id), Some(new));
    }

    #[test]
    fn selecting_a_created_tab_hides_the_others_without_creating() {
        let (mut ws, h, a, b) = ws();
        ws.activate(&a);
        let p = plan(&ws, "W", &set(&[&h, &a, &b]), &none(), &none(), &none());
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
        let p = plan(&ws, "W", &set(&[&h, &a, &b]), &none(), &none(), &none());
        let mut close = vec![a, b];
        close.sort();
        assert_eq!(p.close, close);
        // Closing b activated a, which is now popped out: nothing here to show.
        assert_eq!(p.show, None);
        assert_eq!(p.hide, vec![h]);
    }

    #[test]
    fn nothing_open_shows_nothing() {
        let (mut ws, h, a, b) = ws();
        ws.close_group(&h);
        let p = plan(&ws, "W", &set(&[&h, &a, &b]), &none(), &none(), &none());
        assert_eq!(p.show, None);
        assert_eq!(p.create, None);
        assert_eq!(p.close.len(), 3);
    }
}
