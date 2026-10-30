//! flotilla's Tauri shell. Currently an empty workspace window: the shared chrome with no tabs.

mod persist;
mod sidebar;
mod state;
mod tabs;

use serde::Serialize;
use tauri::{Emitter, Manager, TitleBarStyle, WebviewUrl};

/// The single workspace window's label.
pub(crate) const MAIN_WINDOW: &str = "main";

/// First-run window size; shell-core's geometry plugin restores the saved bounds afterwards.
const FIRST_RUN_SIZE: (f64, f64) = (1400.0, 900.0);

/// Sidebar width the chrome starts at when none is saved.
const DEFAULT_SIDEBAR_W: u32 = 240;

/// What the chrome asks for at mount: title, density, drag and update gates.
#[derive(Serialize)]
struct WindowIdentity {
    title: String,
    colour: Option<String>,
    default_width: u32,
    density: &'static str,
    sidebar_drag: bool,
    auto_update: bool,
}

#[tauri::command]
fn window_identity() -> WindowIdentity {
    WindowIdentity {
        title: "flotilla".into(),
        colour: None,
        default_width: DEFAULT_SIDEBAR_W,
        density: "comfortable",
        sidebar_drag: true,
        auto_update: true,
    }
}

/// The tab list the chrome renders. Empty until the model lands.
#[tauri::command]
fn get_tabs() -> Vec<serde_json::Value> {
    Vec::new()
}

/// The chrome reports the content hole's rect; no content webviews exist yet to place.
#[tauri::command]
fn set_hole_rect(_rect: serde_json::Value) {}

fn config_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("flotilla")
        .join("config.toml")
}

fn build_app_menu<M: Manager<tauri::Wry>>(
    manager: &M,
) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{MenuBuilder, SubmenuBuilder};

    let path = config_path();
    let windows = [shell_core::menu::WindowEntry {
        id: MAIN_WINDOW.into(),
        title: "flotilla".into(),
        open: true,
        colour: None,
    }];
    let spine = shell_core::menu::build_spine(
        manager,
        shell_core::menu::SpineConfig {
            app_name: "flotilla",
            config_path: &path,
            windows: &windows,
        },
        env!("CARGO_PKG_VERSION"),
        env!("BUILD_GIT_SHA"),
        env!("BUILD_DATE"),
    )?;

    // The Edit submenu owns the clipboard accelerators (⌘C/⌘V/⌘X/⌘A/⌘Z) for content webviews.
    let edit_menu = SubmenuBuilder::new(manager, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let nav = shell_core::menu::build_tab_nav(manager, false)?;
    let nav_refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = nav
        .nav
        .iter()
        .map(|i| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>)
        .collect();
    let jump_refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = nav
        .jumps
        .iter()
        .map(|i| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>)
        .collect();
    let tabs_menu = SubmenuBuilder::new(manager, "Tab")
        .items(&nav_refs)
        .separator()
        .item(&spine.close_tab)
        .item(&spine.pop_out_tab)
        .item(&spine.find_in_sidebar)
        .separator()
        .items(&jump_refs)
        .build()?;

    MenuBuilder::new(manager)
        .items(&[
            &spine.submenus[0], // App
            &edit_menu,
            &tabs_menu,
            &spine.submenus[1], // Config
            &spine.submenus[2], // Window
        ])
        .build()
}

fn emit_to_main_chrome<S: Serialize + Clone>(app: &tauri::AppHandle, event: &str, payload: S) {
    let _ = app.emit_to(MAIN_WINDOW, event, payload);
}

pub fn run() {
    // The shell-core plugin set (geometry + updater + process) every sibling app installs identically.
    shell_core::register_plugins(tauri::Builder::default(), Some(&config_path()), &[])
        .invoke_handler(tauri::generate_handler![
            window_identity,
            get_tabs,
            set_hole_rect
        ])
        .setup(|app| {
            // The sidebar chrome is the window's main webview; overlay title bar with the traffic
            // lights floating over chrome-core's `#cc-titlebar` strip.
            tauri::WebviewWindowBuilder::new(
                app,
                MAIN_WINDOW,
                WebviewUrl::App("index.html".into()),
            )
            .title("flotilla")
            .inner_size(FIRST_RUN_SIZE.0, FIRST_RUN_SIZE.1)
            .hidden_title(true)
            .title_bar_style(TitleBarStyle::Overlay)
            .build()?;

            app.set_menu(build_app_menu(app)?)?;
            let cfg_path = config_path();
            app.on_menu_event(move |app, event| {
                let id = event.id().as_ref();
                if shell_core::menu::handle_spine_event(id, &cfg_path) {
                    return;
                }
                if let Some(action) = shell_core::menu::tab_nav_action(id) {
                    use shell_core::menu::TabNavAction;
                    match action {
                        TabNavAction::Next => emit_to_main_chrome(app, "nav-tab", 1i32),
                        TabNavAction::Prev => emit_to_main_chrome(app, "nav-tab", -1i32),
                        TabNavAction::Jump(n) => emit_to_main_chrome(app, "jump-tab", n),
                    }
                    return;
                }
                match id {
                    shell_core::menu::ids::CHECK_UPDATES => {
                        emit_to_main_chrome(app, "check-update", ())
                    }
                    shell_core::menu::ids::CLOSE_TAB => emit_to_main_chrome(app, "close-tab", ()),
                    shell_core::menu::ids::FIND_IN_SIDEBAR => {
                        emit_to_main_chrome(app, "find-in-sidebar", ())
                    }
                    _ => {}
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running flotilla");
}
