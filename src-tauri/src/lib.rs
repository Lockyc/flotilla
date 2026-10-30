//! flotilla's Tauri shell: one window whose sidebar lists the workspace's groups and whose content
//! hole shows the active tab's webview. The session is loaded at setup and saved as it changes.

mod commands;
mod config;
mod detach;
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
fn window_identity(state: tauri::State<state::AppState>) -> WindowIdentity {
    let cfg = state.config.lock().unwrap();
    WindowIdentity {
        title: "flotilla".into(),
        colour: None,
        default_width: DEFAULT_SIDEBAR_W,
        density: cfg.density.as_str(),
        sidebar_drag: true,
        auto_update: cfg.auto_update,
    }
}

/// Applies a reloaded config: remembers it, themes every window, and has the chrome re-read its
/// identity. `allow_insecure` is not applied here; it is read once at launch.
fn apply_config(app: &tauri::AppHandle, cfg: config::Config) {
    app.set_theme(cfg.dark_mode.then_some(tauri::Theme::Dark));
    *app.state::<state::AppState>().config.lock().unwrap() = cfg;
    emit_to_main_chrome(app, "config-reloaded", ());
}

/// Watches the config file: a clean reload is applied (and reformatted when `format_on_save`), a
/// broken one keeps the last good config and shows its error in the sidebar.
fn watch_config(app: tauri::AppHandle, path: std::path::PathBuf) {
    let fmt_path = path.clone();
    shell_core::watch::watch_config(path, move |src| match config::parse(src) {
        Ok(cfg) => {
            // Returning the formatted bytes makes the watcher swallow the echo of our own write.
            let formatted = cfg
                .format_on_save
                .then(|| config_core::format_str(src))
                .filter(|f| f != src && config_core::format_file(&fmt_path).is_ok());
            apply_config(&app, cfg);
            formatted
        }
        Err(msg) => {
            tabs::notice(&app, msg);
            None
        }
    });
}

/// Loads the session from the app data dir and builds the state it seeds. A session notice and a
/// config error share the sidebar's error bar.
fn load_state(
    app: &tauri::App,
    config: config::Config,
    config_error: Option<String>,
) -> state::AppState {
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
    let notice = match (persist::load_notice(&loaded), config_error) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (a, b) => a.or(b),
    };
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
        menu: Default::default(),
        config: std::sync::Mutex::new(config),
        persist,
        notice,
    }
}

/// The popped-out tab whose window has key focus, if one does.
fn focused_detached(app: &tauri::AppHandle) -> Option<flotilla_core::model::TabId> {
    detach::tab_of(app.get_focused_window()?.label())
}

fn emit_to_main_chrome<S: Serialize + Clone>(app: &tauri::AppHandle, event: &str, payload: S) {
    let _ = app.emit_to(MAIN_WINDOW, event, payload);
}

pub fn run() {
    // The shell-core plugin set (geometry + updater + process) every sibling app installs identically.
    shell_core::register_plugins(tauri::Builder::default(), Some(&config::config_path()), &[])
        .invoke_handler(tauri::generate_handler![
            window_identity,
            commands::get_workspace,
            commands::startup_notice,
            commands::select_tab,
            commands::toggle_group,
            commands::new_group,
            commands::home_tab,
            commands::unload_tab,
            commands::pop_out_tab,
            commands::pop_in_tab,
            commands::row_menu,
            commands::set_hole_rect,
        ])
        .setup(|app| {
            let cfg_path = config::config_path();
            let (cfg, config_error) = match config::load(&cfg_path) {
                Ok(cfg) => (cfg, None),
                Err(e) => (config::Config::default(), Some(e)),
            };
            shell_core::web::insecure::set_allowlist(cfg.allow_insecure.clone());
            app.set_theme(cfg.dark_mode.then_some(tauri::Theme::Dark));
            app.manage(load_state(app, cfg, config_error));
            watch_config(app.handle().clone(), cfg_path.clone());

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
            // Closing the workspace window quits, popped-out windows and all; they reopen at
            // launch.
            let closing = window.as_ref().window();
            shell_core::web::content::wire_close_pages(&closing);
            let app_handle = app.handle().clone();
            window.on_window_event(move |event| match event {
                tauri::WindowEvent::CloseRequested { .. } => detach::mark_quitting(),
                tauri::WindowEvent::Destroyed => app_handle.exit(0),
                _ => {}
            });
            // Restore lazily: only the active tab's webview is created now (this also builds the
            // menu bar); every popped-out tab reopens in its own window.
            tabs::sync(app.handle());
            detach::reconcile(app.handle());

            // Row context menus' events arrive here too.
            app.on_menu_event(move |app, event| {
                let id = event.id().as_ref();
                // Edit Config on a missing file seeds the starter config first, so there is
                // something to open.
                if id == shell_core::menu::ids::EDIT_CONFIG {
                    if let Err(e) = config::create_starter(&cfg_path) {
                        tabs::notice(app, e);
                    }
                }
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
                    // In a popped-out window, ⌘W closes it (docking its tab) and ⌘⇧O does nothing.
                    shell_core::menu::ids::CLOSE_TAB => match focused_detached(app) {
                        Some(id) => detach::pop_in(app, &id),
                        None => commands::close_active_tab(app),
                    },
                    shell_core::menu::ids::POP_OUT_TAB => {
                        let active = app
                            .state::<state::AppState>()
                            .ws
                            .lock()
                            .unwrap()
                            .active
                            .clone();
                        if let (None, Some(id)) = (focused_detached(app), active) {
                            if let Err(e) = commands::pop_out(app, &id) {
                                tabs::notice(app, e);
                            }
                        }
                    }
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
                detach::mark_quitting();
                for window in app.windows().values() {
                    shell_core::web::content::close_window_pages(window);
                }
                persist::save_now(app);
            }
        });
}
