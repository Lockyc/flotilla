//! The workspace model: windows of groups of tabs, the one-copy registry and Recent. Pure and
//! platform-neutral; the shell owns webviews, the config file and the session file, and drives
//! this through `Workspace`.
//!
//! The windows and their groups are built from the config ([`Workspace::from_config`]) and
//! replaced on each reload ([`Workspace::rebase`]); the session adds each window's active tab and
//! Recent ([`Workspace::restore`]). Two levels only: a group's head (any URL, usually a Drive
//! folder) and flat members (documents). A document is open at most once across every window,
//! detached tabs included; opening it again raises the existing tab in its own window. The
//! registry is not a separate index: it is derived by scanning the tabs, so it cannot drift from
//! them.

use crate::identity::{doc_key, head_key, identity, tab_id, DocKey};
use serde::{Deserialize, Serialize};
use shell_core::tabs::pick_live_neighbour;
use std::collections::BTreeMap;
use url::Url;

/// Closed groups kept in `Workspace::recent`; the oldest is dropped past this.
pub const RECENT_CAP: usize = 20;

/// `"t-<16 hex>"`, the [`tab_id`] of the tab's key: open-once makes it unique app-wide, and it is
/// the same across reloads, reorders and moves between windows.
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

/// One window's groups, and the tab it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowWs {
    /// The config window's title, which names it in the session.
    pub title: String,
    pub groups: Vec<Group>,
    /// The tab the window shows. Never a detached tab: activating one leaves this as it is and
    /// asks for its window instead ([`Workspace::take_raise`]).
    pub active: Option<TabId>,
}

/// A config `[[window]]` as the model reads it; the shell maps its config onto these.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigWindow {
    pub title: String,
    pub groups: Vec<ConfigGroup>,
}

/// A config `[[window.group]]`: its name, head `url`, store and members. `detached` is the head's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigGroup {
    pub name: String,
    pub url: String,
    pub store: String,
    pub collapsed: bool,
    pub detached: bool,
    pub tabs: Vec<ConfigTab>,
}

/// A config `[[window.group.tab]]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigTab {
    pub title: String,
    pub url: String,
    pub detached: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Workspace {
    pub windows: Vec<WindowWs>,
    /// Closed groups, newest first.
    pub recent: Vec<Group>,
    /// The detached tab the last operation activated, whose window the shell is to raise.
    raise: Pending,
}

/// A request waiting for the shell, not part of the workspace's value: two workspaces differing
/// only in it are equal.
#[derive(Clone, Debug, Default)]
struct Pending(Option<TabId>);

impl PartialEq for Pending {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Pending {}

/// What opening a URL did; either way the tab is now active in its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    Added(TabId),
    Raised(TabId),
}

/// A tab opening `url` in `store`, keyed and identified from them.
fn tab(url: &Url, title: &str, store: &str, detached: bool) -> Tab {
    let key = identity(store, url);
    Tab {
        id: tab_id(&key),
        home_url: url.to_string(),
        title: title.to_string(),
        key,
        detached,
    }
}

/// Re-derives `t`'s key and id from its `home_url` in `store`. A URL that no longer parses keeps
/// both.
fn rekey(t: &mut Tab, store: &str) {
    if let Ok(url) = Url::parse(&t.home_url) {
        t.key = identity(store, &url);
        t.id = tab_id(&t.key);
    }
}

impl Workspace {
    /// The windows and groups the config holds, nothing active. Each tab's `home_url` is the
    /// config's own text, so a write addressing the tab by its `url` finds it. An entry whose URL
    /// does not parse, or whose identity repeats an earlier one, is left out (the config's own
    /// checks refuse both, so this only guards the model's invariant); heads are claimed before
    /// members, so a head is never dropped for a duplicate member.
    pub fn from_config(windows: &[ConfigWindow]) -> Workspace {
        let configured = |url: &str, title: &str, store: &str, detached: bool| {
            let mut t = tab(&Url::parse(url).ok()?, title, store, detached);
            t.home_url = url.to_string();
            Some(t)
        };
        let mut seen: Vec<DocKey> = vec![];
        let mut claim = |t: &Tab| {
            let fresh = !seen.contains(&t.key);
            if fresh {
                seen.push(t.key.clone());
            }
            fresh
        };
        // Heads first, each with the config group it came from.
        let heads: Vec<Vec<(&ConfigGroup, Tab)>> = windows
            .iter()
            .map(|w| {
                w.groups
                    .iter()
                    .filter_map(|g| Some((g, configured(&g.url, &g.name, &g.store, g.detached)?)))
                    .filter(|(_, head)| claim(head))
                    .collect()
            })
            .collect();
        let windows = windows
            .iter()
            .zip(heads)
            .map(|(w, heads)| WindowWs {
                title: w.title.trim().to_string(),
                groups: heads
                    .into_iter()
                    .map(|(g, head)| Group {
                        head,
                        members: g
                            .tabs
                            .iter()
                            .filter_map(|t| configured(&t.url, &t.title, &g.store, t.detached))
                            .filter(|t| claim(t))
                            .collect(),
                        collapsed: g.collapsed,
                        store: g.store.clone(),
                    })
                    .collect(),
                active: None,
            })
            .collect();
        Workspace {
            windows,
            ..Default::default()
        }
    }

    /// Adopts a reloaded config's windows. Each window keeps its active tab when a window of the
    /// same title still holds it docked; when it went (a hand edit), its group's docked head takes
    /// over, else the nearest following group's docked head, else the nearest preceding one's.
    /// Recent is the session's and stays.
    pub fn rebase(&mut self, fresh: Workspace) {
        let old = std::mem::replace(&mut self.windows, fresh.windows);
        for w in &mut self.windows {
            let Some(o) = old.iter().find(|o| o.title == w.title) else {
                w.active = None;
                continue;
            };
            let Some(a) = o.active.clone() else {
                w.active = None;
                continue;
            };
            if docked_in(w, &a) {
                w.active = Some(a);
                continue;
            }
            let gi = o
                .groups
                .iter()
                .position(|g| g.head.id == a || g.members.iter().any(|m| m.id == a))
                .unwrap_or(0);
            let head = o.groups.get(gi).map(|g| g.head.id.clone());
            let docked = |g: &&Group| !g.head.detached;
            let at = gi.min(w.groups.len());
            w.active = head.filter(|h| docked_in(w, h)).or_else(|| {
                w.groups[at..]
                    .iter()
                    .find(docked)
                    .or_else(|| w.groups[..at].iter().rev().find(docked))
                    .map(|g| g.head.id.clone())
            });
        }
    }

