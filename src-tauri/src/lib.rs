//! flotilla's Tauri shell: one OS window per config `[[window]]` (shell-core's `WindowSet`), each
//! with a sidebar listing that window's groups and a content hole showing its active tab's webview.
//! The workspace is built from the config and written back to it; the session (each window's
//! active tab and Recent) is loaded at setup and saved as it changes.

mod commands;
mod config;
mod detach;
mod menu;
mod persist;
mod sidebar;
mod state;
mod tabs;
mod windows;
mod write;

use serde::Serialize;
use shell_core::windows::WindowSet;
use tauri::{Emitter, Manager};

/// Sidebar width the chrome starts at when none is saved.
const DEFAULT_SIDEBAR_W: u32 = 240;

/// What the chrome asks for at mount: its window's title and accent, density, drag and update
/// gates.
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
fn window_identity(
    webview: tauri::Webview,
    state: tauri::State<state::AppState>,
) -> WindowIdentity {
    let title =
        tabs::title_of_label(webview.app_handle(), webview.window().label()).unwrap_or_default();
    let cfg = state.config.lock().unwrap();
    WindowIdentity {
        colour: cfg.colour_of(&title),
        title,
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
    menu::invalidate(app);
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

/// Brings the views in line with the config in force: the `WindowSet` opens, closes, retitles and
/// recolours windows to match it; every window is themed; the webviews, sidebars and menu bar
/// follow the workspace; and each chrome re-reads its identity. Queued for the main loop
/// ([`defer`]), so it is safe from any caller.
pub(crate) fn refresh_views(app: &tauri::AppHandle) {
    let handle = app.clone();
    defer(app, move || {
        let (theme, decls) = {
            let state = handle.state::<state::AppState>();
            let cfg = state.config.lock().unwrap();
            (
                cfg.dark_mode.then_some(tauri::Theme::Dark),
                windows::decls(&cfg),
            )
        };
        handle.state::<WindowSet>().apply(&handle, decls);
        handle.set_theme(theme);
        tabs::changed(&handle);
        for (label, _) in tabs::open_windows(&handle) {
            let _ = handle.emit_to(label.as_str(), "config-reloaded", ());
        }
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
        hole: Default::default(),
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

/// The label of the window a popped-out window's tab belongs to: the window holding its title.
fn origin_of(app: &tauri::AppHandle, detached: &str) -> Option<String> {
    let id = detach::tab_of(detached)?;
    let title = tabs::window_title_of(&app.state::<state::AppState>().ws.lock().unwrap(), &id)?;
    app.state::<WindowSet>().label_for_title(&title)
}

/// Runs a spine command in the window `shell_core::menu::route` chose (a pop-out's commands go to
/// the window its tab is in). The chrome is that window's main webview, so its label is the
/// chrome's.
fn on_spine_action(app: &tauri::AppHandle, routed: shell_core::menu::Routed) {
    use shell_core::menu::{SpineAction, TabNavAction};
    let target = routed.target.as_str();
    if routed.from_detached.is_some() {
        app.state::<WindowSet>().open_or_focus(app, target);
    }
    let from = routed.from_detached.as_deref().and_then(detach::tab_of);
    let emit = |event: &str| {
        let _ = app.emit_to(target, event, ());
    };
    let title = || tabs::title_of_label(app, target);
    match routed.action {
        SpineAction::Nav(TabNavAction::Next) => {
            let _ = app.emit_to(target, "nav-tab", NavTab { dir: 1, from });
        }
        SpineAction::Nav(TabNavAction::Prev) => {
            let _ = app.emit_to(target, "nav-tab", NavTab { dir: -1, from });
        }
        SpineAction::Nav(TabNavAction::Jump(n)) => {
            let _ = app.emit_to(target, "jump-tab", n);
        }
        SpineAction::CloseTab => {
            if let Some(title) = title() {
                commands::close_active_tab(app, &title);
            }
        }
        SpineAction::DockPoppedOut => {
            if let Some(id) = detach::tab_of(target) {
                detach::pop_in(app, &id);
            }
        }
        SpineAction::PopOutTab => {
            let active = title().and_then(|t| {
                tabs::active_in(&app.state::<state::AppState>().ws.lock().unwrap(), &t)
            });
            if let Some(id) = active {
                if let Err(e) = commands::pop_out(app, &id) {
                    tabs::notice(app, e);
                }
            }
        }
        // ⌘⇧F: the content webview may hold first responder, so make the chrome the key view
        // before it focuses its field.
        SpineAction::FindInSidebar => {
            if let Some(chrome) = app.get_webview(target) {
                let _ = chrome.set_focus();
            }
            emit("find-in-sidebar");
        }
        SpineAction::CheckUpdates => emit("check-update"),
        // A pop-out's close docks its tab; a window's goes through the `WindowSet`'s
        // `CloseRequested` like the red button.
        SpineAction::CloseWindow => match detach::tab_of(target) {
            Some(id) => detach::pop_in(app, &id),
            None => {
                if let Some(w) = app.get_window(target) {
                    let _ = w.close();
                }
            }
        },
    }
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
            commands::banner_menu,
            commands::set_hole_rect,
            commands::shell_home_create_config,
            commands::shell_home_edit_config,
            commands::shell_home_open_window,
        ])
        .setup(|app| {
            let cfg_path = config::config_path();
            let (cfg, config_error) = match config::load(&cfg_path) {
                Ok(cfg) => (cfg, None),
                Err(e) => (config::Config::default(), Some(e)),
            };
            shell_core::web::insecure::set_allowlist(cfg.allow_insecure.clone());
            app.set_theme(cfg.dark_mode.then_some(tauri::Theme::Dark));
            let decls = windows::decls(&cfg);
            app.manage(load_state(app, cfg, config_error.clone()));
            // Closing the last window quits, popped-out windows and all; they reopen at launch.
            app.manage(
                WindowSet::new("flotilla", cfg_path.clone(), windows::FlotillaHost)
                    .quit_on_last_close(true),
            );
            watch_config(app.handle().clone(), cfg_path.clone());

            let handle = app.handle();
            let set = app.state::<WindowSet>();
            match config_error {
                Some(e) => set.set_load_error(handle, Some(e)),
                None => set.materialize(handle, decls),
            }
            // Restore lazily: only each window's active tab gets a webview now (this also builds
            // the menu bar); every popped-out tab reopens in its own window.
            tabs::sync(handle);
            detach::reconcile(handle);

            let nav_handle = handle.clone();
            shell_core::mouse_nav::install(move || tabs::focused_webview(&nav_handle));

            // Row and banner context menus' events arrive here too.
            app.on_menu_event(move |app, event| {
                let id = event.id().as_ref();
                // Edit Config on a missing file seeds the starter config first, so there is
                // something to open.
                if id == shell_core::menu::ids::EDIT_CONFIG {
                    if let Err(e) = config::create_starter(&cfg_path) {
                        tabs::notice(app, e);
                    }
                }
                // New Window…, and the banner's Edit Window… / Delete Window….
                if app.state::<WindowSet>().handle_menu(app, id) {
                    return;
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
                        menu::invalidate(app);
                        menu::refresh(app);
                    }
                    return;
                }
                // Window ▸ <a window>: focus it, or reopen it.
                if let Some(label) = shell_core::menu::selected_window(id) {
                    app.state::<WindowSet>().open_or_focus(app, label);
                    menu::invalidate(app);
                    tabs::changed(app);
                    return;
                }
                let focused = app
                    .get_focused_window()
                    .map(|w| w.label().to_string())
                    .or_else(|| menu::focused(app).map(|(label, _)| label))
                    .unwrap_or_default();
                if let Some(routed) = shell_core::menu::route(id, &focused, |l| origin_of(app, l)) {
                    on_spine_action(app, routed);
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building flotilla")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                app.state::<WindowSet>().mark_quitting();
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
