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
        title: cfg.title.clone(),
        colour: cfg.accent(),
        default_width: DEFAULT_SIDEBAR_W,
        density: cfg.density.as_str(),
        sidebar_drag: true,
        auto_update: cfg.auto_update,
    }
}

/// Applies a reloaded config: remembers it, themes every window, retitles the main window, rebuilds
/// the menu bar (its Window entry and tab shortcuts), and has the chrome re-read its identity. `allow_insecure` is not applied
/// here; it is read once at launch.
fn apply_config(app: &tauri::AppHandle, cfg: config::Config) {
    app.set_theme(cfg.dark_mode.then_some(tauri::Theme::Dark));
    if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
        let _ = w.set_title(&cfg.window_title());
    }
    let state = app.state::<state::AppState>();
    *state.config.lock().unwrap() = cfg;
    *state.menu.lock().unwrap() = None;
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || menu::refresh(&handle));
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
    let mut notice = state::Notices::default();
    for msg in [persist::load_notice(&loaded), config_error]
        .into_iter()
        .flatten()
    {
        notice.raise(msg);
    }
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
        notice: std::sync::Mutex::new(notice),
    }
}

/// The `nav-tab` payload: step `dir` from `from`, a popped-out tab, or else from the highlight.
#[derive(Clone, Serialize)]
struct NavTab {
    dir: i32,
    from: Option<flotilla_core::model::TabId>,
}

/// Runs a spine command where `shell_core::menu::route` sent it. Only the main window has a
/// sidebar, so every sidebar command goes to its chrome.
fn on_spine_action(app: &tauri::AppHandle, routed: shell_core::menu::Routed) {
    use shell_core::menu::{SpineAction, TabNavAction};
    if routed.from_detached.is_some() {
        tabs::bring_main_forward(app);
    }
    let from = routed.from_detached.as_deref().and_then(detach::tab_of);
    match routed.action {
        SpineAction::Nav(TabNavAction::Next) => {
            emit_to_main_chrome(app, "nav-tab", NavTab { dir: 1, from })
        }
        SpineAction::Nav(TabNavAction::Prev) => {
            emit_to_main_chrome(app, "nav-tab", NavTab { dir: -1, from })
        }
        SpineAction::Nav(TabNavAction::Jump(n)) => emit_to_main_chrome(app, "jump-tab", n),
        SpineAction::CloseTab => commands::close_active_tab(app),
        SpineAction::DockPoppedOut => {
            if let Some(id) = detach::tab_of(&routed.target) {
                detach::pop_in(app, &id);
            }
        }
        SpineAction::PopOutTab => {
            let active = app
                .state::<state::AppState>()
                .ws
                .lock()
                .unwrap()
                .active
                .clone();
            if let Some(id) = active {
                if let Err(e) = commands::pop_out(app, &id) {
                    tabs::notice(app, e);
                }
            }
        }
        SpineAction::FindInSidebar => emit_to_main_chrome(app, "find-in-sidebar", ()),
        SpineAction::CheckUpdates => emit_to_main_chrome(app, "check-update", ()),
        // Closing a pop-out docks its tab; the workspace window itself stays (flotilla has no
        // other surface to fall back to).
        SpineAction::CloseWindow => {
            if let Some(id) = detach::tab_of(&routed.target) {
                detach::pop_in(app, &id);
            }
        }
    }
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
            commands::nav_back,
            commands::nav_forward,
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
            let window_title = cfg.window_title();
            app.manage(load_state(app, cfg, config_error));
            watch_config(app.handle().clone(), cfg_path.clone());

            // The sidebar chrome is the window's main webview; overlay title bar with the traffic
            // lights floating over chrome-core's `#cc-titlebar` strip.
            let window = tauri::WebviewWindowBuilder::new(
                app,
                MAIN_WINDOW,
                WebviewUrl::App("index.html".into()),
            )
            .title(window_title)
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
            commands::open_early_links(app.handle());

            let nav_handle = app.handle().clone();
            shell_core::mouse_nav::install(move || tabs::focused_webview(&nav_handle));

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
                // muda flips a check item's tick on click: a failed write rebuilds to undo it; a
                // saved one reloads the config, which rebuilds anyway.
                if id == shell_core::menu::ids::TAB_DIGIT_CYCLE {
                    if let Err(e) = config::toggle_tab_digit_keys(&cfg_path) {
                        tabs::notice(app, e);
                        *app.state::<state::AppState>().menu.lock().unwrap() = None;
                        menu::refresh(app);
                    }
                    return;
                }
                // Window ▸ <the main window>: the only window the spine lists.
                if shell_core::menu::selected_window(id) == Some(MAIN_WINDOW) {
                    tabs::bring_main_forward(app);
                    *app.state::<state::AppState>().menu.lock().unwrap() = None;
                    menu::refresh(app);
                    return;
                }
                let focused = app
                    .get_focused_window()
                    .map_or_else(|| MAIN_WINDOW.to_string(), |w| w.label().to_string());
                // Every popped-out tab came from the one main window.
                let origin_of = |l: &str| detach::tab_of(l).map(|_| MAIN_WINDOW.to_string());
                if let Some(routed) = shell_core::menu::route(id, &focused, origin_of) {
                    on_spine_action(app, routed);
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building flotilla")
        .run(|app, event| match event {
            // `open -a flotilla <url>`, or a link router sending a link here.
            tauri::RunEvent::Opened { urls } => {
                for url in &urls {
                    commands::open_handed(app, url);
                }
            }
            tauri::RunEvent::ExitRequested { .. } => {
                detach::mark_quitting();
                for window in app.windows().values() {
                    shell_core::web::content::close_window_pages(window);
                }
                persist::save_now(app);
            }
            _ => {}
        });
}
