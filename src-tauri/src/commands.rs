//! The commands the chrome sidebar invokes. Sync commands, so they run on the main thread and may
//! touch webviews directly; each locks `ws` only to change or read it. A chrome acts on the
//! workspace window its own window shows.

use crate::state::AppState;
use crate::tabs;
use flotilla_core::model::TabId;
use serde::Deserialize;
use shell_core::compositing::{layout_webviews, HoleRect};
use shell_core::windows::WindowSet;
use tauri::{AppHandle, Manager, State, Url, Webview};

/// Content webviews load remote origins, which Tauri already refuses app commands to; this keeps
/// the commands to a window's own chrome page regardless. The title of the window it chromes.
fn require_chrome(webview: &Webview) -> Result<String, String> {
    let label = webview.label();
    if label != webview.window().label() {
        return Err("forbidden: not the chrome".into());
    }
    tabs::title_of_label(webview.app_handle(), label)
        .ok_or_else(|| "forbidden: not the chrome".into())
}

/// The URL a typed or pasted new-group address names. Only web pages can head a group.
pub fn parse_group_url(input: &str) -> Result<Url, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("Paste a folder or page link to open it as a group.".into());
    }
    match Url::parse(s) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => Ok(u),
        _ => Err(format!("Not a web link: {s}")),
    }
}

/// The calling window's sidebar DTO, for the chrome's first paint (later ones arrive as
/// `workspace` events).
#[tauri::command]
pub fn get_workspace(
    webview: Webview,
    state: State<AppState>,
) -> Result<serde_json::Value, String> {
    let title = require_chrome(&webview)?;
    let ws = state.ws.lock().unwrap();
    let created = state.created.lock().unwrap();
    let none = Default::default();
    Ok(crate::sidebar::sidebar_dto(
        tabs::window(&ws, &title),
        created.get(webview.label()).unwrap_or(&none),
    ))
}

/// The chrome has mounted: what the error bar shows for the notices raised before it could listen
/// ([`crate::state::Notices`]).
#[tauri::command]
pub fn startup_notice(state: State<AppState>) -> Option<String> {
    state.notice.lock().unwrap().mount()
}

/// Selecting a popped-out tab's row raises its window instead ([`mutate`]).
#[tauri::command]
pub fn select_tab(id: TabId, webview: Webview, state: State<AppState>) -> Result<(), String> {
    require_chrome(&webview)?;
    if state.ws.lock().unwrap().find(&id).is_none() {
        return Err("unknown tab".into());
    }
    // An explicit selection is a fresh attempt at a tab whose webview failed to build.
    state.failed.lock().unwrap().remove(&id);
    mutate(webview.app_handle(), |ws| ws.activate(&id));
    Ok(())
}

/// The row's pop-out control: tab `id` moves into its own window.
#[tauri::command]
pub fn pop_out_tab(id: TabId, webview: Webview) -> Result<(), String> {
    require_chrome(&webview)?;
    pop_out(webview.app_handle(), &id)
}

/// The label of the OS window showing the workspace window that holds tab `id`.
fn window_label_of(app: &AppHandle, id: &TabId) -> Option<String> {
    let title = tabs::window_title_of(&app.state::<AppState>().ws.lock().unwrap(), id)?;
    app.state::<WindowSet>().label_for_title(&title)
}

/// Pops tab `id` out (the row control, ⌘⇧O).
pub fn pop_out(app: &AppHandle, id: &TabId) -> Result<(), String> {
    let label = window_label_of(app, id).unwrap_or_default();
    crate::write::through(app, |ws| {
        let state = app.state::<AppState>();
        let created = state.created.lock().unwrap();
        let none = Default::default();
        crate::detach::pop_out(ws, created.get(&label).unwrap_or(&none), id)
    })?;
    tabs::changed(app);
    Ok(())
}

/// A popped-out row's pop-in control: closes the tab's window, which returns it to its group.
#[tauri::command]
pub fn pop_in_tab(id: TabId, webview: Webview) -> Result<(), String> {
    require_chrome(&webview)?;
    crate::detach::pop_in(webview.app_handle(), &id);
    Ok(())
}

/// The row dot's Unload: closes tab `id`'s webview and keeps the tab. If it was active, the nearest
/// live visible tab takes over ([`tabs::after_unload`]), else nothing is shown; no cold tab wakes.
#[tauri::command]
pub fn unload_tab(id: TabId, webview: Webview, state: State<AppState>) -> Result<(), String> {
    require_chrome(&webview)?;
    if state.ws.lock().unwrap().find(&id).is_none() {
        return Err("unknown tab".into());
    }
    let window = webview.window();
    if let Some(wv) = tabs::own_webview(&window, &id) {
        shell_core::web::content::close_content_webview(&wv);
    }
    let active_moved = {
        let mut ws = state.ws.lock().unwrap();
        let mut created = state.created.lock().unwrap();
        let created = created.entry(window.label().to_string()).or_default();
        let was_active = ws.is_active(&id);
        let next = tabs::after_unload(&ws, created, &id);
        created.remove(&id);
        if was_active {
            ws.deactivate(&id);
            if let Some(n) = next {
                ws.activate(&n);
            }
        }
        was_active
    };
    if active_moved {
        tabs::changed(webview.app_handle());
    } else {
        tabs::sync(webview.app_handle());
    }
    Ok(())
}

