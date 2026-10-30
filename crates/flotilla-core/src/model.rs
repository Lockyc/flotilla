//! The workspace model: groups of tabs, the one-copy registry and Recent. Pure and platform-neutral;
//! the shell owns webviews and persistence and drives this through `Workspace`.
//!
//! Two levels only: a group's head (any URL, usually a Drive folder) and flat members (documents).
//! A document is open at most once across every group, detached tabs included; opening it again
//! raises the existing tab. The registry is not a separate index: it is derived by scanning the
//! tabs, so it cannot drift from them.

use crate::identity::{doc_key, head_key, DocKey};
use serde::{Deserialize, Serialize};
use url::Url;

/// Closed groups kept in `Workspace::recent`; the oldest is dropped past this.
pub const RECENT_CAP: usize = 20;

/// `"t-<n>"`, from the counter persisted in the workspace. Never reused.
pub type TabId = String;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub home_url: String,
    pub title: String,
    /// The document this tab shows; `None` for a head that is not a document.
    pub key: Option<DocKey>,
    /// Popped out into its own window. Still open for the one-copy rule.
    pub detached: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub head: Tab,
    pub members: Vec<Tab>,
    pub collapsed: bool,
    /// The login store (profile) every tab in the group is open in.
    pub store: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub groups: Vec<Group>,
    /// Closed groups, newest first.
    pub recent: Vec<Group>,
    pub active: Option<TabId>,
    next_id: u64,
}

/// What opening a URL did; either way the tab is now active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    Added(TabId),
    Raised(TabId),
}

impl Workspace {
    fn mint(&mut self, url: &Url, store: &str) -> Tab {
        self.next_id += 1;
        Tab {
            id: format!("t-{}", self.next_id),
            home_url: url.to_string(),
            title: url.to_string(),
            key: doc_key(store, url),
            detached: false,
        }
    }

    fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.groups
            .iter()
            .flat_map(|g| std::iter::once(&g.head).chain(g.members.iter()))
    }

    /// The open tab (head or member, detached or not) showing `key`.
    fn open_doc(&self, key: &DocKey) -> Option<TabId> {
        self.tabs()
            .find(|t| t.key.as_ref() == Some(key))
            .map(|t| t.id.clone())
    }

    /// The open head whose `head_key` is `hk`.
    fn open_head(&self, hk: &str) -> Option<TabId> {
        self.groups
            .iter()
            .find(|g| {
                Url::parse(&g.head.home_url)
                    .map(|u| head_key(&u) == hk)
                    .unwrap_or(false)
            })
            .map(|g| g.head.id.clone())
    }

    fn group_index(&self, id: &TabId) -> Option<usize> {
        self.groups
            .iter()
            .position(|g| &g.head.id == id || g.members.iter().any(|m| &m.id == id))
    }

    /// Makes `id` the active tab, expanding its group if it is a member of a collapsed one.
    pub fn activate(&mut self, id: &TabId) {
        let Some(gi) = self.group_index(id) else {
            return;
        };
        let g = &mut self.groups[gi];
        if &g.head.id != id {
            g.collapsed = false;
        }
        self.active = Some(id.clone());
    }

    fn raise(&mut self, id: TabId) -> Opened {
        self.activate(&id);
        Opened::Raised(id)
    }

    /// Opens `url` as a new group head in `store`. Raises instead when an open head has the same
    /// `head_key`, or an open tab already shows the same document.
    pub fn new_group(&mut self, url: &Url, store: &str) -> Opened {
        if let Some(id) = self.open_head(&head_key(url)) {
            return self.raise(id);
        }
        if let Some(id) = doc_key(store, url).and_then(|k| self.open_doc(&k)) {
            return self.raise(id);
        }
        let head = self.mint(url, store);
        let id = head.id.clone();
        self.groups.push(Group {
            head,
            members: vec![],
            collapsed: false,
            store: store.to_string(),
        });
        self.activate(&id);
        Opened::Added(id)
    }

    /// Opens document `url` from tab `from`, joining `from`'s group at the end. `None` when `url`
    /// is not a document or `from` is not open. An already-open document is raised wherever it is.
    pub fn open_from(&mut self, from: &TabId, url: &Url) -> Option<Opened> {
        let gi = self.group_index(from)?;
        let key = doc_key(&self.groups[gi].store, url)?;
        if let Some(id) = self.open_doc(&key) {
            return Some(self.raise(id));
        }
        let store = self.groups[gi].store.clone();
        let tab = self.mint(url, &store);
        let id = tab.id.clone();
        self.groups[gi].members.push(tab);
        self.activate(&id);
        Some(Opened::Added(id))
    }

    /// Closes a tab. Closing a head closes its group. Closing the active member activates the next
    /// member, else the previous, else the head.
    ///
    /// Not `shell_core::tabs::pick_live_neighbour`: that leans previous-first, and a group's
    /// order is head, members.
    pub fn close_tab(&mut self, id: &TabId) {
        let Some(gi) = self.group_index(id) else {
            return;
        };
        if &self.groups[gi].head.id == id {
            return self.close_group(id);
        }
        let g = &mut self.groups[gi];
        let at = g.members.iter().position(|m| &m.id == id).unwrap();
        g.members.remove(at);
        if self.active.as_ref() == Some(id) {
            let next = g
                .members
                .get(at)
                .or_else(|| at.checked_sub(1).and_then(|p| g.members.get(p)))
                .unwrap_or(&g.head)
                .id
                .clone();
            self.active = Some(next);
        }
    }

    /// Closes the group headed by `head` into the front of `recent`. If the active tab was in it,
    /// the next group's head (else the previous group's) becomes active.
    pub fn close_group(&mut self, head: &TabId) {
        let Some(gi) = self.groups.iter().position(|g| &g.head.id == head) else {
            return;
        };
        let group = self.groups.remove(gi);
        let was_active = self
            .active
            .as_ref()
            .is_some_and(|a| a == &group.head.id || group.members.iter().any(|m| &m.id == a));
        self.recent.insert(0, group);
        self.recent.truncate(RECENT_CAP);
        if was_active {
            self.active = self
                .groups
                .get(gi)
                .or_else(|| gi.checked_sub(1).and_then(|p| self.groups.get(p)))
                .map(|g| g.head.id.clone());
        }
    }

    /// Restores `recent[index]` as a group and activates its head. Members whose document is now
    /// open elsewhere are left where they are. If the head itself is now open elsewhere (same
    /// `head_key`, or its document is open), that tab is raised and returned instead, nothing is
    /// restored, and the entry leaves Recent.
    pub fn reopen_recent(&mut self, index: usize) -> Option<TabId> {
        if index >= self.recent.len() {
            return None;
        }
        let mut group = self.recent.remove(index);
        let occupied = Url::parse(&group.head.home_url)
            .ok()
            .and_then(|u| self.open_head(&head_key(&u)))
            .or_else(|| group.head.key.as_ref().and_then(|k| self.open_doc(k)));
        if let Some(id) = occupied {
            self.raise(id.clone());
            return Some(id);
        }
        let mut seen: Vec<DocKey> = group.head.key.iter().cloned().collect();
        group.members.retain(|m| match &m.key {
            Some(k) if seen.contains(k) || self.open_doc(k).is_some() => false,
            Some(k) => {
                seen.push(k.clone());
                true
            }
            None => true,
        });
        for t in std::iter::once(&mut group.head).chain(group.members.iter_mut()) {
            t.detached = false;
        }
        let id = group.head.id.clone();
        self.groups.push(group);
        self.activate(&id);
        Some(id)
    }

    pub fn forget_recent(&mut self, index: usize) {
        if index < self.recent.len() {
            self.recent.remove(index);
        }
    }

    /// Lifts a member into a new group headed by it (same store), placed after its old group.
    pub fn make_own_group(&mut self, member: &TabId) {
        let Some(gi) = self.group_index(member) else {
            return;
        };
        let g = &mut self.groups[gi];
        let Some(at) = g.members.iter().position(|m| &m.id == member) else {
            return;
        };
        let head = g.members.remove(at);
        let store = g.store.clone();
        self.groups.insert(
            gi + 1,
            Group {
                head,
                members: vec![],
                collapsed: false,
                store,
            },
        );
        self.activate(member);
    }

    /// Moves a member to the end of the group `head` belongs to. A move into a group on another
    /// store re-keys the document to that store, and is refused if it is already open there.
    pub fn move_to_group(&mut self, member: &TabId, head: &TabId) {
        let (Some(from), Some(to)) = (self.group_index(member), self.group_index(head)) else {
            return;
        };
        if from == to {
            return;
        }
        let Some(at) = self.groups[from]
            .members
            .iter()
            .position(|m| &m.id == member)
        else {
            return;
        };
        let mut tab = self.groups[from].members[at].clone();
        if self.groups[from].store != self.groups[to].store {
            let store = self.groups[to].store.clone();
            tab.key = Url::parse(&tab.home_url)
                .ok()
                .and_then(|u| doc_key(&store, &u));
            if tab.key.as_ref().is_some_and(|k| self.open_doc(k).is_some()) {
                return;
            }
        }
        self.groups[from].members.remove(at);
        self.groups[to].members.push(tab);
    }

    fn tab_mut(&mut self, id: &TabId) -> Option<&mut Tab> {
        self.groups.iter_mut().find_map(|g| {
            if &g.head.id == id {
                Some(&mut g.head)
            } else {
                g.members.iter_mut().find(|m| &m.id == id)
            }
        })
    }

    pub fn set_collapsed(&mut self, head: &TabId, collapsed: bool) {
        if let Some(g) = self.groups.iter_mut().find(|g| &g.head.id == head) {
            g.collapsed = collapsed;
        }
    }

    pub fn set_title(&mut self, id: &TabId, title: &str) {
        if let Some(t) = self.tab_mut(id) {
            t.title = title.to_string();
        }
    }

    pub fn set_detached(&mut self, id: &TabId, detached: bool) {
        if let Some(t) = self.tab_mut(id) {
            t.detached = detached;
        }
    }

    pub fn find(&self, id: &TabId) -> Option<(&Group, &Tab)> {
        self.groups.iter().find_map(|g| {
            std::iter::once(&g.head)
                .chain(g.members.iter())
                .find(|t| &t.id == id)
                .map(|t| (g, t))
        })
    }

    pub fn group_of(&self, id: &TabId) -> Option<&Group> {
        self.find(id).map(|(g, _)| g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(n: u32) -> Url {
        Url::parse(&format!(
            "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt{n:02}/edit"
        ))
        .unwrap()
    }

    fn folder(n: u32) -> Url {
        Url::parse(&format!("https://drive.google.com/drive/folders/fold{n}")).unwrap()
    }

    fn id(o: Opened) -> TabId {
        match o {
            Opened::Added(i) | Opened::Raised(i) => i,
        }
    }

    fn head_of(w: &mut Workspace, n: u32) -> TabId {
        id(w.new_group(&folder(n), "default"))
    }

    fn member_ids(w: &Workspace, head: &TabId) -> Vec<TabId> {
        w.group_of(head)
            .unwrap()
            .members
            .iter()
            .map(|t| t.id.clone())
            .collect()
    }

    #[test]
    fn opening_a_doc_adds_it_under_the_group() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        let d = w.open_from(&h, &doc(1)).unwrap();
        assert!(matches!(d, Opened::Added(_)));
        assert_eq!(member_ids(&w, &h), vec![id(d.clone())]);
        assert_eq!(w.active, Some(id(d)));
    }

    #[test]
    fn reopening_an_open_doc_raises_it_even_from_another_group() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        let h2 = head_of(&mut w, 2);
        let d = id(w.open_from(&h1, &doc(1)).unwrap());
        w.activate(&h2);
        assert_eq!(w.open_from(&h2, &doc(1)), Some(Opened::Raised(d.clone())));
        assert_eq!(w.active, Some(d));
        assert!(member_ids(&w, &h2).is_empty());
    }

    #[test]
    fn a_detached_doc_still_counts_as_open() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        let d = id(w.open_from(&h, &doc(1)).unwrap());
        w.set_detached(&d, true);
        assert_eq!(w.open_from(&h, &doc(1)), Some(Opened::Raised(d)));
        assert_eq!(member_ids(&w, &h).len(), 1);
    }

    #[test]
    fn open_from_a_member_joins_the_same_group_flat() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        let a = id(w.open_from(&h, &doc(1)).unwrap());
        let b = id(w.open_from(&a, &doc(2)).unwrap());
        assert_eq!(member_ids(&w, &h), vec![a, b]);
        assert_eq!(w.groups.len(), 1);
    }

    #[test]
    fn non_document_url_is_not_a_member() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        assert_eq!(w.open_from(&h, &folder(2)), None);
        assert!(member_ids(&w, &h).is_empty());
    }

    #[test]
    fn new_group_on_an_open_head_raises_it() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        let _h2 = head_of(&mut w, 2);
        assert_eq!(
            w.new_group(&folder(1), "default"),
            Opened::Raised(h1.clone())
        );
        assert_eq!(w.groups.len(), 2);
        assert_eq!(w.active, Some(h1.clone()));
        // an open member's doc raises the member
        let d = id(w.open_from(&h1, &doc(1)).unwrap());
        assert_eq!(w.new_group(&doc(1), "default"), Opened::Raised(d));
        assert_eq!(w.groups.len(), 2);
    }

    #[test]
    fn a_document_head_occupies_its_doc_key() {
        let mut w = Workspace::default();
        let h1 = id(w.new_group(&doc(1), "default"));
        let h2 = head_of(&mut w, 2);
        assert_eq!(w.open_from(&h2, &doc(1)), Some(Opened::Raised(h1)));
    }

    #[test]
    fn closing_the_head_closes_the_group_to_recent() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        let d = id(w.open_from(&h, &doc(1)).unwrap());
        w.close_tab(&h);
        assert!(w.groups.is_empty());
        assert_eq!(w.recent.len(), 1);
        assert_eq!(w.recent[0].head.id, h);
        assert_eq!(w.recent[0].members[0].id, d);
        assert_eq!(w.active, None);
    }

    #[test]
    fn recent_is_capped_at_20_newest_first() {
        let mut w = Workspace::default();
        let mut heads = vec![];
        for n in 0..25 {
            heads.push(head_of(&mut w, n));
        }
        for h in &heads {
            w.close_group(h);
        }
        assert_eq!(w.recent.len(), RECENT_CAP);
        assert_eq!(w.recent[0].head.id, heads[24]);
        assert_eq!(w.recent[19].head.id, heads[5]);
    }

    #[test]
    fn reopen_recent_skips_members_open_elsewhere() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        w.open_from(&h1, &doc(1)).unwrap();
        let keep = id(w.open_from(&h1, &doc(2)).unwrap());
        w.close_group(&h1);
        let h2 = head_of(&mut w, 2);
        let elsewhere = id(w.open_from(&h2, &doc(1)).unwrap());
        let back = w.reopen_recent(0).unwrap();
        assert_eq!(back, h1);
        assert!(w.recent.is_empty());
        assert_eq!(member_ids(&w, &h1), vec![keep]);
        assert_eq!(member_ids(&w, &h2), vec![elsewhere]);
        assert_eq!(w.active, Some(h1));
    }

    #[test]
    fn reopen_recent_of_a_head_open_elsewhere_raises_it() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        w.close_group(&h1);
        let h2 = head_of(&mut w, 1);
        assert_eq!(w.reopen_recent(0), Some(h2.clone()));
        assert_eq!(w.groups.len(), 1);
        assert_eq!(w.active, Some(h2));
        assert!(w.recent.is_empty());
    }

    #[test]
    fn forget_recent_drops_the_entry() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        w.close_group(&h);
        w.forget_recent(0);
        w.forget_recent(9);
        assert!(w.recent.is_empty());
    }

    #[test]
    fn make_own_group_lifts_a_member_to_a_head() {
        let mut w = Workspace::default();
        let h = id(w.new_group(&folder(1), "work"));
        let d = id(w.open_from(&h, &doc(1)).unwrap());
        w.make_own_group(&d);
        assert_eq!(w.groups.len(), 2);
        assert!(member_ids(&w, &h).is_empty());
        assert_eq!(w.groups[1].head.id, d);
        assert_eq!(w.groups[1].store, "work");
        assert_eq!(w.open_from(&h, &doc(1)), Some(Opened::Raised(d)));
    }

    #[test]
    fn move_to_group_moves_a_member() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        let h2 = head_of(&mut w, 2);
        let d = id(w.open_from(&h1, &doc(1)).unwrap());
        w.move_to_group(&d, &h2);
        assert!(member_ids(&w, &h1).is_empty());
        assert_eq!(member_ids(&w, &h2), vec![d.clone()]);
        assert_eq!(w.open_from(&h1, &doc(1)), Some(Opened::Raised(d)));
    }

    #[test]
    fn activating_a_member_of_a_collapsed_group_expands_it() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        let d = id(w.open_from(&h, &doc(1)).unwrap());
        w.set_collapsed(&h, true);
        w.activate(&d);
        assert!(!w.group_of(&h).unwrap().collapsed);
        w.set_collapsed(&h, true);
        w.open_from(&h, &doc(1));
        assert!(!w.group_of(&h).unwrap().collapsed);
    }

    #[test]
    fn closing_the_active_tab_activates_its_neighbour() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        let a = id(w.open_from(&h, &doc(1)).unwrap());
        let b = id(w.open_from(&h, &doc(2)).unwrap());
        let c = id(w.open_from(&h, &doc(3)).unwrap());
        w.activate(&b);
        w.close_tab(&b);
        assert_eq!(w.active, Some(c.clone())); // next
        w.close_tab(&c);
        assert_eq!(w.active, Some(a.clone())); // else previous
        w.close_tab(&a);
        assert_eq!(w.active, Some(h)); // else head
    }

    #[test]
    fn closing_an_inactive_tab_keeps_the_active_one() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        let a = id(w.open_from(&h, &doc(1)).unwrap());
        w.activate(&h);
        w.close_tab(&a);
        assert_eq!(w.active, Some(h));
    }

    #[test]
    fn titles_and_serialization_round_trip() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        w.set_title(&h, "Projects");
        let s = serde_json::to_string(&w).unwrap();
        let back: Workspace = serde_json::from_str(&s).unwrap();
        assert_eq!(back.find(&h).unwrap().1.title, "Projects");
        assert_eq!(back.groups[0].store, "default");
        let mut back = back;
        let n = head_of(&mut back, 2);
        assert_ne!(n, h);
    }
}
