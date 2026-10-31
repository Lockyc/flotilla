//! The app menu bar, the Group menu and the sidebar row's context menu. The Group menu acts on the
//! active tab and the row menu on the clicked row; both are built from [`menu_state_for`], so an
//! item is enabled (or disabled, its reason in its title) the same way in each.

use crate::commands::{self, mutate};
use crate::state::AppState;
use flotilla_core::model::{SetHomeBlock, TabId, Workspace};
use shell_core::menu::WindowEntry;
use shell_core::windows::WindowSet;
use std::collections::HashSet;
use tauri::menu::{
    IsMenuItem, Menu, MenuBuilder, MenuItem, MenuItemBuilder, MenuItemKind, PredefinedMenuItem,
    Submenu, SubmenuBuilder,
};
use tauri::{AppHandle, Emitter, Manager, Wry};
use url::Url;

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
    pub rename: Item,
    pub set_home: Item,
    pub make_own_group: Item,
    pub move_to: Item,
    /// The groups `move_to` offers: each other group's head id and name.
    pub move_targets: Vec<(TabId, String)>,
    /// The menu bar's ⌘W.
    pub unload_tab: Item,
    pub remove_tab: Item,
    pub unload_group: Item,
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
    /// Edit the row's name in place in its sidebar.
    Rename(Option<TabId>),
    /// Make the tab's current page its home.
    SetHome(Option<TabId>),
    MakeOwnGroup(Option<TabId>),
    /// Move the tab to the group headed by the second id.
    MoveTo(Option<TabId>, TabId),
    RemoveTab(Option<TabId>),
    /// Unload every loaded tab of the tab's group, its head included.
    UnloadGroup(Option<TabId>),
    CloseGroup(Option<TabId>),
    /// Reopen `recent[i]`.
    Reopen(usize),
    ClearRecent,
    /// Add to group…'s item: open Incoming item `.0` into the group headed by `.1`.
    AddIncoming(String, TabId),
}

const PREFIX: &str = "flotilla:";

/// The menu item id for `a`: `flotilla:<verb>[:<arg>][@<tab>]`. Tab ids (`t-<n>`) and Incoming
/// item ids (`in-<n>`, in the tab's place) contain neither `:` nor `@`.
pub fn id(a: &Action) -> String {
    let (verb, tab) = match a {
        Action::AddIncoming(item, head) => (format!("add_incoming:{head}"), Some(item)),
        Action::NewGroup => ("new_group".into(), None),
        Action::Home(t) => ("home".into(), t.as_ref()),
        Action::Rename(t) => ("rename".into(), t.as_ref()),
        Action::SetHome(t) => ("set_home".into(), t.as_ref()),
        Action::MakeOwnGroup(t) => ("make_own_group".into(), t.as_ref()),
        Action::MoveTo(t, head) => (format!("move:{head}"), t.as_ref()),
        Action::RemoveTab(t) => ("remove_tab".into(), t.as_ref()),
        Action::UnloadGroup(t) => ("unload_group".into(), t.as_ref()),
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
        ("rename", None, t) => Action::Rename(t),
        ("set_home", None, t) => Action::SetHome(t),
        ("make_own_group", None, t) => Action::MakeOwnGroup(t),
        ("move", Some(head), t) => Action::MoveTo(t, head.to_string()),
        ("remove_tab", None, t) => Action::RemoveTab(t),
        ("unload_group", None, t) => Action::UnloadGroup(t),
        ("close_group", None, t) => Action::CloseGroup(t),
        ("recent", Some(i), None) => Action::Reopen(i.parse().ok()?),
        ("clear_recent", None, None) => Action::ClearRecent,
        ("add_incoming", Some(head), Some(item)) => Action::AddIncoming(item, head.to_string()),
        _ => return None,
    })
}

const NO_TAB: &str = "No tab selected";
const IS_HEAD: &str = "Already a group's head";
const NO_OTHER_GROUPS: &str = "No other groups";
const NOTHING_CLOSED: &str = "Nothing closed yet";
const NOTHING_LOADED: &str = "Nothing loaded";

