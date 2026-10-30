//! The app menu bar, the Group menu and the sidebar row's context menu. The Group menu acts on the
//! active tab and the row menu on the clicked row; both are built from [`menu_state_for`], so an
//! item is enabled (or disabled, its reason in its title) the same way in each.

use crate::commands::{self, mutate};
use crate::state::AppState;
use crate::MAIN_WINDOW;
use flotilla_core::model::{TabId, Workspace};
use tauri::menu::{
    IsMenuItem, Menu, MenuBuilder, MenuItem, MenuItemBuilder, MenuItemKind, PredefinedMenuItem,
    Submenu, SubmenuBuilder,
};
use tauri::{AppHandle, Emitter, Manager, Wry};

const ACCEL_NEW_GROUP: &str = "Cmd+KeyN";
const ACCEL_CLOSE_GROUP: &str = "Shift+Cmd+Backspace";

/// A menu item's title and whether it can be chosen. A disabled item's title carries its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub enabled: bool,
}

/// The Group menu's items for one tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuState {
    pub home: Item,
    pub make_own_group: Item,
    pub move_to: Item,
    /// The groups `move_to` offers: each other group's head id and name.
    pub move_targets: Vec<(TabId, String)>,
    pub close_tab: Item,
    pub close_group: Item,
    pub recent: Item,
    /// The Recent groups' names, newest first; position is the `recent` index.
    pub recent_entries: Vec<String>,
}

/// What a Group-menu item does. A tab of `None` is the active tab (the menu bar's items); a row
/// menu's items name their row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    NewGroup,
    Home(Option<TabId>),
    MakeOwnGroup(Option<TabId>),
    /// Move the tab to the group headed by the second id.
    MoveTo(Option<TabId>, TabId),
    /// The menu bar's Close Tab is the spine's item, so only a row's carries an `Action`.
    CloseTab(Option<TabId>),
    CloseGroup(Option<TabId>),
    /// Reopen `recent[i]`.
    Reopen(usize),
    ClearRecent,
}

const PREFIX: &str = "flotilla:";

/// The menu item id for `a`: `flotilla:<verb>[:<arg>][@<tab>]`. Tab ids (`t-<n>`) contain neither
/// `:` nor `@`.
pub fn id(a: &Action) -> String {
    let (verb, tab) = match a {
        Action::NewGroup => ("new_group".into(), None),
        Action::Home(t) => ("home".into(), t.as_ref()),
        Action::MakeOwnGroup(t) => ("make_own_group".into(), t.as_ref()),
        Action::MoveTo(t, head) => (format!("move:{head}"), t.as_ref()),
        Action::CloseTab(t) => ("close_tab".into(), t.as_ref()),
        Action::CloseGroup(t) => ("close_group".into(), t.as_ref()),
        Action::Reopen(i) => (format!("recent:{i}"), None),
        Action::ClearRecent => ("clear_recent".into(), None),
    };
    match tab {
        Some(t) => format!("{PREFIX}{verb}@{t}"),
        None => format!("{PREFIX}{verb}"),
    }
}

/// The action a menu item id names; `None` for any other menu's id.
pub fn action(id: &str) -> Option<Action> {
    let rest = id.strip_prefix(PREFIX)?;
    let (verb, tab) = match rest.split_once('@') {
        Some((v, t)) => (v, Some(t.to_string())),
        None => (rest, None),
    };
    let (verb, arg) = match verb.split_once(':') {
        Some((v, a)) => (v, Some(a)),
        None => (verb, None),
    };
    Some(match (verb, arg, tab) {
        ("new_group", None, None) => Action::NewGroup,
        ("home", None, t) => Action::Home(t),
        ("make_own_group", None, t) => Action::MakeOwnGroup(t),
        ("move", Some(head), t) => Action::MoveTo(t, head.to_string()),
        ("close_tab", None, t) => Action::CloseTab(t),
        ("close_group", None, t) => Action::CloseGroup(t),
        ("recent", Some(i), None) => Action::Reopen(i.parse().ok()?),
        ("clear_recent", None, None) => Action::ClearRecent,
        _ => return None,
    })
}