    /// Applies a saved session: each window's active tab by title (when that window holds it
    /// docked), and Recent with every key and id re-derived from its URL and store, members whose
    /// URL no longer parses or whose identity repeats in the group dropped.
    pub fn restore(&mut self, active: BTreeMap<String, TabId>, mut recent: Vec<Group>) {
        for w in &mut self.windows {
            w.active = active.get(&w.title).cloned().filter(|a| docked_in(w, a));
        }
        for g in &mut recent {
            let store = g.store.clone();
            g.members.retain(|m| Url::parse(&m.home_url).is_ok());
            rekey(&mut g.head, &store);
            let mut seen = vec![g.head.key.clone()];
            for m in &mut g.members {
                rekey(m, &store);
            }
            g.members.retain(|m| {
                let fresh = !seen.contains(&m.key);
                if fresh {
                    seen.push(m.key.clone());
                }
                fresh
            });
        }
        recent.truncate(RECENT_CAP);
        self.recent = recent;
    }

    /// Every open tab, in every window.
    pub fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.windows
            .iter()
            .flat_map(|w| w.groups.iter())
            .flat_map(|g| std::iter::once(&g.head).chain(g.members.iter()))
    }

    /// The open tab (head or member, detached or not) with identity `key`.
    fn open_doc(&self, key: &DocKey) -> Option<TabId> {
        self.tabs().find(|t| &t.key == key).map(|t| t.id.clone())
    }

    /// The window and group index holding tab `id`.
    fn locate(&self, id: &TabId) -> Option<(usize, usize)> {
        self.windows.iter().enumerate().find_map(|(wi, w)| {
            w.groups
                .iter()
                .position(|g| &g.head.id == id || g.members.iter().any(|m| &m.id == id))
                .map(|gi| (wi, gi))
        })
    }

    /// The index of the window titled `title` (compared trimmed).
    pub fn window_index(&self, title: &str) -> Option<usize> {
        self.windows.iter().position(|w| w.title == title.trim())
    }

    /// Makes `id` the active tab of its window, expanding its group if it is a member of a
    /// collapsed one. A detached tab is not made active; its window is asked for
    /// ([`Workspace::take_raise`]).
    pub fn activate(&mut self, id: &TabId) {
        let Some((wi, gi)) = self.locate(id) else {
            return;
        };
        if self.find(id).is_some_and(|(_, _, t)| t.detached) {
            self.raise.0 = Some(id.clone());
            return;
        }
        let w = &mut self.windows[wi];
        let g = &mut w.groups[gi];
        if &g.head.id != id {
            g.collapsed = false;
        }
        w.active = Some(id.clone());
    }

    /// Whether `id` is the tab its window shows.
    pub fn is_active(&self, id: &TabId) -> bool {
        self.windows.iter().any(|w| w.active.as_ref() == Some(id))
    }

    /// Leaves the window showing `id` with nothing active.
    pub fn deactivate(&mut self, id: &TabId) {
        for w in &mut self.windows {
            if w.active.as_ref() == Some(id) {
                w.active = None;
            }
        }
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

    /// Opens `url` as a new group head in `store` at the end of window `window`. Raises instead
    /// when a tab with the same identity (`identity`) is open in any window. `None` when there is
    /// no such window.
    pub fn new_group(&mut self, window: &str, url: &Url, store: &str) -> Option<Opened> {
        if let Some(id) = self.open_doc(&identity(store, url)) {
            return Some(self.raise(id));
        }
        let wi = self.window_index(window)?;
        let head = tab(url, url.as_str(), store, false);
        let id = head.id.clone();
        self.windows[wi].groups.push(Group {
            head,
            members: vec![],
            collapsed: false,
            store: store.to_string(),
        });
        self.activate(&id);
        Some(Opened::Added(id))
    }

    /// Opens document `url` from tab `from`, joining `from`'s group at the end. `None` when `url`
    /// is not a document or `from` is not open. An already-open document is raised wherever it
    /// is, in whichever window.
    pub fn open_from(&mut self, from: &TabId, url: &Url) -> Option<Opened> {
        let (wi, gi) = self.locate(from)?;
        let store = self.windows[wi].groups[gi].store.clone();
        let key = doc_key(&store, url)?;
        if let Some(id) = self.open_doc(&key) {
            return Some(self.raise(id));
        }
        let t = tab(url, url.as_str(), &store, false);
        let id = t.id.clone();
        self.windows[wi].groups[gi].members.push(t);
        self.activate(&id);
        Some(Opened::Added(id))
    }

    /// The tab `shell_core::tabs::pick_live_neighbour` picks for member `at` of group `gi` in
    /// window `wi`, over `[head, members...]` with every docked tab eligible.
    fn member_neighbour(&self, wi: usize, gi: usize, at: usize) -> Option<TabId> {
        let g = &self.windows[wi].groups[gi];
        let eligible: Vec<bool> = std::iter::once(&g.head)
            .chain(g.members.iter())
            .map(|t| !t.detached)
            .collect();
        pick_live_neighbour(at + 1, &eligible).map(|i| match i {
            0 => g.head.id.clone(),
            i => g.members[i - 1].id.clone(),
        })
    }

    /// Closes a tab. Closing a head closes its group (`close_group` then picks the neighbouring
    /// group's head). Closing its window's active member activates [`Self::member_neighbour`],
    /// else none.
    pub fn close_tab(&mut self, id: &TabId) {
        let Some((wi, gi)) = self.locate(id) else {
            return;
        };
        if &self.windows[wi].groups[gi].head.id == id {
            return self.close_group(id);
        }
        let at = self.windows[wi].groups[gi]
            .members
            .iter()
            .position(|m| &m.id == id)
            .unwrap();
        let neighbour = self.member_neighbour(wi, gi, at);
        self.windows[wi].groups[gi].members.remove(at);
        if self.windows[wi].active.as_ref() == Some(id) {
            self.windows[wi].active = None;
            if let Some(n) = neighbour {
                self.activate(&n);
            }
        }
    }

    /// Closes the group headed by `head` into the front of `recent`. If its window's active tab
    /// was in it, the nearest following group's docked head (else the nearest preceding one's)
    /// becomes active, else none.
    pub fn close_group(&mut self, head: &TabId) {
        let Some((wi, gi)) = self.locate(head) else {
            return;
        };
        let w = &mut self.windows[wi];
        if &w.groups[gi].head.id != head {
            return;
        }
        let group = w.groups.remove(gi);
        let was_active = w
            .active
            .as_ref()
            .is_some_and(|a| a == &group.head.id || group.members.iter().any(|m| &m.id == a));
        if was_active {
            let docked = |g: &&Group| !g.head.detached;
            w.active = w.groups[gi..]
                .iter()
                .find(docked)
                .or_else(|| w.groups[..gi].iter().rev().find(docked))
                .map(|g| g.head.id.clone());
        }
        self.recent.insert(0, group);
        self.recent.truncate(RECENT_CAP);
    }

    /// Restores `recent[index]` as a group at the end of window `window` and activates its head.
    /// Members whose document is now open elsewhere are left where they are. `None` (the entry
    /// kept) when there is no such entry or window.
    ///
    /// If the head's identity is now open elsewhere, nothing is duplicated and that tab is raised
    /// and returned: when it is a head, the entry's other members join its group and the entry
    /// leaves Recent; when it is a member, the entry stays in Recent untouched.
    pub fn reopen_recent(&mut self, window: &str, index: usize) -> Option<TabId> {
        let entry = self.recent.get(index)?;
        if let Some(occupant) = self.open_doc(&entry.head.key) {
            let (wi, gi) = self.locate(&occupant)?;
            if self.windows[wi].groups[gi].head.id != occupant {
                self.raise(occupant.clone());
                return Some(occupant);
            }
            let entry = self.recent.remove(index);
            let members = self.restorable(entry.members, &[entry.head.key]);
            self.windows[wi].groups[gi].members.extend(members);
            self.raise(occupant.clone());
            return Some(occupant);
        }
        let wi = self.window_index(window)?;
        let mut group = self.recent.remove(index);
        group.members = self.restorable(group.members, &[group.head.key.clone()]);
        group.head.detached = false;
        let id = group.head.id.clone();
        self.windows[wi].groups.push(group);
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

    /// Lifts a member into a new group headed by it (same store), at the end of its window, where
    /// the config appends the new group.
    pub fn make_own_group(&mut self, member: &TabId) {
        let Some((wi, gi)) = self.locate(member) else {
            return;
        };
        let groups = &mut self.windows[wi].groups;
        let Some(at) = groups[gi].members.iter().position(|m| &m.id == member) else {
            return;
        };
        let head = groups[gi].members.remove(at);
        let store = groups[gi].store.clone();
        groups.push(Group {
            head,
            members: vec![],
            collapsed: false,
            store,
        });
        self.activate(member);
    }

    /// Moves a member to the end of the group `head` belongs to, in whichever window. A move into
    /// a group on another store re-keys the document to that store (and so gives it that key's
    /// id), and is refused if it is already open there. An active member stays active, in its new
    /// window; the window it left shows [`Self::member_neighbour`].
    pub fn move_to_group(&mut self, member: &TabId, head: &TabId) {
        let (Some((fw, fg)), Some((tw, tg))) = (self.locate(member), self.locate(head)) else {
            return;
        };
        if (fw, fg) == (tw, tg) {
            return;
        }
        let Some(at) = self.windows[fw].groups[fg]
            .members
            .iter()
            .position(|m| &m.id == member)
        else {
            return;
        };
        let mut moved = self.windows[fw].groups[fg].members[at].clone();
        let store = self.windows[tw].groups[tg].store.clone();
        if self.windows[fw].groups[fg].store != store {
            rekey(&mut moved, &store);
            if self.open_doc(&moved.key).is_some() {
                return;
            }
        }
        let was_active = self.windows[fw].active.as_ref() == Some(member);
        let neighbour = (was_active && fw != tw)
            .then(|| self.member_neighbour(fw, fg, at))
            .flatten();
        self.windows[fw].groups[fg].members.remove(at);
        let id = moved.id.clone();
        self.windows[tw].groups[tg].members.push(moved);
        if was_active {
            if fw != tw {
                self.windows[fw].active = None;
                if let Some(n) = neighbour {
                    self.activate(&n);
                }
            }
            self.activate(&id);
        }
    }

    fn tab_mut(&mut self, id: &TabId) -> Option<&mut Tab> {
        self.windows
            .iter_mut()
            .flat_map(|w| w.groups.iter_mut())
            .find_map(|g| {
                if &g.head.id == id {
                    Some(&mut g.head)
                } else {
                    g.members.iter_mut().find(|m| &m.id == id)
                }
            })
    }

    pub fn set_collapsed(&mut self, head: &TabId, collapsed: bool) {
        if let Some(g) = self
            .windows
            .iter_mut()
            .flat_map(|w| w.groups.iter_mut())
            .find(|g| &g.head.id == head)
        {
            g.collapsed = collapsed;
        }
    }

    pub fn set_title(&mut self, id: &TabId, title: &str) {
        if let Some(t) = self.tab_mut(id) {
            t.title = title.to_string();
        }
    }

    /// Detaching its window's active tab leaves that window with nothing active; the shell picks
    /// what it shows next before detaching.
    pub fn set_detached(&mut self, id: &TabId, detached: bool) {
        if let Some(t) = self.tab_mut(id) {
            t.detached = detached;
            if detached {
                self.deactivate(id);
            }
        }
    }

    /// The window index, group and tab of `id`.
    pub fn find(&self, id: &TabId) -> Option<(usize, &Group, &Tab)> {
        self.windows.iter().enumerate().find_map(|(wi, w)| {
            w.groups.iter().find_map(|g| {
                std::iter::once(&g.head)
                    .chain(g.members.iter())
                    .find(|t| &t.id == id)
                    .map(|t| (wi, g, t))
            })
        })
    }

    pub fn group_of(&self, id: &TabId) -> Option<&Group> {
        self.find(id).map(|(_, g, _)| g)
    }
}

/// Why Set Home to Current Page can't make the page tab is on its home.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetHomeBlock {
    /// The tab is on its home already (or has no page loaded, which is the same thing).
    AtHome,
    /// The page is not an http(s) page.
    NotWeb,
    /// The page is a document open in another tab: making it home would open it twice.
    OpenElsewhere,
}

