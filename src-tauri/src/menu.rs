//! The app menu bar, the Group menu and the sidebar row's context menu. The Group menu acts on the
//! active tab and the row menu on the clicked row; both are built from [`menu_state_for`], so an
//! item is enabled (or disabled, its reason in its title) the same way in each.

use flotilla_core::model::{TabId, Workspace};

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

const NO_TAB: &str = "No tab open";
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
        close_tab: item("Close Tab", no_tab),
        close_group: item("Close Group", no_tab),
        recent: item("Recent", ws.recent.is_empty().then_some(NOTHING_CLOSED)),
        recent_entries: ws.recent.iter().map(|g| g.head.title.clone()).collect(),
    }
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
            assert_eq!(s.home, off("Home — No tab open"));
            assert_eq!(s.make_own_group, off("Make Own Group — No tab open"));
            assert_eq!(s.move_to, off("Move to Group — No tab open"));
            assert_eq!(s.close_tab, off("Close Tab — No tab open"));
            assert_eq!(s.close_group, off("Close Group — No tab open"));
            assert_eq!(s.recent, on("Recent"));
        }
    }
}