/// The tabs of `id`'s group (head first) that Unload Group closes: those with a webview in
/// `live`, a popped-out tab's excepted, since its window shows it.
pub fn group_loaded(ws: &Workspace, live: &HashSet<TabId>, id: &TabId) -> Vec<TabId> {
    ws.group_of(id)
        .map(|g| {
            std::iter::once(&g.head)
                .chain(&g.members)
                .filter(|t| !t.detached && live.contains(&t.id))
                .map(|t| t.id.clone())
                .collect()
        })
        .unwrap_or_default()
}

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
    ws.tabs().any(|t| t.detached)
}

/// Why Set Home to Current Page is unavailable, in the menu's words.
fn set_home_reason(block: SetHomeBlock) -> &'static str {
    match block {
        SetHomeBlock::AtHome => "Already at its home",
        SetHomeBlock::NotWeb => "Not a web page",
        SetHomeBlock::OpenElsewhere => "Open in another tab",
    }
}

/// Set Home's refusal, for the error bar when the page changed after the menu was built.
pub fn set_home_refusal(block: SetHomeBlock) -> String {
    format!(
        "Can't set this tab's home: {}.",
        set_home_reason(block).to_lowercase()
    )
}

/// The Group menu for `tab` (the focused window's active tab for the menu bar, the clicked row for
/// its context menu), whose webview shows `current` (`None`: nothing loaded). A tab that is not
/// open counts as none. `live` is every tab with a webview. A member moves to any other group: its
/// own window's by name, another window's as "name — window".
pub fn menu_state_for(
    ws: &Workspace,
    live: &HashSet<TabId>,
    current: Option<&Url>,
    tab: Option<&TabId>,
) -> MenuState {
    let found = tab.and_then(|id| ws.find(id));
    let no_tab = found.is_none().then_some(NO_TAB);
    let set_home_blocked = no_tab.or_else(|| {
        tab.and_then(|id| ws.set_home_blocked(id, current))
            .map(set_home_reason)
    });
    let is_head = found.is_some_and(|(_, g, t)| g.head.id == t.id);
    let move_targets: Vec<(TabId, String)> = match found {
        Some((wi, own, _)) if !is_head => ws
            .windows
            .iter()
            .enumerate()
            .flat_map(|(i, w)| w.groups.iter().map(move |g| (i, w, g)))
            .filter(|(_, _, g)| g.head.id != own.head.id)
            .map(|(i, w, g)| {
                let name = if i == wi {
                    g.head.title.clone()
                } else {
                    format!("{} — {}", g.head.title, w.title)
                };
                (g.head.id.clone(), name)
            })
            .collect(),
        _ => vec![],
    };
    let move_blocked = no_tab
        .or(is_head.then_some(IS_HEAD))
        .or(move_targets.is_empty().then_some(NO_OTHER_GROUPS));
    MenuState {
        home: item("Home", no_tab),
        rename: item("Rename…", no_tab),
        set_home: item("Set Home to Current Page", set_home_blocked),
        make_own_group: item("Make Own Group", no_tab.or(is_head.then_some(IS_HEAD))),
        move_to: item("Move to Group", move_blocked),
        move_targets,
        // ⌘W also docks a focused pop-out; disabled, it would swallow the shortcut there.
        unload_tab: item("Unload Tab", no_tab.filter(|_| !any_detached(ws))),
        remove_tab: item("Remove Tab", no_tab),
        unload_group: item(
            "Unload Group",
            no_tab.or_else(|| {
                tab.filter(|id| group_loaded(ws, live, id).is_empty())
                    .map(|_| NOTHING_LOADED)
            }),
        ),
        close_group: item("Close Group", no_tab),
        recent: item("Recent", ws.recent.is_empty().then_some(NOTHING_CLOSED)),
        recent_entries: ws.recent.iter().map(|g| g.head.title.clone()).collect(),
    }
}

