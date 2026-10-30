//! flotilla's side of shell-core's shared window layer: the [`WindowHost`] that builds a config
//! window's chrome and tears down its content, writes the window form's changes to the config, and
//! the config → [`WindowDecl`] mapping. A window's workspace (its groups and active tab) is the
//! workspace window with its title; its label only names its OS window and content webviews.

use crate::state::AppState;
use config_core::{add_window, edit_window, remove, At};
use flotilla_core::model::{TabId, Workspace};
use shell_core::compositing::HoleRect;
use shell_core::windows::plan::{Slot, WindowDecl};
use shell_core::windows::{WindowHost, WindowSet, WindowWrite};
use std::collections::{HashMap, HashSet};
use tauri::{AppHandle, Emitter, Manager, TitleBarStyle, WebviewUrl, WebviewWindow};

/// What shell-core needs to know about one `[[window]]`.
pub(crate) fn decl(w: &crate::config::FlotillaWindow) -> WindowDecl {
    WindowDecl {
        title: w.title.trim().to_string(),
        colour: w.colour.clone(),
        width: w.width as f64,
        height: w.height as f64,
        open_on_start: w.open_on_start,
    }
}

/// Every window's [`decl`], config order.
pub(crate) fn decls(cfg: &crate::config::Config) -> Vec<WindowDecl> {
    cfg.windows.iter().map(decl).collect()
}

/// Carries the window titled `old`'s in-place rename or recolour (`decl`) into the held config and
/// the workspace, ahead of the config reload that echoes it.
pub(crate) fn retitle(
    ws: &mut Workspace,
    cfg: &mut crate::config::Config,
    old: &str,
    decl: &WindowDecl,
) {
    let (old, title) = (old.trim(), decl.title.trim());
    if let Some(w) = ws.windows.iter_mut().find(|w| w.title.trim() == old) {
        w.title = title.to_string();
    }
    config_core::retitle_window(&mut cfg.windows, old, title, decl.colour.clone());
}

/// Builds the window for `slot`: the chrome page (`index.html`) under the slot's label, with the
/// traffic lights floating over chrome-core's `#cc-titlebar` strip. Its content webviews are
/// created by the next [`crate::tabs::sync`], which this queues.
fn build(app: &AppHandle, slot: &Slot) -> tauri::Result<WebviewWindow> {
    let dark = app.state::<AppState>().config.lock().unwrap().dark_mode;
    let window =
        tauri::WebviewWindowBuilder::new(app, &slot.label, WebviewUrl::App("index.html".into()))
            .title(&slot.decl.title)
            .inner_size(slot.decl.width, slot.decl.height)
            .hidden_title(true)
            .title_bar_style(TitleBarStyle::Overlay)
            .theme(dark.then_some(tauri::Theme::Dark))
            .build()?;
    crate::menu::follow_focus(app, &window.as_ref().window());
    let later = app.clone();
    crate::defer(app, move || crate::tabs::changed(&later));
    Ok(window)
}

/// What a window's closing drops: the record of its content webviews and its content hole, which
/// went with it. Its workspace window (groups, active tab) is untouched.
pub(crate) fn forget(
    created: &mut HashMap<String, HashSet<TabId>>,
    hole: &mut HashMap<String, HoleRect>,
    label: &str,
) {
    created.remove(label);
    hole.remove(label);
}

/// flotilla's per-window content, built and torn down by the managed [`WindowSet`].
pub(crate) struct FlotillaHost;

impl WindowHost for FlotillaHost {
    fn build(&self, app: &AppHandle, slot: &Slot) -> tauri::Result<WebviewWindow> {
        build(app, slot)
    }

    /// The window's content webviews went with it; its workspace window keeps its groups and
    /// active tab, so it reopens as it was.
    fn closed(&self, app: &AppHandle, label: &str) {
        let state = app.state::<AppState>();
        forget(
            &mut state.created.lock().unwrap(),
            &mut state.hole.lock().unwrap(),
            label,
        );
        if !app.state::<WindowSet>().is_quitting() {
            crate::menu::invalidate(app);
            crate::menu::refresh(app);
        }
    }

    fn retitled(&self, app: &AppHandle, label: &str, old: &str, decl: &WindowDecl) {
        {
            let state = app.state::<AppState>();
            let mut ws = state.ws.lock().unwrap();
            retitle(&mut ws, &mut state.config.lock().unwrap(), old, decl);
        }
        let _ = app.emit_to(label, "config-reloaded", ());
        crate::tabs::push_dto(app);
        crate::persist::mark_dirty(app);
    }