#[tauri::command]
pub fn toggle_group(head: TabId, collapsed: bool, webview: Webview) -> Result<(), String> {
    require_chrome(&webview)?;
    crate::write::through(webview.app_handle(), |ws| {
        ws.set_collapsed(&head, collapsed)
    });
    tabs::push_dto(webview.app_handle());
    Ok(())
}

/// Opens `url` as a new group in the calling window (or raises the tab already showing it, in
/// whichever window). The error is for the sidebar's error bar.
#[tauri::command]
pub fn new_group(url: String, webview: Webview) -> Result<(), String> {
    let window = require_chrome(&webview)?;
    let url = parse_group_url(&url)?;
    mutate(webview.app_handle(), |ws| {
        ws.new_group(&window, &url, crate::config::DEFAULT_STORE);
    });
    Ok(())
}

#[tauri::command]
pub fn home_tab(id: TabId, webview: Webview) -> Result<(), String> {
    require_chrome(&webview)?;
    home(webview.app_handle(), &id)
}

/// The Back button: tab `id`'s page steps back through its history. A tab with no webview has none.
#[tauri::command]
pub fn nav_back(id: TabId, webview: Webview) -> Result<(), String> {
    step_history(&webview, &id, "history.back()")
}

/// The Forward button.
#[tauri::command]
pub fn nav_forward(id: TabId, webview: Webview) -> Result<(), String> {
    step_history(&webview, &id, "history.forward()")
}