/// The Group menu's items for `ms`. `tab` is the row a context menu acts on; `None` builds the
/// menu bar's, which act on the active tab, carry the shortcuts, and lead the tab items with the
/// spine's ⌘W item, `unload`.
fn group_items(
    app: &AppHandle,
    ms: &MenuState,
    tab: Option<&TabId>,
    unload: Option<&MenuItem<Wry>>,
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

    let mut tab_items = vec![];
    if let Some(spine) = unload {
        spine.set_text(&ms.unload_tab.label)?;
        spine.set_enabled(ms.unload_tab.enabled)?;
        tab_items.push(MenuItemKind::MenuItem(spine.clone()));
    }
    tab_items.push(entry(Action::RemoveTab(t.clone()), &ms.remove_tab, None)?);
    tab_items.push(entry(
        Action::UnloadGroup(t.clone()),
        &ms.unload_group,
        None,
    )?);
    tab_items.push(entry(
        Action::CloseGroup(t.clone()),
        &ms.close_group,
        Some(ACCEL_CLOSE_GROUP),
    )?);
    let mut items = vec![
        entry(
            Action::NewGroup,
            &Item {
                label: "New Group…".into(),
                enabled: true,
            },
            Some(ACCEL_NEW_GROUP),
        )?,
        entry(Action::Home(t.clone()), &ms.home, None)?,
        entry(Action::Rename(t.clone()), &ms.rename, None)?,
        entry(Action::SetHome(t.clone()), &ms.set_home, None)?,
        sep()?,
        entry(Action::MakeOwnGroup(t.clone()), &ms.make_own_group, None)?,
        MenuItemKind::Submenu(moves.build()?),
        sep()?,
    ];
    items.extend(tab_items);
    items.extend([sep()?, MenuItemKind::Submenu(recent.build()?)]);
    Ok(items)
}

fn refs(items: &[MenuItemKind<Wry>]) -> Vec<&dyn IsMenuItem<Wry>> {
    items.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect()
}

/// The context menu for sidebar row `tab`.
pub fn row_menu(app: &AppHandle, ms: &MenuState, tab: &TabId) -> tauri::Result<Menu<Wry>> {
    let items = group_items(app, ms, Some(tab), None)?;
    MenuBuilder::new(app).items(&refs(&items)).build()
}

/// An Incoming row's Add to group… menu for item `item`: each of its window's `groups` (head id,
/// name), in sidebar order.
pub fn incoming_menu(
    app: &AppHandle,
    item: &str,
    groups: &[(TabId, String)],
) -> tauri::Result<Menu<Wry>> {
    let mut menu = MenuBuilder::new(app);
    for (head, name) in groups {
        let a = Action::AddIncoming(item.to_string(), head.clone());
        menu = menu.item(&MenuItemBuilder::with_id(id(&a), name).build(app)?);
    }
    menu.build()
}

/// What the menu bar is built from: the Group menu's state and the Window menu's rows (label,
/// title, open).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuKey {
    pub group: MenuState,
    pub windows: Vec<(String, String, bool)>,
}

/// The menu bar: shell-core's App, Edit, Tab, Config and Window menus (every configured window, open
/// or closed) around flotilla's Group menu, which acts on the focused window's active tab.
fn build_app_menu(
    app: &AppHandle,
    ms: &MenuState,
    windows: &[WindowEntry],
) -> tauri::Result<Menu<Wry>> {
    let cycle_digits = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap()
        .tab_digit_keys
        .is_cycle();
    let spine = shell_core::menu::build_spine(
        app,
        shell_core::menu::SpineConfig {
            app_name: "flotilla",
            windows,
        },
        env!("CARGO_PKG_VERSION"),
        env!("BUILD_GIT_SHA"),
        env!("BUILD_DATE"),
    )?;
    let tabs_menu = shell_core::menu::build_tab_menu(app, &spine, cycle_digits, &[])?;

    let group = group_items(app, ms, None, Some(&spine.close_tab))?;
    let group_menu: Submenu<Wry> = SubmenuBuilder::new(app, "Group")
        .items(&refs(&group))
        .build()?;

    MenuBuilder::new(app)
        .items(&[
            &spine.submenus[0], // App
            &spine.edit,
            &tabs_menu,
            &group_menu,
            &spine.submenus[1], // Config
            &spine.submenus[2], // Window
        ])
        .build()
}