impl Workspace {
    /// Why tab `id`, showing `current` (`None`: nothing loaded), can't make `current` its home,
    /// if it can't.
    pub fn set_home_blocked(&self, id: &TabId, current: Option<&Url>) -> Option<SetHomeBlock> {
        let (_, g, t) = self.find(id)?;
        // At home when the page is its home, by `head_key`: an address the page decorated on
        // load (`?tab=…`, `#heading=…`, `/u/0/`) is still home; another view of it is not.
        let home = Url::parse(&t.home_url).map(|h| head_key(&h));
        let at_home = |c: &&Url| {
            home.as_ref()
                .map_or(c.as_str() == t.home_url, |h| *h == head_key(c))
        };
        let Some(current) = current.filter(|c| !at_home(c)) else {
            return Some(SetHomeBlock::AtHome);
        };
        if !matches!(current.scheme(), "http" | "https") {
            return Some(SetHomeBlock::NotWeb);
        }
        self.open_doc(&identity(&g.store, current))
            .filter(|other| other != id)
            .map(|_| SetHomeBlock::OpenElsewhere)
    }

    /// Makes `url` tab `id`'s home: its `home_url`, and so its key and id, change in place. It
    /// keeps its title (a title that is still the old URL becomes the new one), its place, whether
    /// it is detached, and its window's selection; a head keeps its members. Returns the tab's new
    /// id.
    pub fn set_home(&mut self, id: &TabId, url: &Url) -> Result<TabId, SetHomeBlock> {
        if let Some(block) = self
            .set_home_blocked(id, Some(url))
            .filter(|b| *b != SetHomeBlock::AtHome)
        {
            return Err(block);
        }
        let (wi, gi) = self.locate(id).ok_or(SetHomeBlock::AtHome)?;
        let store = self.windows[wi].groups[gi].store.clone();
        let fresh = tab(url, "", &store, false);
        let t = self.tab_mut(id).expect("located");
        // A title that is still the URL moves with it, so the page's title can still replace it.
        if t.title == t.home_url {
            t.title = fresh.home_url.clone();
        }
        t.key = fresh.key;
        t.id = fresh.id.clone();
        t.home_url = fresh.home_url;
        let w = &mut self.windows[wi];
        if w.active.as_ref() == Some(id) {
            w.active = Some(fresh.id.clone());
        }
        Ok(fresh.id)
    }
}