fn step_history(webview: &Webview, id: &TabId, js: &str) -> Result<(), String> {
    require_chrome(webview)?;
    match tabs::own_webview(&webview.window(), id) {
        Some(wv) => wv.eval(js).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Opens the row menu for tab `id` at the chrome's `(x, y)`: the Group menu acting on that row.
#[tauri::command]
pub fn row_menu(
    id: TabId,
    x: f64,
    y: f64,
    webview: Webview,
    state: State<AppState>,
) -> Result<(), String> {
    require_chrome(&webview)?;
    let ms = {
        let ws = state.ws.lock().unwrap();
        if ws.find(&id).is_none() {
            return Err("unknown tab".into());
        }
        crate::menu::menu_state_for(&ws, Some(&id))
    };
    let menu = crate::menu::row_menu(webview.app_handle(), &ms, &id).map_err(|e| e.to_string())?;
    webview
        .window()
        .popup_menu_at(&menu, tauri::LogicalPosition::new(x, y))
        .map_err(|e| e.to_string())
}

/// The banner's context menu (chrome-core's `onBannerMenu`) at the chrome's `(x, y)`: Edit
/// Window… and Delete Window… for the calling window. Its items arrive as menu events, which the
/// `WindowSet` handles.
#[tauri::command]
pub fn banner_menu(x: f64, y: f64, webview: Webview) -> Result<(), String> {
    require_chrome(&webview)?;
    let app = webview.app_handle();
    app.state::<WindowSet>()
        .banner_menu(app, webview.window().label(), x, y)
        .map_err(|e| e.to_string())
}

/// The webview showing tab `id`, in whichever window hosts it.
fn webview_of(app: &AppHandle, id: &TabId) -> Option<Webview> {
    app.webviews()
        .into_iter()
        .find(|(l, _)| tabs::tab_of(l).is_some_and(|(_, t)| t == *id))
        .map(|(_, wv)| wv)
}

/// Returns tab `id` to the URL it opened with. A tab with no webview is already there, unless it
/// is its window's active tab and its webview failed to build: Home is then a fresh attempt at it.
pub fn home(app: &AppHandle, id: &TabId) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (home, retry) = {
        let ws = state.ws.lock().unwrap();
        let home = ws.find(id).ok_or("unknown tab")?.2.home_url.clone();
        let created = state
            .created
            .lock()
            .unwrap()
            .values()
            .any(|c| c.contains(id));
        let retry = ws.is_active(id) && !created && state.failed.lock().unwrap().remove(id);
        (home, retry)
    };
    if retry {
        tabs::changed(app);
        return Ok(());
    }
    let Some(wv) = webview_of(app, id) else {
        return Ok(());
    };
    let url = Url::parse(&home).map_err(|e| e.to_string())?;
    shell_core::web::content::navigate_to(&wv.window(), wv.label(), &url).map_err(|e| e.to_string())
}

/// Applies `op` to the workspace, writes it to the config ([`crate::write::through`]), raises the
/// window of a popped-out tab it activated ([`crate::detach::apply`]), then brings everything in
/// line with it.
pub fn mutate(app: &AppHandle, op: impl FnOnce(&mut flotilla_core::model::Workspace)) {
    let raised = crate::write::through(app, |ws| crate::detach::apply(ws, op));
    if let Some(id) = &raised {
        crate::detach::raise(app, id);
    }
    tabs::changed(app);
}

/// The chrome's `#content-hole` rect, logical px, top-left origin.
#[derive(Deserialize)]
pub struct RectArg {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Who reports a content hole: a window's chrome, or a popped-out window's banner page (its
/// window's own webview, under a detached label). `None` for anyone else.
#[derive(Debug, PartialEq, Eq)]
pub enum HoleCaller {
    Window,
    Detached,
}

/// `listed`: whether `window` is one of the `WindowSet`'s windows.
pub fn hole_caller(webview: &str, window: &str, listed: bool) -> Option<HoleCaller> {
    if webview != window {
        None
    } else if shell_core::detach::is_detached_label(webview) {
        Some(HoleCaller::Detached)
    } else if listed {
        Some(HoleCaller::Window)
    } else {
        None
    }
}

/// Moves the calling window's content webviews to fill the hole it measured. A window's hole is
/// also stored: its new webviews are created into it.
#[tauri::command]
pub fn set_hole_rect(
    rect: RectArg,
    webview: Webview,
    state: State<AppState>,
) -> Result<(), String> {
    let window = webview.window();
    let listed = tabs::title_of_label(webview.app_handle(), window.label()).is_some();
    let caller =
        hole_caller(webview.label(), window.label(), listed).ok_or("forbidden: not a chrome")?;
    if ![rect.x, rect.y, rect.width, rect.height]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err("non-finite hole rect".into());
    }
    let hole = HoleRect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    };
    if caller == HoleCaller::Window {
        state
            .hole
            .lock()
            .unwrap()
            .insert(window.label().to_string(), hole);
    }
    layout_webviews(&window, hole);
    Ok(())
}

/// ⌘W in the window titled `title`: closes its active tab (a head closes its group).
pub fn close_active_tab(app: &AppHandle, title: &str) {
    mutate(app, |ws| {
        if let Some(id) = tabs::active_in(ws, title) {
            ws.close_tab(&id);
        }
    });
}

/// The home surface's "Create a starter config" button: seeds the starter config and applies it,
/// which opens its window.
#[tauri::command]
pub fn shell_home_create_config(app: AppHandle) -> Result<(), String> {
    let path = crate::config::config_path();
    let state = app.state::<AppState>();
    {
        let _held = state.gate.hold();
        if !crate::config::create_starter(&path)? {
            return Err(format!(
                "{} already exists — left untouched",
                path.display()
            ));
        }
        crate::reload_model(&app, crate::config::load(&path)?);
    }
    crate::refresh_views(&app);
    Ok(())
}

/// The home surface's "Edit Config" button (a broken config): the spine's own Edit Config.
#[tauri::command]
pub fn shell_home_edit_config() {
    let path = crate::config::config_path();
    shell_core::menu::handle_spine_event(shell_core::menu::ids::EDIT_CONFIG, &path);
}

/// The home surface's per-window button: open the window, or focus it if it is open.
#[tauri::command]
pub fn shell_home_open_window(id: String, app: AppHandle) {
    app.state::<WindowSet>().open_or_focus(&app, &id);
    tabs::changed(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAB: &str = "t-0123456789abcdef";

    #[test]
    fn only_a_window_own_page_reports_its_hole() {
        assert_eq!(
            hole_caller("rotary", "rotary", true),
            Some(HoleCaller::Window)
        );
        let d = crate::detach::window_label(TAB);
        assert_eq!(hole_caller(&d, &d, false), Some(HoleCaller::Detached));
        // A content webview, in either window.
        let content = tabs::label("rotary", TAB);
        assert_eq!(hole_caller(&content, "rotary", true), None);
        assert_eq!(hole_caller(&tabs::label(&d, TAB), &d, false), None);
        // A window the set does not list (the home surface, the window form).
        assert_eq!(hole_caller("other", "other", false), None);
    }

    #[test]
    fn group_urls_must_be_web_links() {
        assert_eq!(
            parse_group_url("  https://drive.google.com/drive/folders/f1 ")
                .unwrap()
                .as_str(),
            "https://drive.google.com/drive/folders/f1"
        );
        assert!(parse_group_url("").is_err());
        assert_eq!(
            parse_group_url("drive.google.com/x").unwrap_err(),
            "Not a web link: drive.google.com/x"
        );
        assert!(parse_group_url("file:///etc/passwd").is_err());
        assert!(parse_group_url("javascript:alert(1)").is_err());
    }
}