/// Whether the menu bar must be rebuilt to show `next`; records `next` as built when so. Every DTO
/// push asks, retitles included, and most change nothing the menu shows.
pub fn needs_rebuild<T: PartialEq>(last: &mut Option<T>, next: T) -> bool {
    if last.as_ref() == Some(&next) {
        return false;
    }
    *last = Some(next);
    true
}

/// Forgets what the menu bar was built from, so the next [`refresh`] rebuilds it.
pub fn invalidate(app: &AppHandle) {
    *app.state::<AppState>().menu.lock().unwrap() = None;
}

/// The workspace window the menu bar acts on, as (label, title): the focused window, or for a
/// focused pop-out the window its tab is in, else the first open window.
pub fn focused(app: &AppHandle) -> Option<(String, String)> {
    let open = crate::tabs::open_windows(app);
    let focused = app.get_focused_window().map(|w| w.label().to_string());
    let from_popout = focused
        .as_deref()
        .and_then(crate::detach::tab_of)
        .and_then(|id| {
            crate::tabs::window_title_of(&app.state::<AppState>().ws.lock().unwrap(), &id)
        });
    open.iter()
        .find(|(l, t)| Some(l) == focused.as_ref() || Some(t) == from_popout.as_ref())
        .or(open.first())
        .cloned()
}

/// Rebuilds the menu bar whenever `window` gains focus, so its Group menu acts on that window's
/// active tab ([`refresh`] rebuilds only when that changes what it shows).
pub fn follow_focus(app: &AppHandle, window: &tauri::Window) {
    let app = app.clone();
    window.on_window_event(move |e| {
        if let tauri::WindowEvent::Focused(true) = e {
            refresh(&app);
        }
    });
}

/// The tab the Group menu acts on: a focused pop-out's own tab (never its window's `active`),
/// else the active tab of the workspace window titled `title`. `focused` is the focused OS
/// window's label.
pub fn menu_tab(ws: &Workspace, focused: Option<&str>, title: Option<&str>) -> Option<TabId> {
    focused
        .and_then(crate::detach::tab_of)
        .filter(|id| ws.find(id).is_some())
        .or_else(|| title.and_then(|t| crate::tabs::active_in(ws, t)))
}

/// The label of the focused OS window, a pop-out's included.
fn focused_label(app: &AppHandle) -> Option<String> {
    app.get_focused_window().map(|w| w.label().to_string())
}

/// What the menu bar shows with `tab` the one it acts on ([`menu_tab`]): its Group menu, and the
/// Window menu's rows. `current` is the page `tab` shows.
pub fn key_for(
    ws: &Workspace,
    live: &HashSet<TabId>,
    tab: Option<&TabId>,
    current: Option<&Url>,
    windows: &[WindowEntry],
) -> MenuKey {
    MenuKey {
        group: menu_state_for(ws, live, current, tab),
        windows: windows
            .iter()
            .map(|e| (e.id.clone(), e.title.clone(), e.open))
            .collect(),
    }
}

/// Rebuilds the menu bar from the workspace and the window set as they are now, if that changes
/// it. Runs on every DTO push and whenever a window or pop-out gains focus.
pub fn refresh(app: &AppHandle) {
    let state = app.state::<AppState>();
    let windows = app.state::<WindowSet>().entries();
    let title = focused(app).map(|(_, t)| t);
    let label = focused_label(app);
    let tab = menu_tab(
        &state.ws.lock().unwrap(),
        label.as_deref(),
        title.as_deref(),
    );
    // The page is read with no lock held: a webview call.
    let current = tab
        .as_ref()
        .and_then(|id| crate::tabs::current_url(app, id));
    let live = crate::tabs::live(app);
    let key = key_for(
        &state.ws.lock().unwrap(),
        &live,
        tab.as_ref(),
        current.as_ref(),
        &windows,
    );
    let group = key.group.clone();
    if !needs_rebuild(&mut state.menu.lock().unwrap(), key) {
        return;
    }
    match build_app_menu(app, &group, &windows) {
        Ok(menu) => {
            let _ = app.set_menu(menu);
        }
        // Not built: the next push tries again.
        Err(_) => invalidate(app),
    }
}

