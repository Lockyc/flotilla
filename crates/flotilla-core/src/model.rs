//! The workspace model: groups of tabs, the one-copy registry and Recent. Pure and platform-neutral;
//! the shell owns webviews and persistence and drives this through `Workspace`.
//!
//! Two levels only: a group's head (any URL, usually a Drive folder) and flat members (documents).
//! A document is open at most once across every group, detached tabs included; opening it again
//! raises the existing tab. The registry is not a separate index: it is derived by scanning the
//! tabs, so it cannot drift from them.

use crate::identity::{doc_key, identity, DocKey};
use serde::{Deserialize, Serialize};
use shell_core::tabs::pick_live_neighbour;
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
    /// What this tab is, scoped by its group's store: the document it shows, or for a head that
    /// is not a document, its `head_key`. Open-once compares these.
    pub key: DocKey,
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
    /// The tab the main window shows. Never a detached tab: activating one leaves this as it is
    /// and asks for its window instead ([`Workspace::take_raise`]).
    pub active: Option<TabId>,
    next_id: u64,
    /// The detached tab the last operation activated, whose window the shell is to raise.
    #[serde(skip)]
    raise: Pending,
}

/// A request waiting for the shell, not part of the workspace's value: never saved, and two
/// workspaces differing only in it are equal.
#[derive(Clone, Debug, Default)]
struct Pending(Option<TabId>);

