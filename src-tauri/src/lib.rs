//! flotilla's Tauri shell: one window whose sidebar lists the workspace's groups and whose content
//! hole shows the active tab's webview. The session is loaded at setup and saved as it changes.

mod commands;
mod menu;
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

/// Loads the session from the app data dir and builds the state it seeds.
fn load_state(app: &tauri::App) -> state::AppState {
    use flotilla_core::session::{self, Loaded};
    let (path, loaded) = match app.path().app_data_dir() {
        Ok(dir) => {
            let path = dir.join("session.json");
            let loaded = session::load(&path);
            (Some(path), loaded)
        }
        Err(e) => (
            None,
            Loaded::Unpersistable {
                reason: format!("no app data folder: {e}"),
            },
        ),
    };
    let notice = persist::load_notice(&loaded);
    let persist = persist::Persist::for_load(path, &loaded);
    let ws = match loaded {
        Loaded::Restored(ws) => ws,
        _ => Default::default(),
    };
    state::AppState {
        ws: std::sync::Mutex::new(ws),
        hole: std::sync::Mutex::new(shell_core::compositing::initial_hole(
            FIRST_RUN_SIZE.0,
            FIRST_RUN_SIZE.1,
        )),
        created: Default::default(),
        failed: Default::default(),
        persist,
        notice,
    }
}

pub(crate) fn config_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("flotilla")
        .join("config.toml")
}

fn emit_to_main_chrome<S: Serialize + Clone>(app: &tauri::AppHandle, event: &str, payload: S) {
    let _ = app.emit_to(MAIN_WINDOW, event, payload);
}

pub fn run() {
    // The shell-core plugin set (geometry + updater + process) every sibling app installs identically.
    shell_core::register_plugins(tauri::Builder::default(), Some(&config_path()), &[])
        .invoke_handler(tauri::generate_handler![
            window_identity,
            commands::get_workspace,
            commands::startup_notice,
            commands::select_tab,
            commands::toggle_group,
            commands::new_group,
            commands::home_tab,
            commands::unload_tab,
            commands::row_menu,
            commands::set_hole_rect,
        ])
        .setup(|app| {
            let state = load_state(app);
            app.manage(state);

            // The sidebar chrome is the window's main webview; overlay title bar with the traffic
            // lights floating over chrome-core's `#cc-titlebar` strip.
            let window = tauri::WebviewWindowBuilder::new(
                app,
                MAIN_WINDOW,
                WebviewUrl::App("index.html".into()),
            )
            .title("flotilla")
            .inner_size(FIRST_RUN_SIZE.0, FIRST_RUN_SIZE.1)
            .hidden_title(true)
            .title_bar_style(TitleBarStyle::Overlay)
            .build()?;
            // Closing the window drops its content webviews, which would leave their pages running.
            let closing = window.as_ref().window();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { .. } = event {
                    shell_core::web::content::close_window_pages(&closing);
                }
            });
            // Restore lazily: only the active tab's webview is created now.
            tabs::sync(app.handle());

            app.set_menu(menu::build_app_menu(app.handle())?)?;
            let cfg_path = config_path();
            // Row context menus' events arrive here too.
            app.on_menu_event(move |app, event| {
                let id = event.id().as_ref();
                if shell_core::menu::handle_spine_event(id, &cfg_path) || menu::handle(app, id) {
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
                    shell_core::menu::ids::CLOSE_TAB => commands::close_active_tab(app),
                    shell_core::menu::ids::FIND_IN_SIDEBAR => {
                        emit_to_main_chrome(app, "find-in-sidebar", ())
                    }
                    _ => {}
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building flotilla")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(window) = app.get_window(MAIN_WINDOW) {
                    shell_core::web::content::close_window_pages(&window);
                }
                persist::save_now(app);
            }
        });
}