/// Runs the Group-menu or row-menu item `menu_id`; `false` if it is not one. Main thread.
pub fn handle(app: &AppHandle, menu_id: &str) -> bool {
    let Some(a) = action(menu_id) else {
        return false;
    };
    let window = focused(app);
    let title = window.as_ref().map(|(_, t)| t.clone());
    let label = focused_label(app);
    let pick = |t: Option<TabId>, ws: &Workspace| {
        t.or_else(|| menu_tab(ws, label.as_deref(), title.as_deref()))
    };
    match a {
        Action::NewGroup => {
            // The key focus may be in a content webview; the field is in the chrome's.
            if let Some((label, _)) = &window {
                if let Some(chrome) = app.get_webview(label) {
                    let _ = chrome.set_focus();
                }
                let _ = app.emit_to(label.as_str(), "focus-new-group", ());
            }
        }
        Action::Home(t) => {
            let t = pick(t, &app.state::<AppState>().ws.lock().unwrap());
            if let Some(t) = t {
                if let Err(e) = commands::home(app, &t) {
                    crate::tabs::notice(app, e);
                }
            }
        }
        Action::Rename(t) => {
            let t = pick(t, &app.state::<AppState>().ws.lock().unwrap());
            if let Some(t) = t {
                commands::begin_rename(app, &t);
            }
        }
        Action::SetHome(t) => {
            let t = pick(t, &app.state::<AppState>().ws.lock().unwrap());
            if let Some(t) = t {
                if let Err(e) = commands::set_home(app, &t) {
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
        Action::RemoveTab(t) => mutate(app, |ws| {
            if let Some(t) = pick(t, ws) {
                ws.close_tab(&t);
            }
        }),
        Action::UnloadGroup(t) => {
            let t = pick(t, &app.state::<AppState>().ws.lock().unwrap());
            if let Some(t) = t {
                commands::unload_group(app, &t);
            }
        }
        Action::CloseGroup(t) => mutate(app, |ws| {
            let head = pick(t, ws)
                .and_then(|t| ws.group_of(&t))
                .map(|g| g.head.id.clone());
            if let Some(h) = head {
                ws.close_group(&h);
            }
        }),
        Action::Reopen(i) => {
            if let Some(window) = &title {
                commands::show_from(app, window, true, |ws| ws.reopen_recent(window, i));
            }
        }
        Action::ClearRecent => mutate(app, |ws| ws.recent.clear()),
        Action::AddIncoming(item, head) => {
            let window =
                crate::tabs::window_title_of(&app.state::<AppState>().ws.lock().unwrap(), &head);
            if let Some(window) = window {
                commands::show_from(app, &window, true, |ws| {
                    ws.add_incoming(&item, &head)
                        .map(flotilla_core::model::opened_id)
                });
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::testing::{new_group, one_window};
    use flotilla_core::model::Opened;
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    /// One group, "Auction" (head h) with a member m.
    fn one_group() -> (Workspace, TabId, TabId) {
        let mut ws = one_window();
        let Opened::Added(h) = new_group(&mut ws, "https://drive.google.com/drive/folders/f1")
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
        let Opened::Added(h) = new_group(
            ws,
            &format!("https://drive.google.com/drive/folders/{folder}"),
        ) else {
            panic!()
        };
        ws.set_title(&h, name);
        h
    }

    /// No tab has a webview.
    fn cold() -> HashSet<TabId> {
        HashSet::new()
    }

    #[test]
    fn unload_group_needs_a_loaded_tab_in_the_group_and_skips_pop_outs() {
        let (mut ws, h, m) = one_group();
        let w = add_group(&mut ws, "f2", "Website");
        let live = |ids: &[&TabId]| ids.iter().map(|&i| i.clone()).collect::<HashSet<_>>();
        // Only another group is loaded: nothing here to unload, from head or member.
        for tab in [&h, &m] {
            assert_eq!(
                menu_state_for(&ws, &live(&[&w]), None, Some(tab)).unload_group,
                off("Unload Group — Nothing loaded")
            );
        }
        // A loaded member offers it on the head's row too, and both go.
        let both = live(&[&h, &m, &w]);
        assert_eq!(
            menu_state_for(&ws, &both, None, Some(&h)).unload_group,
            on("Unload Group")
        );
        assert_eq!(group_loaded(&ws, &both, &m), [h.clone(), m.clone()]);
        // A popped-out member stays in its window.
        ws.set_detached(&m, true);
        assert_eq!(group_loaded(&ws, &both, &h), [h.clone()]);
        assert_eq!(
            menu_state_for(&ws, &live(&[&m]), None, Some(&h)).unload_group,
            off("Unload Group — Nothing loaded")
        );
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
            Action::RemoveTab(t()),
            Action::UnloadGroup(None),
            Action::UnloadGroup(t()),
            Action::CloseGroup(None),
            Action::CloseGroup(t()),
            Action::Reopen(2),
            Action::ClearRecent,
            Action::AddIncoming("in-00ff".into(), "t-1".into()),
        ] {
            assert_eq!(action(&id(&a)), Some(a.clone()), "{}", id(&a));
        }
        assert_eq!(id(&Action::Home(t())), "flotilla:home@t-3");
        assert_eq!(
            id(&Action::AddIncoming("in-00ff".into(), "t-1".into())),
            "flotilla:add_incoming:t-1@in-00ff"
        );
        assert_eq!(action("flotilla:add_incoming:t-1"), None);
        assert_eq!(action("shell:close_tab"), None);
        assert_eq!(action("flotilla:recent:x"), None);
        assert_eq!(action("flotilla:nope"), None);
    }

    #[test]
    fn a_head_cannot_become_its_own_group_or_move() {
        let (mut ws, h, _) = one_group();
        add_group(&mut ws, "f2", "Website");
        let s = menu_state_for(&ws, &cold(), None, Some(&h));
        assert_eq!(s.home, on("Home"));
        assert_eq!(
            s.make_own_group,
            off("Make Own Group — Already a group's head")
        );
        assert_eq!(s.move_to, off("Move to Group — Already a group's head"));
        assert!(s.move_targets.is_empty());
        assert_eq!(s.remove_tab, on("Remove Tab"));
        assert_eq!(s.close_group, on("Close Group"));
    }

    #[test]
    fn a_member_can_leave_for_any_other_group() {
        let (mut ws, h, m) = one_group();
        let w = add_group(&mut ws, "f2", "Website");
        let x = add_group(&mut ws, "f3", "Grants");
        let s = menu_state_for(&ws, &cold(), None, Some(&m));
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
        let s = menu_state_for(&ws, &cold(), None, Some(&m));
        assert_eq!(s.make_own_group, on("Make Own Group"));
        assert_eq!(s.move_to, off("Move to Group — No other groups"));
        assert!(s.move_targets.is_empty());
    }

    #[test]
    fn a_member_moves_to_another_windows_group_named_with_its_window() {
        let window = |t: &str| flotilla_core::model::ConfigWindow {
            title: t.into(),
            groups: vec![],
        };
        let mut ws = Workspace::from_config(&[window("A"), window("B")]);
        let folder = |n| url(&format!("https://drive.google.com/drive/folders/f{n}"));
        let Some(Opened::Added(h)) = ws.new_group("A", &folder(1), "default") else {
            panic!()
        };
        ws.set_title(&h, "Auction");
        let Some(Opened::Added(m)) = ws.open_from(
            &h,
            &url("https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit"),
        ) else {
            panic!()
        };
        let Some(Opened::Added(b)) = ws.new_group("B", &folder(2), "default") else {
            panic!()
        };
        ws.set_title(&b, "Newsletters");
        let s = menu_state_for(&ws, &cold(), None, Some(&m));
        assert_eq!(s.move_targets, vec![(b, "Newsletters — B".to_string())]);
        assert_eq!(s.move_to, on("Move to Group"));
    }

    const DOC1: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit";

    #[test]
    fn rename_is_offered_for_heads_and_documents() {
        let (ws, h, m) = one_group();
        assert_eq!(
            menu_state_for(&ws, &cold(), None, Some(&h)).rename,
            on("Rename…")
        );
        assert_eq!(
            menu_state_for(&ws, &cold(), None, Some(&m)).rename,
            on("Rename…")
        );
        assert_eq!(
            menu_state_for(&ws, &cold(), None, None).rename,
            off("Rename… — No tab selected")
        );
    }

    #[test]
    fn set_home_disabled_at_home() {
        let (ws, h, m) = one_group();
        // Not loaded, or on the page it opened with: already home.
        let loaded = url(&format!("{DOC1}?tab=t.0#heading=h.x"));
        for (tab, current) in [
            (&m, None),
            (&m, Some(url(DOC1))),
            (&m, Some(loaded)),
            (&h, None),
        ] {
            assert_eq!(
                menu_state_for(&ws, &cold(), current.as_ref(), Some(tab)).set_home,
                off("Set Home to Current Page — Already at its home")
            );
        }
        // Browsed into a subfolder: the head can make it home.
        let sub = url("https://drive.google.com/drive/folders/sub");
        assert_eq!(
            menu_state_for(&ws, &cold(), Some(&sub), Some(&h)).set_home,
            on("Set Home to Current Page")
        );
    }

    #[test]
    fn set_home_refuses_non_web_urls() {
        let (ws, h, _) = one_group();
        for page in ["about:blank", "file:///etc/hosts", "data:text/html,x"] {
            assert_eq!(
                menu_state_for(&ws, &cold(), Some(&url(page)), Some(&h)).set_home,
                off("Set Home to Current Page — Not a web page"),
                "{page}"
            );
        }
    }

    #[test]
    fn set_home_refuses_a_doc_open_elsewhere() {
        let (mut ws, h, m) = one_group();
        add_group(&mut ws, "f2", "Website");
        // The head browsed to the member's document: it is open in another tab.
        assert_eq!(
            menu_state_for(&ws, &cold(), Some(&url(DOC1)), Some(&h)).set_home,
            off("Set Home to Current Page — Open in another tab")
        );
        // A group head's folder, likewise.
        let f2 = url("https://drive.google.com/drive/folders/f2");
        assert_eq!(
            menu_state_for(&ws, &cold(), Some(&f2), Some(&m)).set_home,
            off("Set Home to Current Page — Open in another tab")
        );
    }

    #[test]
    fn the_group_menu_follows_the_focused_window() {
        let window = |t: &str| flotilla_core::model::ConfigWindow {
            title: t.into(),
            groups: vec![],
        };
        let mut ws = Workspace::from_config(&[window("A"), window("B")]);
        let folder = |n| url(&format!("https://drive.google.com/drive/folders/f{n}"));
        let Some(Opened::Added(a)) = ws.new_group("A", &folder(1), "default") else {
            panic!()
        };
        let Some(Opened::Added(m)) = ws.open_from(
            &a,
            &url("https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit"),
        ) else {
            panic!()
        };
        let entries = [
            WindowEntry {
                id: "a".into(),
                title: "A".into(),
                open: true,
                colour: None,
            },
            WindowEntry {
                id: "b".into(),
                title: "B".into(),
                open: true,
                colour: None,
            },
        ];
        // A in front, its member active: the member's items.
        let tab_a = menu_tab(&ws, Some("a"), Some("A"));
        assert_eq!(tab_a.as_ref(), Some(&m));
        let in_a = key_for(&ws, &cold(), tab_a.as_ref(), None, &entries);
        assert_eq!(in_a.group, menu_state_for(&ws, &cold(), None, Some(&m)));
        assert_eq!(in_a.group.make_own_group, on("Make Own Group"));
        // B in front, nothing active there: no tab, so nothing acts on A's member.
        let tab_b = menu_tab(&ws, Some("b"), Some("B"));
        assert_eq!(tab_b, None);
        let in_b = key_for(&ws, &cold(), tab_b.as_ref(), None, &entries);
        assert_eq!(in_b.group, menu_state_for(&ws, &cold(), None, None));
        assert_ne!(in_a, in_b, "a focus change rebuilds the menu bar");
        assert_eq!(in_b.windows.len(), 2);
        // A focused pop-out: its own tab, not its window's active one.
        ws.set_detached(&m, true);
        ws.activate(&a);
        let popout = crate::detach::window_label(&m);
        assert_eq!(menu_tab(&ws, Some(&popout), Some("A")), Some(m));
    }

    #[test]
    fn recent_lists_closed_groups_newest_first() {
        let (mut ws, h, _) = one_group();
        let s = menu_state_for(&ws, &cold(), None, Some(&h));
        assert_eq!(s.recent, off("Recent — Nothing closed yet"));
        assert!(s.recent_entries.is_empty());

        let w = add_group(&mut ws, "f2", "Website");
        ws.close_group(&h);
        ws.close_group(&w);
        let s = menu_state_for(&ws, &cold(), None, None);
        assert_eq!(s.recent, on("Recent"));
        assert_eq!(s.recent_entries, ["Website", "Auction"]);
    }

    #[test]
    fn no_tab_disables_every_tab_action_but_keeps_recent() {
        let (mut ws, h, _) = one_group();
        ws.close_group(&h);
        for s in [
            menu_state_for(&ws, &cold(), None, None),
            menu_state_for(&ws, &cold(), None, Some(&"t-99".to_string())),
        ] {
            assert_eq!(s.home, off("Home — No tab selected"));
            assert_eq!(s.make_own_group, off("Make Own Group — No tab selected"));
            assert_eq!(s.move_to, off("Move to Group — No tab selected"));
            assert_eq!(s.unload_tab, off("Unload Tab — No tab selected"));
            assert_eq!(s.remove_tab, off("Remove Tab — No tab selected"));
            assert_eq!(s.unload_group, off("Unload Group — No tab selected"));
            assert_eq!(s.close_group, off("Close Group — No tab selected"));
            assert_eq!(s.recent, on("Recent"));
        }
    }

    /// A disabled NSMenuItem swallows its shortcut, and ⌘W in a pop-out docks it whatever the main
    /// window shows.
    #[test]
    fn unload_tab_stays_enabled_with_no_active_tab_while_a_tab_is_popped_out() {
        let (mut ws, _, m) = one_group();
        ws.set_detached(&m, true);
        ws.windows[0].active = None;
        let s = menu_state_for(&ws, &cold(), None, None);
        assert_eq!(s.unload_tab, on("Unload Tab"));
        assert_eq!(s.remove_tab, off("Remove Tab — No tab selected"));
        assert_eq!(s.home, off("Home — No tab selected"));
        ws.set_detached(&m, false);
        assert_eq!(
            menu_state_for(&ws, &cold(), None, None).unload_tab,
            off("Unload Tab — No tab selected")
        );
    }

    #[test]
    fn the_menu_bar_is_rebuilt_only_when_its_state_changes() {
        let (mut ws, h, m) = one_group();
        let mut last = None;
        assert!(needs_rebuild(
            &mut last,
            menu_state_for(&ws, &cold(), None, Some(&h))
        ));
        // A push that changes nothing the menu shows (a member's retitle) rebuilds nothing.
        ws.set_title(&m, "Budget");
        assert!(!needs_rebuild(
            &mut last,
            menu_state_for(&ws, &cold(), None, Some(&h))
        ));
        // Selecting the member enables Make Own Group: rebuilt.
        assert!(needs_rebuild(
            &mut last,
            menu_state_for(&ws, &cold(), None, Some(&m))
        ));
        assert_eq!(last, Some(menu_state_for(&ws, &cold(), None, Some(&m))));
    }
}
