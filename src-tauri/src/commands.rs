//! The commands the chrome sidebar invokes. Sync commands, so they run on the main thread and may
//! touch webviews directly; each locks `ws` only to change or read it.

use crate::state::AppState;
use crate::tabs;
use crate::MAIN_WINDOW;
use flotilla_core::model::TabId;
use serde::Deserialize;
use shell_core::compositing::{layout_webviews, HoleRect};
use tauri::{AppHandle, Manager, State, Url, Webview};

/// The login store every group opens in until per-group logins are configurable.
pub const DEFAULT_STORE: &str = "default";

/// Content webviews load remote origins, which Tauri already refuses app commands to; this keeps
/// the commands to the main window's own chrome page regardless.
fn require_chrome(webview: &Webview) -> Result<(), String> {
    if webview.label() == MAIN_WINDOW {
        Ok(())
    } else {
        Err("forbidden: not the chrome".into())
    }
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

/// The sidebar DTO, for the chrome's first paint (later ones arrive as `workspace` events).
#[tauri::command]
pub fn get_workspace(
    webview: Webview,
    state: State<AppState>,
) -> Result<serde_json::Value, String> {
    require_chrome(&webview)?;
    let ws = state.ws.lock().unwrap();
    Ok(crate::sidebar::sidebar_dto(
        &ws,
        &state.created.lock().unwrap(),
    ))
}

/// What the error bar shows at launch (a quarantined or unpersistable session).
#[tauri::command]
pub fn startup_notice(state: State<AppState>) -> Option<String> {
    state.notice.clone()
}

#[tauri::command]
pub fn select_tab(id: TabId, webview: Webview, state: State<AppState>) -> Result<(), String> {
    require_chrome(&webview)?;
    {
        let mut ws = state.ws.lock().unwrap();
        if ws.find(&id).is_none() {
            return Err("unknown tab".into());
        }
        ws.activate(&id);
    }
    // An explicit selection is a fresh attempt at a tab whose webview failed to build.
    state.failed.lock().unwrap().remove(&id);
    tabs::changed(webview.app_handle());
    Ok(())
}

/// The row dot's Unload: closes tab `id`'s webview and keeps the tab. If it was active, the nearest
/// live visible tab takes over ([`tabs::after_unload`]), else nothing is shown; no cold tab wakes.
#[tauri::command]
pub fn unload_tab(id: TabId, webview: Webview, state: State<AppState>) -> Result<(), String> {
    require_chrome(&webview)?;
    let window = webview.window();
    if let Some(wv) = window
        .get_webview(&tabs::label(&id))
        .filter(|wv| wv.window().label() == window.label())
    {
        shell_core::web::content::close_content_webview(&wv);
    }
    let active_moved = {
        let mut ws = state.ws.lock().unwrap();
        let mut created = state.created.lock().unwrap();
        let was_active = ws.active.as_ref() == Some(&id);
        let next = tabs::after_unload(&ws, &created, &id);
        created.remove(&id);
        if was_active {
            ws.active = None;
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
pub fn toggle_group(
    head: TabId,
    collapsed: bool,
    webview: Webview,
    state: State<AppState>,
) -> Result<(), String> {
    require_chrome(&webview)?;
    state.ws.lock().unwrap().set_collapsed(&head, collapsed);
    tabs::push_dto(webview.app_handle());
    crate::persist::mark_dirty(webview.app_handle());
    Ok(())
}

/// Opens `url` as a new group (or raises the tab already showing it). The error is for the
/// sidebar's error bar.
#[tauri::command]
pub fn new_group(url: String, webview: Webview, state: State<AppState>) -> Result<(), String> {
    require_chrome(&webview)?;
    let url = parse_group_url(&url)?;
    state.ws.lock().unwrap().new_group(&url, DEFAULT_STORE);
    tabs::changed(webview.app_handle());
    Ok(())
}

/// Returns tab `id` to the URL it opened with. A tab with no webview yet is already there.
#[tauri::command]
pub fn home_tab(id: TabId, webview: Webview, state: State<AppState>) -> Result<(), String> {
    require_chrome(&webview)?;
    let home = {
        let ws = state.ws.lock().unwrap();
        ws.find(&id).ok_or("unknown tab")?.1.home_url.clone()
    };
    let url = Url::parse(&home).map_err(|e| e.to_string())?;
    shell_core::web::content::navigate_to(&webview.window(), &tabs::label(&id), &url)
        .map_err(|e| e.to_string())
}

/// The chrome's `#content-hole` rect, logical px, top-left origin.
#[derive(Deserialize)]
pub struct RectArg {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Stores the hole the chrome measured (new webviews are created into it) and moves the existing
/// content webviews to fill it.
#[tauri::command]
pub fn set_hole_rect(
    rect: RectArg,
    webview: Webview,
    state: State<AppState>,
) -> Result<(), String> {
    require_chrome(&webview)?;
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
    *state.hole.lock().unwrap() = hole;
    layout_webviews(&webview.window(), hole);
    Ok(())
}

/// ⌘W: closes the active tab (a head closes its group).
pub fn close_active_tab(app: &AppHandle) {
    {
        let state = app.state::<AppState>();
        let mut ws = state.ws.lock().unwrap();
        let Some(id) = ws.active.clone() else {
            return;
        };
        ws.close_tab(&id);
    }
    tabs::changed(app);
}

#[cfg(test)]
mod tests {
    use super::*;

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