const NO_TAB: &str = "No tab selected";
const IS_HEAD: &str = "Already a group's head";
const NO_OTHER_GROUPS: &str = "No other groups";
const NOTHING_CLOSED: &str = "Nothing closed yet";

fn item(label: &str, blocked: Option<&str>) -> Item {
    match blocked {
        None => Item {
            label: label.into(),
            enabled: true,
        },
        Some(reason) => Item {
            label: format!("{label} — {reason}"),
            enabled: false,
        },
    }
}

fn any_detached(ws: &Workspace) -> bool {
    ws.groups
        .iter()
        .flat_map(|g| std::iter::once(&g.head).chain(g.members.iter()))
        .any(|t| t.detached)
}

/// The Group menu for `tab` (the active tab for the menu bar, the clicked row for its context
/// menu). A tab that is not open counts as none.
pub fn menu_state_for(ws: &Workspace, tab: Option<&TabId>) -> MenuState {
    let found = tab.and_then(|id| ws.find(id));
    let no_tab = found.is_none().then_some(NO_TAB);
    let is_head = found.is_some_and(|(g, t)| g.head.id == t.id);
    let move_targets: Vec<(TabId, String)> = match found {
        Some((own, _)) if !is_head => ws
            .groups
            .iter()
            .filter(|g| g.head.id != own.head.id)
            .map(|g| (g.head.id.clone(), g.head.title.clone()))
            .collect(),
        _ => vec![],
    };
    let move_blocked = no_tab
        .or(is_head.then_some(IS_HEAD))
        .or(move_targets.is_empty().then_some(NO_OTHER_GROUPS));
    MenuState {
        home: item("Home", no_tab),
        make_own_group: item("Make Own Group", no_tab.or(is_head.then_some(IS_HEAD))),
        move_to: item("Move to Group", move_blocked),
        move_targets,
        // The menu bar's Close Tab carries ⌘W, which also docks a focused pop-out; disabled, it
        // would swallow the shortcut there.
        close_tab: item("Close Tab", no_tab.filter(|_| !any_detached(ws))),
        close_group: item("Close Group", no_tab),
        recent: item("Recent", ws.recent.is_empty().then_some(NOTHING_CLOSED)),
        recent_entries: ws.recent.iter().map(|g| g.head.title.clone()).collect(),
    }
}

/// The Group menu's items for `ms`. `tab` is the row a context menu acts on; `None` builds the
/// menu bar's, which act on the active tab, carry the shortcuts, and use the spine's `close_tab`.
fn group_items(
    app: &AppHandle,
    ms: &MenuState,
    tab: Option<&TabId>,
    close_tab: Option<&MenuItem<Wry>>,
) -> tauri::Result<Vec<MenuItemKind<Wry>>> {
    let t = tab.cloned();
    let entry = |a: Action, it: &Item, accel: Option<&str>| -> tauri::Result<MenuItemKind<Wry>> {
        let mut b = MenuItemBuilder::with_id(id(&a), &it.label).enabled(it.enabled);
        if let (None, Some(k)) = (tab, accel) {
            b = b.accelerator(k);
        }
        Ok(MenuItemKind::MenuItem(b.build(app)?))
    };
    let sep = || -> tauri::Result<MenuItemKind<Wry>> {
        Ok(MenuItemKind::Predefined(PredefinedMenuItem::separator(
            app,
        )?))
    };

    let mut moves = SubmenuBuilder::new(app, &ms.move_to.label).enabled(ms.move_to.enabled);
    for (head, name) in &ms.move_targets {
        let a = Action::MoveTo(t.clone(), head.clone());
        moves = moves.item(&MenuItemBuilder::with_id(id(&a), name).build(app)?);
    }
    let mut recent = SubmenuBuilder::new(app, &ms.recent.label).enabled(ms.recent.enabled);
    for (i, name) in ms.recent_entries.iter().enumerate() {
        recent = recent.item(&MenuItemBuilder::with_id(id(&Action::Reopen(i)), name).build(app)?);
    }
    let recent = recent
        .separator()
        .item(&MenuItemBuilder::with_id(id(&Action::ClearRecent), "Clear Recent").build(app)?);

    let close_tab = match close_tab {
        Some(spine) => {
            spine.set_text(&ms.close_tab.label)?;
            spine.set_enabled(ms.close_tab.enabled)?;
            MenuItemKind::MenuItem(spine.clone())
        }
        None => entry(Action::CloseTab(t.clone()), &ms.close_tab, None)?,
    };
    Ok(vec![
        entry(
            Action::NewGroup,
            &Item {
                label: "New Group…".into(),
                enabled: true,
            },
            Some(ACCEL_NEW_GROUP),
        )?,
        entry(Action::Home(t.clone()), &ms.home, None)?,
        sep()?,
        entry(Action::MakeOwnGroup(t.clone()), &ms.make_own_group, None)?,
        MenuItemKind::Submenu(moves.build()?),
        sep()?,
        close_tab,
        entry(
            Action::CloseGroup(t.clone()),
            &ms.close_group,
            Some(ACCEL_CLOSE_GROUP),
        )?,
        sep()?,
        MenuItemKind::Submenu(recent.build()?),
    ])
}