/// Whether window `w` holds `id` as a docked tab.
fn docked_in(w: &WindowWs, id: &TabId) -> bool {
    w.groups
        .iter()
        .flat_map(|g| std::iter::once(&g.head).chain(g.members.iter()))
        .any(|t| &t.id == id && !t.detached)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: &str = "W";

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

    fn window(title: &str) -> ConfigWindow {
        ConfigWindow {
            title: title.into(),
            groups: vec![],
        }
    }

    /// One empty window, `W`.
    fn ws() -> Workspace {
        Workspace::from_config(&[window(W)])
    }

    fn head_of(w: &mut Workspace, n: u32) -> TabId {
        id(w.new_group(W, &folder(n), "default").unwrap())
    }

    fn member_ids(w: &Workspace, head: &TabId) -> Vec<TabId> {
        w.group_of(head)
            .unwrap()
            .members
            .iter()
            .map(|t| t.id.clone())
            .collect()
    }

    fn active(w: &Workspace) -> Option<TabId> {
        w.windows[0].active.clone()
    }

    fn groups(w: &Workspace) -> &[Group] {
        &w.windows[0].groups
    }

    fn cgroup(name: &str, url: &Url, store: &str, tabs: &[(&str, &Url)]) -> ConfigGroup {
        ConfigGroup {
            name: name.into(),
            url: url.to_string(),
            store: store.into(),
            collapsed: false,
            detached: false,
            tabs: tabs
                .iter()
                .map(|(t, u)| ConfigTab {
                    title: (*t).into(),
                    url: u.to_string(),
                    detached: false,
                })
                .collect(),
        }
    }

    /// Window A: group "One" (folder 1; docs 1, 2). Window B: group "Two" (folder 2; doc 3).
    fn two_windows() -> Vec<ConfigWindow> {
        vec![
            ConfigWindow {
                title: "A".into(),
                groups: vec![cgroup(
                    "One",
                    &folder(1),
                    "default",
                    &[("Budget", &doc(1)), ("Minutes", &doc(2))],
                )],
            },
            ConfigWindow {
                title: "B".into(),
                groups: vec![cgroup("Two", &folder(2), "default", &[("Roster", &doc(3))])],
            },
        ]
    }

    #[test]
    fn from_config_never_drops_a_head_for_a_duplicate_member() {
        // Window A lists doc 3 as a member; window B, later, heads a group with it.
        let mut cfg = two_windows();
        cfg[0].groups[0].tabs.push(ConfigTab {
            title: "Dup".into(),
            url: doc(3).to_string(),
            detached: false,
        });
        cfg[1].groups.push(cgroup("Three", &doc(4), "default", &[]));
        cfg[1].groups[1].url = doc(3).to_string();
        cfg[1].groups[0].tabs.clear();
        let w = Workspace::from_config(&cfg);
        assert_eq!(w.windows[1].groups[1].head.title, "Three");
        assert_eq!(w.windows[0].groups[0].members.len(), 2);
    }

    #[test]
    fn from_config_keeps_the_configs_url_text_and_head_detached() {
        let mut cfg = two_windows();
        cfg[0].groups[0].url = "https://example.com".into();
        cfg[0].groups[0].detached = true;
        let w = Workspace::from_config(&cfg);
        let head = &w.windows[0].groups[0].head;
        assert_eq!(head.home_url, "https://example.com");
        assert!(head.detached);
    }

    #[test]
    fn rebase_falls_back_to_a_neighbour_when_the_active_tab_goes() {
        let mut w = Workspace::from_config(&two_windows());
        let minutes = w.windows[0].groups[0].members[1].id.clone();
        let one = w.windows[0].groups[0].head.id.clone();
        w.activate(&minutes);
        // A hand edit removes Minutes: its group's head takes over.
        let mut cfg = two_windows();
        cfg[0].groups[0].tabs.pop();
        w.rebase(Workspace::from_config(&cfg));
        assert_eq!(w.windows[0].active, Some(one));
        // A hand edit removes the whole group: the next group's head takes over.
        cfg[0]
            .groups
            .push(cgroup("Later", &folder(7), "default", &[]));
        w.rebase(Workspace::from_config(&cfg));
        cfg[0].groups.remove(0);
        w.rebase(Workspace::from_config(&cfg));
        assert_eq!(
            w.windows[0].active,
            Some(tab_id(&identity("default", &folder(7))))
        );
    }

    #[test]
    fn from_config_builds_windows_groups_and_titles() {
        let w = Workspace::from_config(&two_windows());
        assert_eq!(w.windows.len(), 2);
        assert_eq!(w.windows[0].title, "A");
        let g = &w.windows[0].groups[0];
        assert_eq!(g.head.title, "One");
        assert_eq!(g.head.home_url, folder(1).to_string());
        assert_eq!(g.store, "default");
        let titles: Vec<_> = g.members.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titles, ["Budget", "Minutes"]);
        assert_eq!(g.members[0].id, tab_id(&identity("default", &doc(1))));
        assert!(w.windows.iter().all(|w| w.active.is_none()));
    }

    #[test]
    fn ids_are_stable_across_reloads_and_reorders() {
        let first = Workspace::from_config(&two_windows());
        let mut reordered = two_windows();
        reordered.reverse();
        reordered[1].groups[0].tabs.reverse();
        // The Roster moves from window B to window A.
        let roster = reordered[0].groups[0].tabs.remove(0);
        reordered[1].groups[0].tabs.push(roster);
        let second = Workspace::from_config(&reordered);
        let ids = |w: &Workspace| {
            let mut v: Vec<TabId> = w.tabs().map(|t| t.id.clone()).collect();
            v.sort();
            v
        };
        assert_eq!(ids(&first), ids(&second));
        assert_eq!(ids(&first).len(), 5);
        assert_eq!(
            Workspace::from_config(&two_windows()),
            first,
            "a reload of the same config is the same workspace"
        );
    }

    #[test]
    fn opening_a_doc_open_in_another_window_raises_it_there() {
        let mut w = Workspace::from_config(&two_windows());
        let one = w.windows[0].groups[0].head.id.clone();
        let roster = w.windows[1].groups[0].members[0].id.clone();
        w.activate(&one);
        assert_eq!(
            w.open_from(&one, &doc(3)),
            Some(Opened::Raised(roster.clone()))
        );
        assert_eq!(w.windows[1].active, Some(roster));
        assert_eq!(w.windows[0].active, Some(one.clone()));
        assert_eq!(member_ids(&w, &one).len(), 2);
        // A new group on a document open in another window raises it there too.
        let budget = w.windows[0].groups[0].members[0].id.clone();
        assert_eq!(
            w.new_group("B", &doc(1), "default"),
            Some(Opened::Raised(budget.clone()))
        );
        assert_eq!(w.windows[0].active, Some(budget));
        assert_eq!(w.windows[1].groups.len(), 1);
    }

    #[test]
    fn rebase_keeps_active_when_the_tab_survives() {
        let mut w = Workspace::from_config(&two_windows());
        let minutes = w.windows[0].groups[0].members[1].id.clone();
        let roster = w.windows[1].groups[0].members[0].id.clone();
        w.activate(&minutes);
        w.activate(&roster);
        w.close_group(&w.windows[1].groups[0].head.id.clone());
        assert_eq!(w.recent.len(), 1);
        // The reload drops the Roster from B and renames Minutes.
        let mut cfg = two_windows();
        cfg[0].groups[0].tabs[1].title = "Minutes 2026".into();
        cfg[1].groups[0].tabs.clear();
        w.rebase(Workspace::from_config(&cfg));
        assert_eq!(w.windows[0].active, Some(minutes.clone()));
        assert_eq!(w.find(&minutes).unwrap().2.title, "Minutes 2026");
        assert_eq!(w.windows[1].active, None);
        assert_eq!(
            w.recent.len(),
            1,
            "Recent is the session's, not the config's"
        );
        // A tab that went away hands over to its group's head.
        let mut cfg = two_windows();
        cfg[0].groups[0].tabs.clear();
        w.rebase(Workspace::from_config(&cfg));
        assert_eq!(
            w.windows[0].active,
            Some(w.windows[0].groups[0].head.id.clone())
        );
    }

    #[test]
    fn restore_applies_active_by_window_title_and_rekeys_recent() {
        let mut w = Workspace::from_config(&two_windows());
        let budget = w.windows[0].groups[0].members[0].id.clone();
        let mut recent = Workspace::from_config(&[ConfigWindow {
            title: "X".into(),
            groups: vec![cgroup("Old", &folder(9), "default", &[("D", &doc(9))])],
        }])
        .windows[0]
            .groups
            .clone();
        recent[0].head.key.id = "bogus".into();
        recent[0].head.id = "t-bogus".into();
        let active = BTreeMap::from([
            ("A".to_string(), budget.clone()),
            ("B".to_string(), budget.clone()), // not in B: ignored
            ("Gone".to_string(), budget.clone()),
        ]);
        w.restore(active, recent);
        assert_eq!(w.windows[0].active, Some(budget));
        assert_eq!(w.windows[1].active, None);
        assert_eq!(w.recent[0].head.key, identity("default", &folder(9)));
        assert_eq!(w.recent[0].head.id, tab_id(&w.recent[0].head.key));
    }

    #[test]
    fn opening_a_doc_adds_it_under_the_group() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        let d = w.open_from(&h, &doc(1)).unwrap();
        assert!(matches!(d, Opened::Added(_)));
        assert_eq!(member_ids(&w, &h), vec![id(d.clone())]);
        assert_eq!(active(&w), Some(id(d)));
    }

    #[test]
    fn reopening_an_open_doc_raises_it_even_from_another_group() {
        let mut w = ws();
        let h1 = head_of(&mut w, 1);
        let h2 = head_of(&mut w, 2);
        let d = id(w.open_from(&h1, &doc(1)).unwrap());
        w.activate(&h2);
        assert_eq!(w.open_from(&h2, &doc(1)), Some(Opened::Raised(d.clone())));
        assert_eq!(active(&w), Some(d));
        assert!(member_ids(&w, &h2).is_empty());
    }

    #[test]
    fn a_detached_doc_still_counts_as_open() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        let d = id(w.open_from(&h, &doc(1)).unwrap());
        w.set_detached(&d, true);
        assert_eq!(w.open_from(&h, &doc(1)), Some(Opened::Raised(d)));
        assert_eq!(member_ids(&w, &h).len(), 1);
    }

    #[test]
    fn open_from_a_member_joins_the_same_group_flat() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        let a = id(w.open_from(&h, &doc(1)).unwrap());
        let b = id(w.open_from(&a, &doc(2)).unwrap());
        assert_eq!(member_ids(&w, &h), vec![a, b]);
        assert_eq!(groups(&w).len(), 1);
    }

    #[test]
    fn non_document_url_is_not_a_member() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        assert_eq!(w.open_from(&h, &folder(2)), None);
        assert!(member_ids(&w, &h).is_empty());
    }

    #[test]
    fn new_group_on_an_open_head_raises_it() {
        let mut w = ws();
        let h1 = head_of(&mut w, 1);
        let _h2 = head_of(&mut w, 2);
        assert_eq!(
            w.new_group(W, &folder(1), "default"),
            Some(Opened::Raised(h1.clone()))
        );
        assert_eq!(groups(&w).len(), 2);
        assert_eq!(active(&w), Some(h1.clone()));
        // an open member's doc raises the member
        let d = id(w.open_from(&h1, &doc(1)).unwrap());
        assert_eq!(w.new_group(W, &doc(1), "default"), Some(Opened::Raised(d)));
        assert_eq!(groups(&w).len(), 2);
        // a window that does not exist takes nothing
        assert_eq!(w.new_group("nowhere", &folder(5), "default"), None);
    }

    #[test]
    fn a_document_head_occupies_its_doc_key() {
        let mut w = ws();
        let h1 = id(w.new_group(W, &doc(1), "default").unwrap());
        let h2 = head_of(&mut w, 2);
        assert_eq!(w.open_from(&h2, &doc(1)), Some(Opened::Raised(h1)));
    }

    #[test]
    fn closing_the_head_closes_the_group_to_recent() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        let d = id(w.open_from(&h, &doc(1)).unwrap());
        w.close_tab(&h);
        assert!(groups(&w).is_empty());
        assert_eq!(w.recent.len(), 1);
        assert_eq!(w.recent[0].head.id, h);
        assert_eq!(w.recent[0].members[0].id, d);
        assert_eq!(active(&w), None);
    }

    #[test]
    fn recent_is_capped_at_20_newest_first() {
        let mut w = ws();
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
        let mut w = ws();
        let h1 = head_of(&mut w, 1);
        w.open_from(&h1, &doc(1)).unwrap();
        let keep = id(w.open_from(&h1, &doc(2)).unwrap());
        w.close_group(&h1);
        let h2 = head_of(&mut w, 2);
        let elsewhere = id(w.open_from(&h2, &doc(1)).unwrap());
        let back = w.reopen_recent(W, 0).unwrap();
        assert_eq!(back, h1);
        assert!(w.recent.is_empty());
        assert_eq!(member_ids(&w, &h1), vec![keep]);
        assert_eq!(member_ids(&w, &h2), vec![elsewhere]);
        assert_eq!(active(&w), Some(h1));
    }

    #[test]
    fn reopen_recent_of_a_head_open_elsewhere_raises_it() {
        let mut w = ws();
        let h1 = head_of(&mut w, 1);
        w.close_group(&h1);
        let h2 = head_of(&mut w, 1);
        assert_eq!(w.reopen_recent(W, 0), Some(h2.clone()));
        assert_eq!(groups(&w).len(), 1);
        assert_eq!(active(&w), Some(h2));
        assert!(w.recent.is_empty());
    }

    #[test]
    fn reopen_recent_into_another_window() {
        let mut w = Workspace::from_config(&[window("A"), window("B")]);
        let h = id(w.new_group("A", &folder(1), "default").unwrap());
        w.close_group(&h);
        assert_eq!(w.reopen_recent("nowhere", 0), None);
        assert_eq!(w.recent.len(), 1);
        assert_eq!(w.reopen_recent("B", 0), Some(h.clone()));
        assert_eq!(w.windows[1].groups[0].head.id, h);
        assert_eq!(w.windows[1].active, Some(h));
    }

    #[test]
    fn forget_recent_drops_the_entry() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        w.close_group(&h);
        w.forget_recent(0);
        w.forget_recent(9);
        assert!(w.recent.is_empty());
    }

    #[test]
    fn make_own_group_lifts_a_member_to_a_head() {
        let mut w = ws();
        let h = id(w.new_group(W, &folder(1), "work").unwrap());
        let d = id(w.open_from(&h, &doc(1)).unwrap());
        head_of(&mut w, 2);
        w.make_own_group(&d);
        assert_eq!(groups(&w).len(), 3);
        assert!(member_ids(&w, &h).is_empty());
        // At the end of the window, where the config appends it.
        assert_eq!(groups(&w)[2].head.id, d);
        assert_eq!(groups(&w)[2].store, "work");
        assert_eq!(w.open_from(&h, &doc(1)), Some(Opened::Raised(d)));
    }

    #[test]
    fn move_to_group_moves_a_member() {
        let mut w = ws();
        let h1 = head_of(&mut w, 1);
        let h2 = head_of(&mut w, 2);
        let d = id(w.open_from(&h1, &doc(1)).unwrap());
        w.move_to_group(&d, &h2);
        assert!(member_ids(&w, &h1).is_empty());
        assert_eq!(member_ids(&w, &h2), vec![d.clone()]);
        assert_eq!(w.open_from(&h1, &doc(1)), Some(Opened::Raised(d)));
    }

    #[test]
    fn moving_the_active_member_to_another_window_follows_it_there() {
        let mut w = Workspace::from_config(&two_windows());
        let budget = w.windows[0].groups[0].members[0].id.clone();
        let one = w.windows[0].groups[0].head.id.clone();
        let two = w.windows[1].groups[0].head.id.clone();
        w.activate(&budget);
        w.move_to_group(&budget, &two);
        assert_eq!(member_ids(&w, &two).last(), Some(&budget));
        assert_eq!(w.windows[1].active, Some(budget));
        // The window it left shows the tab the member's close would have picked.
        assert_eq!(w.windows[0].active, Some(one));
    }

    #[test]
    fn activating_a_member_of_a_collapsed_group_expands_it() {
        let mut w = ws();
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
        let mut w = ws();
        let h = head_of(&mut w, 1);
        let a = id(w.open_from(&h, &doc(1)).unwrap());
        let b = id(w.open_from(&h, &doc(2)).unwrap());
        let c = id(w.open_from(&h, &doc(3)).unwrap());
        w.activate(&b);
        w.close_tab(&b);
        assert_eq!(active(&w), Some(a.clone())); // leans up
        w.activate(&c);
        w.close_tab(&c);
        assert_eq!(active(&w), Some(a.clone()));
        w.close_tab(&a);
        assert_eq!(active(&w), Some(h)); // the head is above the first member
    }

    #[test]
    fn closing_an_inactive_tab_keeps_the_active_one() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        let a = id(w.open_from(&h, &doc(1)).unwrap());
        w.activate(&h);
        w.close_tab(&a);
        assert_eq!(active(&w), Some(h));
    }

    #[test]
    fn closing_in_one_window_leaves_the_others_active() {
        let mut w = Workspace::from_config(&two_windows());
        let roster = w.windows[1].groups[0].members[0].id.clone();
        let one = w.windows[0].groups[0].head.id.clone();
        w.activate(&roster);
        w.activate(&one);
        w.close_group(&one);
        assert_eq!(w.windows[0].active, None);
        assert_eq!(w.windows[1].active, Some(roster));
    }

    #[test]
    fn titles_and_serialization_round_trip() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        w.set_title(&h, "Projects");
        let g = w.group_of(&h).unwrap().clone();
        let s = serde_json::to_string(&g).unwrap();
        let back: Group = serde_json::from_str(&s).unwrap();
        assert_eq!(back.head.title, "Projects");
        assert_eq!(back.store, "default");
        let n = head_of(&mut w, 2);
        assert_ne!(n, h);
    }

    #[test]
    fn identity_is_scoped_by_store_not_by_role() {
        let mut w = ws();
        let h1 = id(w.new_group(W, &doc(1), "default").unwrap());
        let work = id(w.new_group(W, &folder(2), "work").unwrap());
        // same doc, other store: a second tab is allowed
        let m = id(w.open_from(&work, &doc(1)).unwrap());
        assert_ne!(m, h1);
        w.make_own_group(&m);
        assert_eq!(groups(&w).len(), 3);
        // same doc, same store: raised, whether the tab is a head or a member
        assert_eq!(w.new_group(W, &doc(1), "work"), Some(Opened::Raised(m)));
        assert_eq!(
            w.new_group(W, &doc(1), "default"),
            Some(Opened::Raised(h1.clone()))
        );
        assert_eq!(
            w.new_group(W, &folder(2), "work"),
            Some(Opened::Raised(work))
        );
        // the same folder in another store is its own head
        assert!(matches!(
            w.new_group(W, &folder(2), "default"),
            Some(Opened::Added(_))
        ));
    }

    #[test]
    fn reopen_recent_with_head_open_elsewhere_merges_members_into_it() {
        let mut w = ws();
        let h1 = head_of(&mut w, 1);
        w.open_from(&h1, &doc(1)).unwrap();
        w.open_from(&h1, &doc(2)).unwrap();
        w.close_group(&h1);
        let h2 = head_of(&mut w, 1);
        let shared = id(w.open_from(&h2, &doc(1)).unwrap());
        assert_eq!(w.reopen_recent(W, 0), Some(h2.clone()));
        assert!(w.recent.is_empty());
        let members = member_ids(&w, &h2);
        assert_eq!(members.len(), 2);
        assert_eq!(members[0], shared);
        assert_eq!(active(&w), Some(h2));
    }

    #[test]
    fn reopen_recent_with_head_open_as_a_member_leaves_the_entry() {
        let mut w = ws();
        let h1 = id(w.new_group(W, &doc(5), "default").unwrap());
        let kept = id(w.open_from(&h1, &doc(1)).unwrap());
        w.close_group(&h1);
        let h2 = head_of(&mut w, 2);
        let m = id(w.open_from(&h2, &doc(5)).unwrap());
        assert_eq!(w.reopen_recent(W, 0), Some(m.clone()));
        assert_eq!(w.recent.len(), 1);
        assert_eq!(w.recent[0].members[0].id, kept);
        assert_eq!(groups(&w).len(), 1);
        assert_eq!(active(&w), Some(m));
    }

    #[test]
    fn closing_the_active_member_of_a_collapsed_group_expands_it() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        let a = id(w.open_from(&h, &doc(1)).unwrap());
        let b = id(w.open_from(&h, &doc(2)).unwrap());
        w.set_collapsed(&h, true);
        w.close_tab(&b);
        assert_eq!(active(&w), Some(a));
        assert!(!w.group_of(&h).unwrap().collapsed);
    }

    #[test]
    fn moving_the_active_member_into_a_collapsed_group_expands_it() {
        let mut w = ws();
        let h1 = head_of(&mut w, 1);
        let h2 = head_of(&mut w, 2);
        let d = id(w.open_from(&h1, &doc(1)).unwrap());
        w.set_collapsed(&h2, true);
        w.move_to_group(&d, &h2);
        assert_eq!(active(&w), Some(d));
        assert!(!w.group_of(&h2).unwrap().collapsed);
    }

    #[test]
    fn moving_onto_its_own_group_is_a_no_op() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        let a = id(w.open_from(&h, &doc(1)).unwrap());
        let b = id(w.open_from(&h, &doc(2)).unwrap());
        w.move_to_group(&a, &h);
        w.move_to_group(&a, &b);
        assert_eq!(member_ids(&w, &h), vec![a, b]);
    }

    #[test]
    fn make_own_group_of_a_head_is_a_no_op() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        w.open_from(&h, &doc(1)).unwrap();
        let before = w.clone();
        w.make_own_group(&h);
        assert_eq!(w, before);
    }

    #[test]
    fn moving_across_stores_rekeys_or_refuses_on_collision() {
        let mut w = ws();
        let h1 = head_of(&mut w, 1);
        let work = id(w.new_group(W, &folder(2), "work").unwrap());
        let d = id(w.open_from(&h1, &doc(1)).unwrap());
        w.move_to_group(&d, &work);
        // Re-keyed into the work store, the tab's id is that key's.
        let moved = tab_id(&identity("work", &doc(1)));
        assert_eq!(w.find(&moved).unwrap().2.key.store, "work");
        assert_eq!(member_ids(&w, &work), vec![moved.clone()]);
        assert_eq!(active(&w), Some(moved.clone()));
        // doc 2 is open in both stores: moving the default copy into work collides
        let e = id(w.open_from(&h1, &doc(2)).unwrap());
        let e2 = id(w.open_from(&work, &doc(2)).unwrap());
        w.move_to_group(&e, &work);
        assert_eq!(member_ids(&w, &h1), vec![e]);
        assert_eq!(member_ids(&w, &work), vec![moved, e2]);
    }

    #[test]
    fn closing_the_active_group_activates_the_neighbouring_head() {
        let mut w = ws();
        let h1 = head_of(&mut w, 1);
        let h2 = head_of(&mut w, 2);
        let h3 = head_of(&mut w, 3);
        w.activate(&h2);
        w.close_group(&h2);
        assert_eq!(active(&w), Some(h3.clone()));
        w.close_group(&h3);
        assert_eq!(active(&w), Some(h1));
    }

    #[test]
    fn the_same_document_gets_the_same_id() {
        let mut w = ws();
        let a = head_of(&mut w, 1);
        w.close_group(&a);
        w.forget_recent(0);
        let b = head_of(&mut w, 2);
        // Closed and opened again, in the same or another workspace: the same id.
        assert_eq!(head_of(&mut w, 1), a);
        assert_eq!(head_of(&mut ws(), 1), a);
        assert_ne!(a, b);
    }

    #[test]
    fn restore_rederives_recent_keys_and_ids_and_drops_bad_members() {
        let mut w = ws();
        let h1 = head_of(&mut w, 1);
        w.open_from(&h1, &doc(1)).unwrap();
        w.open_from(&h1, &doc(2)).unwrap();
        w.close_group(&h1);
        let mut recent = w.recent.clone();
        recent[0].head.key.id = "bogus".into();
        recent[0].members[1].home_url = "not a url".into();
        // a member repeating the first one
        let dup = recent[0].members[0].clone();
        recent[0].members.push(dup);
        let mut fresh = ws();
        fresh.restore(BTreeMap::new(), recent);
        let g = &fresh.recent[0];
        assert_eq!(g.head.key, identity("default", &folder(1)));
        assert_eq!(g.head.id, h1);
        assert_eq!(g.members.len(), 1);
    }

    #[test]
    fn restore_keeps_a_recent_head_whose_url_no_longer_parses() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        w.close_group(&h);
        let mut recent = w.recent.clone();
        recent[0].head.home_url = "also not".into();
        let key = recent[0].head.key.clone();
        let mut fresh = ws();
        fresh.restore(BTreeMap::new(), recent);
        assert_eq!(fresh.recent[0].head.key, key);
    }

    // `active` is never a detached tab.

    /// Group 1: head h1, members a, b. Group 2: head h2. `active` is b.
    fn two_groups() -> (Workspace, TabId, TabId, TabId, TabId) {
        let mut w = ws();
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
        assert_eq!(active(&w), Some(b));
        // Nor does it expand the group the docked tab would have been shown in.
        assert!(groups(&w)[0].collapsed);
        assert_eq!(w.take_raise(), Some(a));
        assert_eq!(w.take_raise(), None);
    }

    #[test]
    fn raising_a_detached_doc_asks_for_its_window() {
        let (mut w, h1, a, b, _) = two_groups();
        w.set_detached(&a, true);
        assert_eq!(w.open_from(&h1, &doc(1)), Some(Opened::Raised(a.clone())));
        assert_eq!(active(&w), Some(b));
        assert_eq!(w.take_raise(), Some(a));
    }

    #[test]
    fn detaching_the_active_tab_clears_active() {
        let (mut w, _, _, b, _) = two_groups();
        w.set_detached(&b, true);
        assert_eq!(active(&w), None);
    }

    #[test]
    fn making_a_detached_member_its_own_group_keeps_active() {
        let (mut w, _, a, b, _) = two_groups();
        w.set_detached(&a, true);
        w.make_own_group(&a);
        assert_eq!(groups(&w)[2].head.id, a);
        assert_eq!(active(&w), Some(b));
        assert_eq!(w.take_raise(), Some(a));
    }

    #[test]
    fn moving_a_detached_member_keeps_active() {
        let (mut w, _, a, b, h2) = two_groups();
        w.set_detached(&a, true);
        w.move_to_group(&a, &h2);
        assert_eq!(member_ids(&w, &h2), vec![a]);
        assert_eq!(active(&w), Some(b));
        assert_eq!(w.take_raise(), None);
    }

    #[test]
    fn closing_a_tab_skips_a_detached_neighbour() {
        let (mut w, h1, a, b, _) = two_groups();
        w.set_detached(&a, true);
        w.close_tab(&b);
        assert_eq!(active(&w), Some(h1.clone()));
        // Every neighbour detached: nothing is active.
        let (mut w, h1, a, b, _) = two_groups();
        w.set_detached(&h1, true);
        w.set_detached(&a, true);
        w.close_tab(&b);
        assert_eq!(active(&w), None);
    }

    #[test]
    fn closing_a_group_skips_a_group_whose_head_is_detached() {
        let (mut w, h1, _, b, h2) = two_groups();
        let h3 = head_of(&mut w, 3);
        w.activate(&b);
        w.set_detached(&h2, true);
        w.close_group(&h1);
        assert_eq!(active(&w), Some(h3.clone()));
        // Only detached heads remain: nothing is active.
        w.set_detached(&h3, true);
        assert_eq!(active(&w), None);
        let (mut w, h1, _, _, h2) = two_groups();
        w.set_detached(&h2, true);
        w.close_group(&h1);
        assert_eq!(active(&w), None);
    }

    #[test]
    fn loading_clears_an_active_detached_tab() {
        let (mut w, _, _, b, _) = two_groups();
        w.tab_mut(&b).unwrap().detached = true;
        let mut fresh = w.clone();
        fresh.windows[0].active = None;
        fresh.restore(BTreeMap::from([(W.to_string(), b)]), vec![]);
        assert_eq!(fresh.windows[0].active, None);
    }

    #[test]
    fn deactivate_clears_only_its_own_window() {
        let mut w = Workspace::from_config(&two_windows());
        let one = w.windows[0].groups[0].head.id.clone();
        let two = w.windows[1].groups[0].head.id.clone();
        w.activate(&one);
        w.activate(&two);
        assert!(w.is_active(&one) && w.is_active(&two));
        w.deactivate(&one);
        assert_eq!(w.windows[0].active, None);
        assert_eq!(w.windows[1].active, Some(two));
    }

    #[test]
    fn set_home_carries_a_title_that_is_still_the_url() {
        let mut w = ws();
        let h = head_of(&mut w, 1);
        // Never named by its page: its title is its URL, and moves with it so a title can follow.
        let sub = folder(8);
        let new = w.set_home(&h, &sub).unwrap();
        assert_eq!(w.find(&new).unwrap().2.title, sub.as_str());
        // A real name stays.
        w.set_title(&new, "Newsletters");
        let newer = w.set_home(&new, &folder(9)).unwrap();
        assert_eq!(w.find(&newer).unwrap().2.title, "Newsletters");
    }

    #[test]
    fn set_home_counts_a_page_google_decorated_as_home() {
        let w = Workspace::from_config(&two_windows());
        let budget = w.windows[0].groups[0].members[0].id.clone();
        let loaded = Url::parse(&format!("{}?tab=t.0#heading=h.x", doc(1))).unwrap();
        assert_eq!(
            w.set_home_blocked(&budget, Some(&loaded)),
            Some(SetHomeBlock::AtHome)
        );
    }

    #[test]
    fn set_home_rekeys_the_tab_in_place() {
        let mut w = Workspace::from_config(&two_windows());
        let one = w.windows[0].groups[0].head.id.clone();
        let budget = w.windows[0].groups[0].members[0].id.clone();
        w.activate(&one);
        let sub = folder(8);
        // A head: re-keyed where it is, still active, its members kept.
        let new = w.set_home(&one, &sub).unwrap();
        assert_eq!(new, tab_id(&identity("default", &sub)));
        let g = &w.windows[0].groups[0];
        assert_eq!(
            (g.head.id.clone(), g.head.home_url.clone()),
            (new.clone(), sub.to_string())
        );
        assert_eq!(g.head.title, "One", "the name is left alone");
        assert_eq!(g.members.len(), 2);
        assert_eq!(w.windows[0].active, Some(new));
        // A detached member stays detached, in its place.
        w.set_detached(&budget, true);
        let d9 = doc(9);
        let moved = w.set_home(&budget, &d9).unwrap();
        let m = &w.windows[0].groups[0].members[0];
        assert_eq!((m.id.clone(), m.detached), (moved, true));
        assert!(w.find(&budget).is_none());
    }

    #[test]
    fn set_home_blocks_at_home_off_the_web_and_on_a_doc_open_elsewhere() {
        let mut w = Workspace::from_config(&two_windows());
        let budget = w.windows[0].groups[0].members[0].id.clone();
        let at = |s: &str| Url::parse(s).unwrap();
        assert_eq!(
            w.set_home_blocked(&budget, None),
            Some(SetHomeBlock::AtHome)
        );
        assert_eq!(
            w.set_home_blocked(&budget, Some(&doc(1))),
            Some(SetHomeBlock::AtHome)
        );
        assert_eq!(
            w.set_home_blocked(&budget, Some(&at("about:blank"))),
            Some(SetHomeBlock::NotWeb)
        );
        // Doc 3 is the Roster, open in window B.
        assert_eq!(
            w.set_home_blocked(&budget, Some(&doc(3))),
            Some(SetHomeBlock::OpenElsewhere)
        );
        assert_eq!(
            w.set_home(&budget, &doc(3)),
            Err(SetHomeBlock::OpenElsewhere)
        );
        assert_eq!(w.set_home_blocked(&budget, Some(&doc(7))), None);
        // The same document at another address is still this tab: allowed.
        let view = Url::parse(&doc(1).as_str().replace("/edit", "/view")).unwrap();
        assert_eq!(w.set_home_blocked(&budget, Some(&view)), None);
    }
}