impl PartialEq for Pending {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Pending {}

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
            key: identity(store, url),
            detached: false,
        }
    }

    /// Makes a deserialised workspace consistent with the rules the code enforces, trusting
    /// nothing on disk that is derivable: re-derives every tab's key (groups and recent) from its
    /// `home_url` and group store, drops later open members whose key now repeats an earlier
    /// open tab's (heads are never dropped), clears `active` unless it names an open docked tab,
    /// and raises `next_id` to at least every id still present once unparseable members are gone
    /// (duplicate members, dropped after it, included) so a new tab never reuses a restored one.
    pub fn rekey_and_repair(&mut self) {
        for g in self.groups.iter_mut().chain(self.recent.iter_mut()) {
            let store = g.store.clone();
            // A member whose URL no longer parses cannot be keyed or opened: dropped. A head
            // with such a URL keeps the group and its persisted key.
            g.members.retain(|m| Url::parse(&m.home_url).is_ok());
            for t in std::iter::once(&mut g.head).chain(g.members.iter_mut()) {
                if let Ok(url) = Url::parse(&t.home_url) {
                    t.key = identity(&store, &url);
                }
            }
        }
        let max = self
            .groups
            .iter()
            .chain(self.recent.iter())
            .flat_map(|g| std::iter::once(&g.head).chain(g.members.iter()))
            .filter_map(|t| t.id.strip_prefix("t-")?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        self.next_id = self.next_id.max(max);
        let mut seen: Vec<DocKey> = self.groups.iter().map(|g| g.head.key.clone()).collect();
        for g in &mut self.groups {
            g.members.retain(|m| {
                if seen.contains(&m.key) {
                    false
                } else {
                    seen.push(m.key.clone());
                    true
                }
            });
        }
        if let Some(a) = &self.active {
            if self.find(a).is_none_or(|(_, t)| t.detached) {
                self.active = None;
            }
        }
    }

    fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.groups
            .iter()
            .flat_map(|g| std::iter::once(&g.head).chain(g.members.iter()))
    }

    /// The open tab (head or member, detached or not) with identity `key`.
    fn open_doc(&self, key: &DocKey) -> Option<TabId> {
        self.tabs().find(|t| &t.key == key).map(|t| t.id.clone())
    }

    fn group_index(&self, id: &TabId) -> Option<usize> {
        self.groups
            .iter()
            .position(|g| &g.head.id == id || g.members.iter().any(|m| &m.id == id))
    }

    /// Makes `id` the active tab, expanding its group if it is a member of a collapsed one. A
    /// detached tab is not made active; its window is asked for ([`Workspace::take_raise`]).
    pub fn activate(&mut self, id: &TabId) {
        let Some(gi) = self.group_index(id) else {
            return;
        };
        if self.find(id).is_some_and(|(_, t)| t.detached) {
            self.raise.0 = Some(id.clone());
            return;
        }
        let g = &mut self.groups[gi];
        if &g.head.id != id {
            g.collapsed = false;
        }
        self.active = Some(id.clone());
    }

    /// The detached tab an operation since the last call activated, if any: its window is to be
    /// raised. Cleared by the call.
    pub fn take_raise(&mut self) -> Option<TabId> {
        self.raise.0.take()
    }

    fn raise(&mut self, id: TabId) -> Opened {
        self.activate(&id);
        Opened::Raised(id)
    }

    /// Opens `url` as a new group head in `store`. Raises instead when a tab with the same
    /// identity (`identity`) is already open.
    pub fn new_group(&mut self, url: &Url, store: &str) -> Opened {
        if let Some(id) = self.open_doc(&identity(store, url)) {
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

    /// Closes a tab. Closing a head closes its group (`close_group` then picks the neighbouring
    /// group's head). Closing the active member activates the tab
    /// `shell_core::tabs::pick_live_neighbour` picks over `[head, members...]` with every docked
    /// tab eligible, else none.
    pub fn close_tab(&mut self, id: &TabId) {
        let Some(gi) = self.group_index(id) else {
            return;
        };
        if &self.groups[gi].head.id == id {
            return self.close_group(id);
        }
        let g = &mut self.groups[gi];
        let at = g.members.iter().position(|m| &m.id == id).unwrap();
        let eligible: Vec<bool> = std::iter::once(&g.head)
            .chain(g.members.iter())
            .map(|t| !t.detached)
            .collect();
        let pick = pick_live_neighbour(at + 1, &eligible);
        let neighbour = pick.map(|i| match i {
            0 => g.head.id.clone(),
            i => g.members[i - 1].id.clone(),
        });
        g.members.remove(at);
        if self.active.as_ref() == Some(id) {
            self.active = None;
            if let Some(n) = neighbour {
                self.activate(&n);
            }
        }
    }

    /// Closes the group headed by `head` into the front of `recent`. If the active tab was in it,
    /// the nearest following group's docked head (else the nearest preceding one's) becomes
    /// active, else none.
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
            let docked = |g: &&Group| !g.head.detached;
            self.active = self.groups[gi..]
                .iter()
                .find(docked)
                .or_else(|| self.groups[..gi].iter().rev().find(docked))
                .map(|g| g.head.id.clone());
        }
    }

    /// Restores `recent[index]` as a group and activates its head. Members whose document is now
    /// open elsewhere are left where they are.
    ///
    /// If the head's identity is now open elsewhere, nothing is duplicated and that tab is raised
    /// and returned: when it is a head, the entry's other members join its group and the entry
    /// leaves Recent; when it is a member, the entry stays in Recent untouched.
    pub fn reopen_recent(&mut self, index: usize) -> Option<TabId> {
        let entry = self.recent.get(index)?;
        if let Some(occupant) = self.open_doc(&entry.head.key) {
            let gi = self.group_index(&occupant)?;
            if self.groups[gi].head.id != occupant {
                self.raise(occupant.clone());
                return Some(occupant);
            }
            let entry = self.recent.remove(index);
            let members = self.restorable(entry.members, &[entry.head.key]);
            self.groups[gi].members.extend(members);
            self.raise(occupant.clone());
            return Some(occupant);
        }
        let mut group = self.recent.remove(index);
        group.members = self.restorable(group.members, &[group.head.key.clone()]);
        group.head.detached = false;
        let id = group.head.id.clone();
        self.groups.push(group);
        self.activate(&id);
        Some(id)
    }

    /// `members` minus those whose identity is open now or repeats, with `detached` cleared.
    fn restorable(&self, members: Vec<Tab>, taken: &[DocKey]) -> Vec<Tab> {
        let mut seen = taken.to_vec();
        let mut out = vec![];
        for mut m in members {
            if seen.contains(&m.key) || self.open_doc(&m.key).is_some() {
                continue;
            }
            seen.push(m.key.clone());
            m.detached = false;
            out.push(m);
        }
        out
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
            let Ok(url) = Url::parse(&tab.home_url) else {
                return;
            };
            tab.key = identity(&store, &url);
            if self.open_doc(&tab.key).is_some() {
                return;
            }
        }
        self.groups[from].members.remove(at);
        self.groups[to].members.push(tab);
        if self.active.as_ref() == Some(member) {
            self.activate(member);
        }
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

    /// Detaching the active tab leaves nothing active; the shell picks what the main window shows
    /// next before detaching.
    pub fn set_detached(&mut self, id: &TabId, detached: bool) {
        if let Some(t) = self.tab_mut(id) {
            t.detached = detached;
            if detached && self.active.as_ref() == Some(id) {
                self.active = None;
            }
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
        assert_eq!(w.active, Some(a.clone())); // leans up
        w.activate(&c);
        w.close_tab(&c);
        assert_eq!(w.active, Some(a.clone()));
        w.close_tab(&a);
        assert_eq!(w.active, Some(h)); // the head is above the first member
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

    #[test]
    fn identity_is_scoped_by_store_not_by_role() {
        let mut w = Workspace::default();
        let h1 = id(w.new_group(&doc(1), "default"));
        let work = id(w.new_group(&folder(2), "work"));
        // same doc, other store: a second tab is allowed
        let m = id(w.open_from(&work, &doc(1)).unwrap());
        assert_ne!(m, h1);
        w.make_own_group(&m);
        assert_eq!(w.groups.len(), 3);
        // same doc, same store: raised, whether the tab is a head or a member
        assert_eq!(w.new_group(&doc(1), "work"), Opened::Raised(m));
        assert_eq!(w.new_group(&doc(1), "default"), Opened::Raised(h1.clone()));
        assert_eq!(w.new_group(&folder(2), "work"), Opened::Raised(work));
        // the same folder in another store is its own head
        assert!(matches!(
            w.new_group(&folder(2), "default"),
            Opened::Added(_)
        ));
    }

    #[test]
    fn reopen_recent_with_head_open_elsewhere_merges_members_into_it() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        w.open_from(&h1, &doc(1)).unwrap();
        w.open_from(&h1, &doc(2)).unwrap();
        w.close_group(&h1);
        let h2 = head_of(&mut w, 1);
        let shared = id(w.open_from(&h2, &doc(1)).unwrap());
        assert_eq!(w.reopen_recent(0), Some(h2.clone()));
        assert!(w.recent.is_empty());
        let members = member_ids(&w, &h2);
        assert_eq!(members.len(), 2);
        assert_eq!(members[0], shared);
        assert_eq!(w.active, Some(h2));
    }

    #[test]
    fn reopen_recent_with_head_open_as_a_member_leaves_the_entry() {
        let mut w = Workspace::default();
        let h1 = id(w.new_group(&doc(5), "default"));
        let kept = id(w.open_from(&h1, &doc(1)).unwrap());
        w.close_group(&h1);
        let h2 = head_of(&mut w, 2);
        let m = id(w.open_from(&h2, &doc(5)).unwrap());
        assert_eq!(w.reopen_recent(0), Some(m.clone()));
        assert_eq!(w.recent.len(), 1);
        assert_eq!(w.recent[0].members[0].id, kept);
        assert_eq!(w.groups.len(), 1);
        assert_eq!(w.active, Some(m));
    }

    #[test]
    fn closing_the_active_member_of_a_collapsed_group_expands_it() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        let a = id(w.open_from(&h, &doc(1)).unwrap());
        let b = id(w.open_from(&h, &doc(2)).unwrap());
        w.set_collapsed(&h, true);
        w.close_tab(&b);
        assert_eq!(w.active, Some(a));
        assert!(!w.group_of(&h).unwrap().collapsed);
    }

    #[test]
    fn moving_the_active_member_into_a_collapsed_group_expands_it() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        let h2 = head_of(&mut w, 2);
        let d = id(w.open_from(&h1, &doc(1)).unwrap());
        w.set_collapsed(&h2, true);
        w.move_to_group(&d, &h2);
        assert_eq!(w.active, Some(d));
        assert!(!w.group_of(&h2).unwrap().collapsed);
    }

    #[test]
    fn moving_onto_its_own_group_is_a_no_op() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        let a = id(w.open_from(&h, &doc(1)).unwrap());
        let b = id(w.open_from(&h, &doc(2)).unwrap());
        w.move_to_group(&a, &h);
        w.move_to_group(&a, &b);
        assert_eq!(member_ids(&w, &h), vec![a, b]);
    }

    #[test]
    fn make_own_group_of_a_head_is_a_no_op() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        w.open_from(&h, &doc(1)).unwrap();
        let before = w.clone();
        w.make_own_group(&h);
        assert_eq!(w, before);
    }

    #[test]
    fn moving_across_stores_rekeys_or_refuses_on_collision() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        let work = id(w.new_group(&folder(2), "work"));
        let d = id(w.open_from(&h1, &doc(1)).unwrap());
        w.move_to_group(&d, &work);
        assert_eq!(w.find(&d).unwrap().1.key.store, "work");
        assert_eq!(member_ids(&w, &work), vec![d.clone()]);
        // doc 2 is open in both stores: moving the default copy into work collides
        let e = id(w.open_from(&h1, &doc(2)).unwrap());
        let e2 = id(w.open_from(&work, &doc(2)).unwrap());
        w.move_to_group(&e, &work);
        assert_eq!(member_ids(&w, &h1), vec![e]);
        assert_eq!(member_ids(&w, &work), vec![d, e2]);
    }

    #[test]
    fn closing_the_active_group_activates_the_neighbouring_head() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        let h2 = head_of(&mut w, 2);
        let h3 = head_of(&mut w, 3);
        w.activate(&h2);
        w.close_group(&h2);
        assert_eq!(w.active, Some(h3.clone()));
        w.close_group(&h3);
        assert_eq!(w.active, Some(h1));
    }

    #[test]
    fn ids_are_not_reused_within_a_session() {
        let mut w = Workspace::default();
        let a = head_of(&mut w, 1);
        w.close_group(&a);
        let b = head_of(&mut w, 2);
        w.close_tab(&b);
        let c = head_of(&mut w, 3);
        assert!(a != b && b != c && a != c);
    }

    #[test]
    fn rekey_and_repair_rederives_keys_dedups_and_fixes_next_id() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        let h2 = head_of(&mut w, 2);
        let a = id(w.open_from(&h1, &doc(1)).unwrap());
        let b = id(w.open_from(&h2, &doc(2)).unwrap());
        w.close_group(&h2);
        let r = head_of(&mut w, 3);
        w.close_group(&r);
        // corrupt: wrong keys, a member repeating another's key, a lowered counter, a stale active
        w.groups[0].head.key.id = "bogus".into();
        w.groups[0].members[0].key = w.groups[0].head.key.clone();
        w.recent[0].head.key.id = "bogus".into();
        let dup = Tab {
            id: "t-99".into(),
            home_url: doc(1).to_string(),
            title: "dup".into(),
            key: w.groups[0].head.key.clone(),
            detached: false,
        };
        w.groups[0].members.push(dup);
        w.next_id = 1;
        w.active = Some("t-500".into());
        w.rekey_and_repair();
        assert_eq!(w.groups[0].head.key, identity("default", &folder(1)));
        assert_eq!(w.recent[0].head.key, identity("default", &folder(3)));
        assert_eq!(w.recent[1].members[0].key, identity("default", &doc(2)));
        assert_eq!(member_ids(&w, &h1), vec![a]); // later duplicate dropped, first kept
        assert_eq!(w.active, None);
        assert_eq!(w.next_id, 99);
        let _ = b;
        assert_eq!(id(w.new_group(&folder(9), "default")), "t-100");
    }

    #[test]
    fn rekey_and_repair_never_drops_a_head_for_a_duplicate_member() {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        let h2 = id(w.new_group(&doc(1), "default"));
        let mut m = w.groups[0].head.clone();
        m.id = "t-77".into();
        m.home_url = doc(1).to_string();
        w.groups[0].members.push(m);
        w.rekey_and_repair();
        assert!(w.groups[0].members.is_empty());
        assert_eq!(w.groups[1].head.id, h2);
        assert_eq!(w.groups[0].head.id, h1);
    }

    #[test]
    fn rekey_and_repair_drops_unparseable_members_but_keeps_such_a_head() {
        let mut w = Workspace::default();
        let h = head_of(&mut w, 1);
        w.open_from(&h, &doc(1)).unwrap();
        let key = w.groups[0].head.key.clone();
        w.groups[0].members[0].home_url = "not a url".into();
        w.groups[0].head.home_url = "also not".into();
        w.rekey_and_repair();
        assert!(w.groups[0].members.is_empty());
        assert_eq!(w.groups[0].head.key, key);
    }

    // `active` is never a detached tab.

    /// Group 1: head h1, members a, b. Group 2: head h2. `active` is b.
    fn two_groups() -> (Workspace, TabId, TabId, TabId, TabId) {
        let mut w = Workspace::default();
        let h1 = head_of(&mut w, 1);
        let a = id(w.open_from(&h1, &doc(1)).unwrap());
        let b = id(w.open_from(&h1, &doc(2)).unwrap());
        let h2 = head_of(&mut w, 2);
        w.activate(&b);
        (w, h1, a, b, h2)
    }

    #[test]
    fn activating_a_detached_tab_keeps_active_and_asks_for_its_window() {
        let (mut w, h1, a, b, _) = two_groups();
        w.set_detached(&a, true);
        w.set_collapsed(&h1, true);
        w.activate(&a);
        assert_eq!(w.active, Some(b));
        // Nor does it expand the group the docked tab would have been shown in.
        assert!(w.groups[0].collapsed);
        assert_eq!(w.take_raise(), Some(a));
        assert_eq!(w.take_raise(), None);
    }

    #[test]
    fn raising_a_detached_doc_asks_for_its_window() {
        let (mut w, h1, a, b, _) = two_groups();
        w.set_detached(&a, true);
        assert_eq!(w.open_from(&h1, &doc(1)), Some(Opened::Raised(a.clone())));
        assert_eq!(w.active, Some(b));
        assert_eq!(w.take_raise(), Some(a));
    }

    #[test]
    fn detaching_the_active_tab_clears_active() {
        let (mut w, _, _, b, _) = two_groups();
        w.set_detached(&b, true);
        assert_eq!(w.active, None);
    }

    #[test]
    fn making_a_detached_member_its_own_group_keeps_active() {
        let (mut w, _, a, b, _) = two_groups();
        w.set_detached(&a, true);
        w.make_own_group(&a);
        assert_eq!(w.groups[1].head.id, a);
        assert_eq!(w.active, Some(b));
        assert_eq!(w.take_raise(), Some(a));
    }

    #[test]
    fn moving_a_detached_member_keeps_active() {
        let (mut w, _, a, b, h2) = two_groups();
        w.set_detached(&a, true);
        w.move_to_group(&a, &h2);
        assert_eq!(member_ids(&w, &h2), vec![a]);
        assert_eq!(w.active, Some(b));
        assert_eq!(w.take_raise(), None);
    }

    #[test]
    fn closing_a_tab_skips_a_detached_neighbour() {
        let (mut w, h1, a, b, _) = two_groups();
        w.set_detached(&a, true);
        w.close_tab(&b);
        assert_eq!(w.active, Some(h1.clone()));
        // Every neighbour detached: nothing is active.
        let (mut w, h1, a, b, _) = two_groups();
        w.set_detached(&h1, true);
        w.set_detached(&a, true);
        w.close_tab(&b);
        assert_eq!(w.active, None);
    }

    #[test]
    fn closing_a_group_skips_a_group_whose_head_is_detached() {
        let (mut w, h1, _, b, h2) = two_groups();
        let h3 = head_of(&mut w, 3);
        w.activate(&b);
        w.set_detached(&h2, true);
        w.close_group(&h1);
        assert_eq!(w.active, Some(h3.clone()));
        // Only detached heads remain: nothing is active.
        w.set_detached(&h3, true);
        assert_eq!(w.active, None);
        let (mut w, h1, _, _, h2) = two_groups();
        w.set_detached(&h2, true);
        w.close_group(&h1);
        assert_eq!(w.active, None);
    }

    #[test]
    fn loading_clears_an_active_detached_tab() {
        let (mut w, _, _, b, _) = two_groups();
        w.tab_mut(&b).unwrap().detached = true;
        w.rekey_and_repair();
        assert_eq!(w.active, None);
    }
}