fn refs(items: &[MenuItemKind<Wry>]) -> Vec<&dyn IsMenuItem<Wry>> {
    items.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect()
}

/// The context menu for sidebar row `tab`.
pub fn row_menu(app: &AppHandle, ms: &MenuState, tab: &TabId) -> tauri::Result<Menu<Wry>> {
    let items = group_items(app, ms, Some(tab), None)?;
    MenuBuilder::new(app).items(&refs(&items)).build()
}

/// The menu bar: the shell-core spine's App, Config and Window menus around flotilla's Edit, Tab
/// and Group menus, the Group menu acting on the active tab.
fn build_app_menu(app: &AppHandle, ms: &MenuState) -> tauri::Result<Menu<Wry>> {
    let path = crate::config::config_path();
    let (title, colour, cycle_digits) = {
        let cfg = app.state::<AppState>().config.lock().unwrap().clone();
        (
            cfg.window_title(),
            cfg.accent(),
            cfg.tab_digit_keys.is_cycle(),
        )
    };
    let windows = [shell_core::menu::WindowEntry {
        id: MAIN_WINDOW.into(),
        title,
        open: true,
        colour,
    }];
    let spine = shell_core::menu::build_spine(
        app,
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
    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let nav = shell_core::menu::build_tab_nav(app, cycle_digits)?;
    let nav_refs: Vec<&dyn IsMenuItem<Wry>> =
        nav.nav.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect();
    let jump_refs: Vec<&dyn IsMenuItem<Wry>> = nav
        .jumps
        .iter()
        .map(|i| i as &dyn IsMenuItem<Wry>)
        .collect();
    let tabs_menu = SubmenuBuilder::new(app, "Tab")
        .items(&nav_refs)
        .separator()
        .item(&spine.close_tab)
        .item(&spine.pop_out_tab)
        .item(&spine.find_in_sidebar)
        .separator()
        .items(&jump_refs)
        .build()?;

    let group = group_items(app, ms, None, Some(&spine.close_tab))?;
    let group_menu: Submenu<Wry> = SubmenuBuilder::new(app, "Group")
        .items(&refs(&group))
        .build()?;

    MenuBuilder::new(app)
        .items(&[
            &spine.submenus[0], // App
            &edit_menu,
            &tabs_menu,
            &group_menu,
            &spine.submenus[1], // Config
            &spine.submenus[2], // Window
        ])
        .build()
}

/// Whether the menu bar must be rebuilt to show `next`; records `next` as built when so. Every DTO
/// push asks, retitles included, and most change nothing the menu shows.
pub fn needs_rebuild(last: &mut Option<MenuState>, next: MenuState) -> bool {
    if last.as_ref() == Some(&next) {
        return false;
    }
    *last = Some(next);
    true
}

/// Rebuilds the menu bar from the workspace as it is now, if that changes it.
pub fn refresh(app: &AppHandle) {
    let state = app.state::<AppState>();
    let ms = {
        let ws = state.ws.lock().unwrap();
        menu_state_for(&ws, ws.active.as_ref())
    };
    if !needs_rebuild(&mut state.menu.lock().unwrap(), ms.clone()) {
        return;
    }
    match build_app_menu(app, &ms) {
        Ok(menu) => {
            let _ = app.set_menu(menu);
        }
        // Not built: the next push tries again.
        Err(_) => *state.menu.lock().unwrap() = None,
    }
}

/// Runs the Group-menu or row-menu item `menu_id`; `false` if it is not one. Main thread.
pub fn handle(app: &AppHandle, menu_id: &str) -> bool {
    let Some(a) = action(menu_id) else {
        return false;
    };
    let pick = |t: Option<TabId>, ws: &Workspace| t.or_else(|| ws.active.clone());
    match a {
        Action::NewGroup => {
            // The key focus may be in a content webview; the field is in the chrome's.
            if let Some(chrome) = app.get_webview(MAIN_WINDOW) {
                let _ = chrome.set_focus();
            }
            let _ = app.emit_to(MAIN_WINDOW, "focus-new-group", ());
        }
        Action::Home(t) => {
            let t = pick(t, &app.state::<AppState>().ws.lock().unwrap());
            if let Some(t) = t {
                if let Err(e) = commands::home(app, &t) {
                    crate::tabs::notice(app, e);
                }
            }
        }
        Action::MakeOwnGroup(t) => mutate(app, |ws| {
            if let Some(t) = pick(t, ws) {
                ws.make_own_group(&t);
            }
        }),
        Action::MoveTo(t, head) => mutate(app, |ws| {
            if let Some(t) = pick(t, ws) {
                ws.move_to_group(&t, &head);
            }
        }),
        Action::CloseTab(t) => mutate(app, |ws| {
            if let Some(t) = pick(t, ws) {
                ws.close_tab(&t);
            }
        }),
        Action::CloseGroup(t) => mutate(app, |ws| {
            let head = pick(t, ws)
                .and_then(|t| ws.group_of(&t))
                .map(|g| g.head.id.clone());
            if let Some(h) = head {
                ws.close_group(&h);
            }
        }),
        Action::Reopen(i) => mutate(app, |ws| {
            ws.reopen_recent(i);
        }),
        Action::ClearRecent => mutate(app, |ws| ws.recent.clear()),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_core::model::Opened;
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    /// One group, "Auction" (head h) with a member m.
    fn one_group() -> (Workspace, TabId, TabId) {
        let mut ws = Workspace::default();
        let Opened::Added(h) =
            ws.new_group(&url("https://drive.google.com/drive/folders/f1"), "default")
        else {
            panic!()
        };
        ws.set_title(&h, "Auction");
        let Some(Opened::Added(m)) = ws.open_from(
            &h,
            &url("https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit"),
        ) else {
            panic!()
        };
        (ws, h, m)
    }

    fn add_group(ws: &mut Workspace, folder: &str, name: &str) -> TabId {
        let Opened::Added(h) = ws.new_group(
            &url(&format!("https://drive.google.com/drive/folders/{folder}")),
            "default",
        ) else {
            panic!()
        };
        ws.set_title(&h, name);
        h
    }

    fn on(label: &str) -> Item {
        Item {
            label: label.into(),
            enabled: true,
        }
    }

    fn off(label: &str) -> Item {
        Item {
            label: label.into(),
            enabled: false,
        }
    }

    #[test]
    fn action_ids_round_trip() {
        let t = || Some("t-3".to_string());
        for a in [
            Action::NewGroup,
            Action::Home(None),
            Action::Home(t()),
            Action::MakeOwnGroup(None),
            Action::MakeOwnGroup(t()),
            Action::MoveTo(None, "t-1".into()),
            Action::MoveTo(t(), "t-1".into()),
            Action::CloseTab(t()),
            Action::CloseGroup(None),
            Action::CloseGroup(t()),
            Action::Reopen(2),
            Action::ClearRecent,
        ] {
            assert_eq!(action(&id(&a)), Some(a.clone()), "{}", id(&a));
        }
        assert_eq!(id(&Action::Home(t())), "flotilla:home@t-3");
        assert_eq!(action("shell:close_tab"), None);
        assert_eq!(action("flotilla:recent:x"), None);
        assert_eq!(action("flotilla:nope"), None);
    }

    #[test]
    fn a_head_cannot_become_its_own_group_or_move() {
        let (mut ws, h, _) = one_group();
        add_group(&mut ws, "f2", "Website");
        let s = menu_state_for(&ws, Some(&h));
        assert_eq!(s.home, on("Home"));
        assert_eq!(
            s.make_own_group,
            off("Make Own Group — Already a group's head")
        );
        assert_eq!(s.move_to, off("Move to Group — Already a group's head"));
        assert!(s.move_targets.is_empty());
        assert_eq!(s.close_tab, on("Close Tab"));
        assert_eq!(s.close_group, on("Close Group"));
    }

    #[test]
    fn a_member_can_leave_for_any_other_group() {
        let (mut ws, h, m) = one_group();
        let w = add_group(&mut ws, "f2", "Website");
        let x = add_group(&mut ws, "f3", "Grants");
        let s = menu_state_for(&ws, Some(&m));
        assert_eq!(s.make_own_group, on("Make Own Group"));
        assert_eq!(s.move_to, on("Move to Group"));
        // Its own group is not a destination.
        assert_eq!(
            s.move_targets,
            [(w, "Website".to_string()), (x, "Grants".to_string())]
        );
        assert!(!s.move_targets.iter().any(|(id, _)| id == &h));
    }

    #[test]
    fn a_single_group_offers_nowhere_to_move() {
        let (ws, _, m) = one_group();
        let s = menu_state_for(&ws, Some(&m));
        assert_eq!(s.make_own_group, on("Make Own Group"));
        assert_eq!(s.move_to, off("Move to Group — No other groups"));
        assert!(s.move_targets.is_empty());
    }

    #[test]
    fn recent_lists_closed_groups_newest_first() {
        let (mut ws, h, _) = one_group();
        let s = menu_state_for(&ws, Some(&h));
        assert_eq!(s.recent, off("Recent — Nothing closed yet"));
        assert!(s.recent_entries.is_empty());

        let w = add_group(&mut ws, "f2", "Website");
        ws.close_group(&h);
        ws.close_group(&w);
        let s = menu_state_for(&ws, None);
        assert_eq!(s.recent, on("Recent"));
        assert_eq!(s.recent_entries, ["Website", "Auction"]);
    }

    #[test]
    fn no_tab_disables_every_tab_action_but_keeps_recent() {
        let (mut ws, h, _) = one_group();
        ws.close_group(&h);
        for s in [
            menu_state_for(&ws, None),
            menu_state_for(&ws, Some(&"t-99".to_string())),
        ] {
            assert_eq!(s.home, off("Home — No tab selected"));
            assert_eq!(s.make_own_group, off("Make Own Group — No tab selected"));
            assert_eq!(s.move_to, off("Move to Group — No tab selected"));
            assert_eq!(s.close_tab, off("Close Tab — No tab selected"));
            assert_eq!(s.close_group, off("Close Group — No tab selected"));
            assert_eq!(s.recent, on("Recent"));
        }
    }

    /// A disabled NSMenuItem swallows its shortcut, and ⌘W in a pop-out docks it whatever the main
    /// window shows.
    #[test]
    fn close_tab_stays_enabled_with_no_active_tab_while_a_tab_is_popped_out() {
        let (mut ws, _, m) = one_group();
        ws.set_detached(&m, true);
        ws.active = None;
        let s = menu_state_for(&ws, None);
        assert_eq!(s.close_tab, on("Close Tab"));
        assert_eq!(s.home, off("Home — No tab selected"));
        ws.set_detached(&m, false);
        assert_eq!(
            menu_state_for(&ws, None).close_tab,
            off("Close Tab — No tab selected")
        );
    }

    #[test]
    fn the_menu_bar_is_rebuilt_only_when_its_state_changes() {
        let (mut ws, h, m) = one_group();
        let mut last = None;
        assert!(needs_rebuild(&mut last, menu_state_for(&ws, Some(&h))));
        // A push that changes nothing the menu shows (a member's retitle) rebuilds nothing.
        ws.set_title(&m, "Budget");
        assert!(!needs_rebuild(&mut last, menu_state_for(&ws, Some(&h))));
        // Selecting the member enables Make Own Group: rebuilt.
        assert!(needs_rebuild(&mut last, menu_state_for(&ws, Some(&m))));
        assert_eq!(last, Some(menu_state_for(&ws, Some(&m))));
    }
}