    /// Runs under the write gate, released before returning: the set calls `rename` or `delete`
    /// next, which reach [`Self::retitled`] / [`Self::closed`].
    fn write(&self, app: &AppHandle, write: &WindowWrite) -> Result<(), String> {
        let path = crate::config::config_path();
        let state = app.state::<AppState>();
        let _held = state.gate.hold();
        crate::config::create_starter(&path)?;
        match write {
            WindowWrite::Add { title, colour } => add_window(&path, title, colour.as_deref()),
            WindowWrite::Set {
                title,
                new_title,
                colour,
            } => edit_window(
                &path,
                title,
                new_title.as_deref(),
                colour.as_ref().map(Option::as_deref),
            ),
            WindowWrite::Remove { title } => remove(
                &path,
                At {
                    window: title,
                    group: None,
                    tab: None,
                },
            ),
        }
        .map_err(|e| e.to_string())
    }

    fn edit_failed(&self, app: &AppHandle, message: &str) {
        crate::tabs::notice(app, message.to_string());
    }

    fn changed(&self, app: &AppHandle) {
        crate::menu::invalidate(app);
        crate::menu::refresh(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(src: &str) -> crate::config::Config {
        crate::config::parse(src).unwrap()
    }

    #[test]
    fn windows_map_to_decls_in_config_order() {
        let c = cfg("[[window]]\ntitle = \" Club \"\ncolour = \"#abc\"\nwidth = 800\nheight = 600\nopen_on_start = false\n[[window]]\ntitle = \"Home\"\n");
        assert_eq!(
            decls(&c),
            vec![
                WindowDecl {
                    title: "Club".into(),
                    colour: Some("#aabbcc".into()),
                    width: 800.0,
                    height: 600.0,
                    open_on_start: false,
                },
                WindowDecl {
                    title: "Home".into(),
                    colour: None,
                    width: 1500.0,
                    height: 1000.0,
                    open_on_start: true,
                },
            ]
        );
    }

    #[test]
    fn a_renamed_window_keeps_its_groups_and_active_tab() {
        let mut c = cfg(
            "[[window]]\ntitle = \"A\"\n[[window]]\ntitle = \"B\"\n\
             [[window.group]]\nname = \"F\"\nurl = \"https://drive.google.com/drive/folders/f1\"\n",
        );
        let mut ws = Workspace::from_config(&c.workspace_windows());
        let head = ws.windows[1].groups[0].head.id.clone();
        ws.activate(&head);
        // The set reports B renamed to "Newsletters".
        let decl = WindowDecl {
            title: "Newsletters".into(),
            colour: Some("#2a6f4e".into()),
            width: 1500.0,
            height: 1000.0,
            open_on_start: true,
        };
        retitle(&mut ws, &mut c, " B ", &decl);
        assert_eq!(ws.windows[1].title, "Newsletters");
        assert_eq!(ws.windows[1].groups.len(), 1);
        assert_eq!(c.windows[1].title, "Newsletters");
        assert_eq!(c.colour_of("Newsletters").as_deref(), Some("#2a6f4e"));
        // The reload that echoes the write changes nothing, the active tab included.
        let before = ws.clone();
        ws.rebase(Workspace::from_config(&c.workspace_windows()));
        assert_eq!(ws, before);
        assert_eq!(ws.windows[1].active, Some(head));
    }

    #[test]
    fn closing_a_window_keeps_it_in_the_window_menu() {
        // A closed window is still a workspace window: its groups and active tab stay, and the
        // session still records its active tab, so the Window menu's (closed) entry reopens it
        // as it was.
        let c = cfg("[[window]]\ntitle = \"A\"\n[[window]]\ntitle = \"B\"\n");
        let mut ws = Workspace::from_config(&c.workspace_windows());
        let id = match ws
            .new_group(
                "B",
                &url::Url::parse("https://drive.google.com/drive/folders/f1").unwrap(),
                "default",
            )
            .unwrap()
        {
            flotilla_core::model::Opened::Added(id) => id,
            other => panic!("{other:?}"),
        };
        use shell_core::windows::plan::{assign, menu_entries};
        // Both open, under the labels the set gave them.
        let live = |open: &[&str]| {
            assign(&decls(&c), &Default::default())
                .into_iter()
                .filter(|s| open.contains(&s.decl.title.as_str()))
                .map(|s| (s.decl.title, s.label))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let b_label = live(&["B"])["B"].clone();
        let mut created = HashMap::from([(b_label.clone(), HashSet::from([id.clone()]))]);
        let mut hole = HashMap::from([(
            b_label.clone(),
            shell_core::compositing::initial_hole(800.0, 600.0),
        )]);
        // B closes: `FlotillaHost::closed` drops its view state only.
        forget(&mut created, &mut hole, &b_label);
        assert!(created.is_empty() && hole.is_empty());
        assert_eq!(crate::tabs::active_in(&ws, "B"), Some(id.clone()));
        assert_eq!(
            flotilla_core::session::Session::of(&ws).active.get("B"),
            Some(&id)
        );
        // The Window menu, from the set's slots with only A live, still lists B, closed.
        let entries = menu_entries(&assign(&decls(&c), &live(&["A"])));
        let rows: Vec<(&str, bool)> = entries.iter().map(|e| (e.title.as_str(), e.open)).collect();
        assert_eq!(rows, [("A", true), ("B", false)]);
    }
}
