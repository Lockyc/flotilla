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
mod write;

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
        title: cfg
            .windows
            .first()
            .map(|w| w.title.trim().to_string())
            .unwrap_or_default(),
        colour: cfg.accent(),
        default_width: DEFAULT_SIDEBAR_W,
        density: cfg.density.as_str(),
        sidebar_drag: true,
        auto_update: cfg.auto_update,
    }
}

/// Applies a reloaded config on the watcher thread: [`reload_model`], then [`refresh_views`],
/// which reaches the main thread by the main loop. `allow_insecure` is not applied here; it is
/// read once at launch.
fn apply_config(app: &tauri::AppHandle, cfg: config::Config) {
    reload_model(app, cfg);
    refresh_views(app);
}

/// Remembers a reloaded config and rebases the workspace on its windows; no view work, so it is
/// safe under the write gate on any thread.
pub(crate) fn reload_model(app: &tauri::AppHandle, cfg: config::Config) {
    let state = app.state::<state::AppState>();
    let fresh = flotilla_core::model::Workspace::from_config(&cfg.workspace_windows());
    state.ws.lock().unwrap().rebase(fresh);
    *state.config.lock().unwrap() = cfg;
    *state.menu.lock().unwrap() = None;
}

/// Queues `task` for a later turn of the main loop. `run_on_main_thread` called on the main thread
/// runs the task inline, inside whatever is on the stack (WebKit's policy callback, a held gate);
/// posted from another thread it goes through the event-loop proxy and waits its turn.
pub(crate) fn defer(app: &tauri::AppHandle, task: impl FnOnce() + Send + 'static) {
    let handle = app.clone();
    from_another_thread(
        move |task| {
            let _ = handle.run_on_main_thread(task);
        },
        task,
    );
}

/// Calls `post(task)` on a fresh thread.
fn from_another_thread<T: Send + 'static>(post: impl FnOnce(T) + Send + 'static, task: T) {
    std::thread::spawn(move || post(task));
}

/// Brings the views in line with the config in force: themes every window, retitles the main
/// window, brings the webviews, sidebar and menu bar (its Window entry and tab shortcuts) in line,
/// and has the chrome re-read its identity. Queued for the main loop ([`defer`]), so it is safe
/// from any caller.
pub(crate) fn refresh_views(app: &tauri::AppHandle) {
    let handle = app.clone();
    defer(app, move || {
        let (theme, title) = {
            let cfg = handle
                .state::<state::AppState>()
                .config
                .lock()
                .unwrap()
                .clone();
            (
                cfg.dark_mode.then_some(tauri::Theme::Dark),
                cfg.window_title(),
            )
        };
        handle.set_theme(theme);
        if let Some(w) = handle.get_webview_window(MAIN_WINDOW) {
            let _ = w.set_title(&title);
        }
        tabs::changed(&handle);
        emit_to_main_chrome(&handle, "config-reloaded", ());
    });
}

/// Watches the config file: a clean reload is applied (and reformatted when `format_on_save`), a
/// broken one keeps the last good config and shows its error in the sidebar. Each reload runs
/// under the write gate and reads the file again there (`write::Gate::reload`).
fn watch_config(app: tauri::AppHandle, path: std::path::PathBuf) {
    let read_path = path.clone();
    shell_core::watch::watch_config(path, move |delivered| {
        let state = app.state::<state::AppState>();
        let held = state.gate.reload(&read_path, |read| {
            let src = read.unwrap_or_else(|_| delivered.to_string());
            match config::parse(&src) {
                Ok(cfg) => {
                    let formatted = cfg
                        .format_on_save
                        .then(|| config_core::format_str(&src))
                        .filter(|f| *f != src && config_core::format_file(&read_path).is_ok());
                    apply_config(&app, cfg);
                    formatted.unwrap_or(src)
                }
                Err(msg) => {
                    tabs::notice(&app, msg);
                    src
                }
            }
        });
        // What the app now holds, when it is not what the watcher read: a reformat, or a file
        // read again whole. Either way its echo is swallowed.
        (held != delivered).then_some(held)
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
    let mut ws = flotilla_core::model::Workspace::from_config(&config.workspace_windows());
    if let Loaded::Restored(s) = loaded {
        ws.restore(s.active, s.recent);
    }
    state::AppState {
        gate: Default::default(),
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
            let active = tabs::shown_active(&app.state::<state::AppState>().ws.lock().unwrap());
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
                    let toggled = {
                        let state = app.state::<state::AppState>();
                        let _held = state.gate.hold();
                        config::toggle_tab_digit_keys(&cfg_path)
                    };
                    if let Err(e) = toggled {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `defer`'s post leaves the caller's thread, so `run_on_main_thread` goes through the
    /// event-loop proxy instead of running the task inline on a main-thread caller.
    #[test]
    fn a_deferred_task_is_posted_from_another_thread() {
        let (tx, rx) = std::sync::mpsc::channel();
        from_another_thread(
            move |task: u8| tx.send((std::thread::current().id(), task)).unwrap(),
            7,
        );
        let (poster, task) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_ne!(poster, std::thread::current().id());
        assert_eq!(task, 7);
    }
}
