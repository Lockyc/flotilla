//! Persisting the run state: one JSON file the shell points us at, holding each window's active
//! tab and Incoming queue, and Recent. The windows and groups themselves live in the config. Keys
//! and ids in it are derived data, re-derived when applied (`Workspace::restore`), never trusted
//! from disk.

use crate::model::{Group, Incoming, TabId, Workspace};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SESSION_VERSION: u32 = 2;

/// What a session file keeps: each window's active tab and non-empty Incoming queue by window
/// title (the workspace's `stray` queues among them), Recent, the tabs that were loaded (had
/// a webview), which relaunch loads again, and the Unsorted documents a Drive lookup left
/// waiting, with the reason their row shows, which relaunch and Connect look up again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub active: BTreeMap<String, TabId>,
    pub recent: Vec<Group>,
    #[serde(default)]
    pub incoming: BTreeMap<String, Vec<Incoming>>,
    #[serde(default)]
    pub loaded: BTreeSet<TabId>,
    #[serde(default)]
    pub waiting: BTreeMap<TabId, String>,
}

/// Whether document `id` may keep its waiting reason in `ws`: it is in an Unsorted, or `ws` has no
/// windows (a config that didn't parse), which keeps every reason as it keeps the loaded tabs.
pub fn still_waiting(ws: &Workspace, id: &TabId) -> bool {
    ws.windows.is_empty() || ws.is_unsorted(id)
}

impl Session {
    pub fn of(ws: &Workspace) -> Session {
        Session {
            active: ws
                .windows
                .iter()
                .filter_map(|w| Some((w.title.clone(), w.active.clone()?)))
                .chain(ws.stray_active.clone())
                .collect(),
            recent: ws.recent.clone(),
            incoming: ws
                .windows
                .iter()
                .filter(|w| !w.incoming.is_empty())
                .map(|w| (w.title.clone(), w.incoming.clone()))
                .chain(ws.stray.clone())
                .collect(),
            loaded: BTreeSet::new(),
            waiting: BTreeMap::new(),
        }
    }

    /// `ws`'s session with `loaded`, those of them still open in it, and `waiting`, those of them
    /// [`still_waiting`]. A workspace with no windows (a config that didn't parse) keeps them all,
    /// as its `stray` queues are kept, so they apply again once the config is good.
    pub fn with_shell<'a>(
        ws: &Workspace,
        loaded: impl IntoIterator<Item = &'a TabId>,
        waiting: impl IntoIterator<Item = (&'a TabId, &'a String)>,
    ) -> Session {
        Session {
            loaded: loaded
                .into_iter()
                .filter(|id| ws.windows.is_empty() || ws.find(id).is_some())
                .cloned()
                .collect(),
            waiting: waiting
                .into_iter()
                .filter(|(id, _)| still_waiting(ws, id))
                .map(|(id, why)| (id.clone(), why.clone()))
                .collect(),
            ..Session::of(ws)
        }
    }
}

/// A version 1 session, from before the workspace lived in the config: the one window's groups,
/// Recent, and its active tab.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct V1Session {
    pub groups: Vec<Group>,
    pub recent: Vec<Group>,
    pub active: Option<TabId>,
}

/// What `load` found at the session path.
///
/// After `Unpersistable` the shell must not call `save`: the file at the path is still there and
/// unread, and saving would overwrite it (a newer build's session, say).
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    Fresh,
    Restored(Session),
    /// A version 1 file, left in place until its groups move into the config. The workspace is
    /// built from the config alone, so after a failed migration it lacks them until a later
    /// launch's migration lands.
    V1(V1Session),
    /// The file was unparseable or from a newer build; it was moved here, not deleted.
    Quarantined {
        moved_to: PathBuf,
    },
    /// The file could not be read or moved aside. Run without persisting and show `reason`.
    Unpersistable {
        reason: String,
    },
}

#[derive(Deserialize)]
struct Header {
    version: u32,
}

#[derive(Deserialize)]
struct V1File {
    workspace: V1Session,
}

pub fn load(path: &Path) -> Loaded {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Loaded::Fresh,
        Err(e) => return quarantine(path, format!("cannot read {}: {e}", path.display())),
    };
    let parsed = match serde_json::from_slice::<Header>(&bytes).map(|h| h.version) {
        Ok(SESSION_VERSION) => serde_json::from_slice::<File>(&bytes)
            .ok()
            .map(|f| Loaded::Restored(f.session)),
        Ok(1) => serde_json::from_slice::<V1File>(&bytes)
            .ok()
            .map(|f| Loaded::V1(f.workspace)),
        _ => None,
    };
    parsed.unwrap_or_else(|| quarantine(path, format!("cannot parse {}", path.display())))
}

fn quarantine(path: &Path, reason: String) -> Loaded {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let base = format!("{}.bad-{secs}", path.display());
    let mut moved_to = PathBuf::from(&base);
    let mut n = 0;
    while moved_to.symlink_metadata().is_ok() {
        n += 1;
        moved_to = PathBuf::from(format!("{base}-{n}"));
    }
    match fs::rename(path, &moved_to) {
        Ok(()) => Loaded::Quarantined { moved_to },
        Err(e) => Loaded::Unpersistable {
            reason: format!("{reason}; could not move it aside: {e}"),
        },
    }
}

/// Writes `ws`'s [`Session`] with the `loaded` tabs and `waiting` documents
/// ([`Session::with_shell`], [`save_session`]).
pub fn save<'a>(
    path: &Path,
    ws: &Workspace,
    loaded: impl IntoIterator<Item = &'a TabId>,
    waiting: impl IntoIterator<Item = (&'a TabId, &'a String)>,
) -> std::io::Result<()> {
    save_session(path, &Session::with_shell(ws, loaded, waiting))
}

/// Writes `session` to `path` ([`write_atomic`]).
pub fn save_session(path: &Path, session: &Session) -> std::io::Result<()> {
    let file = File {
        version: SESSION_VERSION,
        session: session.clone(),
    };
    let json = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
    write_atomic(path, &json)
}

pub const MANIFEST_VERSION: u32 = 1;

/// `windows.json`, the file link routers read to offer flotilla's windows and groups:
/// `{"version":1,"windows":[{"title","colour","groups":[<group name>…]}]}`, windows in config
/// order and each one's groups in sidebar order. `colour_of` is the accent of the config window
/// titled so; `None` is neutral (`null`).
pub fn windows_manifest(ws: &Workspace, colour_of: impl Fn(&str) -> Option<String>) -> String {
    let windows: Vec<serde_json::Value> = ws
        .windows
        .iter()
        .map(|w| {
            let colour = colour_of(&w.title);
            let groups: Vec<&str> = w.groups.iter().map(|g| g.head.title.as_str()).collect();
            serde_json::json!({ "title": w.title, "colour": colour, "groups": groups })
        })
        .collect();
    serde_json::to_string_pretty(
        &serde_json::json!({ "version": MANIFEST_VERSION, "windows": windows }),
    )
    .expect("a JSON value serialises")
}

/// Writes `bytes` to `<path>.tmp`, fsyncs it, then renames over `path`, so a crash leaves the old
/// file or the new one, never half of either.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let written = (|| {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(flatten)]
    session: Session,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConfigWindow, Opened};
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    /// Windows A and B; A shows a docked member, B its head, and one group is in Recent.
    fn sample() -> Workspace {
        let window = |t: &str| ConfigWindow {
            store: "default".into(),
            unsorted: vec![],
            title: t.into(),
            groups: vec![],
        };
        let mut w = Workspace::from_config(&[window("A"), window("B")]);
        let f = url("https://drive.google.com/drive/folders/f1");
        let d = url("https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit");
        let Some(Opened::Added(h)) = w.new_group_in("A", &f, "work") else {
            panic!()
        };
        let Some(Opened::Added(m)) = w.open_from(&h, &d) else {
            panic!()
        };
        w.set_title(&m, "Budget");
        let g2 = url("https://drive.google.com/drive/folders/f2");
        let Some(Opened::Added(h2)) = w.new_group("B", &g2) else {
            panic!()
        };
        w.close_group(&h2, &Default::default());
        let g3 = url("https://drive.google.com/drive/folders/f3");
        w.new_group("B", &g3).unwrap();
        w.activate(&m);
        w
    }

    #[test]
    fn a_workspace_with_no_windows_keeps_the_loaded_tabs() {
        let ids: Vec<TabId> = sample()
            .windows
            .iter()
            .filter_map(|w| w.active.clone())
            .collect();
        assert!(!ids.is_empty());
        let none = Workspace::from_config(&[]);
        assert_eq!(
            Session::with_shell(&none, &ids, []).loaded,
            ids.iter().cloned().collect()
        );
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        save(&p, &none, &ids, []).unwrap();
        let Loaded::Restored(s) = load(&p) else {
            panic!()
        };
        assert_eq!(s.loaded, ids.iter().cloned().collect());
        // With windows, a tab no longer open is still dropped.
        let gone = Session::with_shell(&sample(), &[TabId::default()], []);
        assert!(gone.loaded.is_empty());
    }

    /// An Unsorted document a lookup left waiting keeps its reason across a relaunch; one no
    /// longer in Unsorted does not.
    #[test]
    fn waiting_documents_keep_their_reason() {
        let mut w = sample();
        let Some(crate::model::Handed::Unsorted(d)) = w.receive(
            "A",
            None,
            "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt02/edit",
        ) else {
            panic!()
        };
        let member = w.windows[0].groups[0].members[0].id.clone();
        let why = "Connect a Google account".to_string();
        let reasons = BTreeMap::from([(d.clone(), why.clone()), (member, why.clone())]);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        save(&p, &w, [], &reasons).unwrap();
        let Loaded::Restored(s) = load(&p) else {
            panic!()
        };
        assert_eq!(s.waiting, BTreeMap::from([(d.clone(), why.clone())]));
        // A config that didn't parse keeps every reason.
        let none = Workspace::from_config(&[]);
        assert_eq!(Session::with_shell(&none, [], &reasons).waiting, reasons);
    }

    #[test]
    fn session_v2_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let w = sample();
        save(&p, &w, [], []).unwrap();
        let Loaded::Restored(s) = load(&p) else {
            panic!()
        };
        assert_eq!(s, Session::of(&w));
        assert_eq!(s.active.len(), 2);
        assert_eq!(s.recent.len(), 1);
        // Restored onto the same windows, the run state comes back whole.
        let mut fresh = w.clone();
        for win in &mut fresh.windows {
            win.active = None;
        }
        fresh.recent.clear();
        fresh.restore(s.active, s.recent, s.incoming);
        assert_eq!(fresh, w);
        let json = fs::read_to_string(&p).unwrap();
        assert!(
            json.contains("\"version\": 2") && !json.contains("\"groups\""),
            "{json}"
        );
    }

    #[test]
    fn session_keeps_the_loaded_tabs_still_open() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let w = sample();
        let open: Vec<TabId> = w.tabs().map(|t| t.id.clone()).collect();
        let gone = "0000000000000000".to_string();
        save(&p, &w, [&open[0], &gone], []).unwrap();
        let Loaded::Restored(s) = load(&p) else {
            panic!()
        };
        assert_eq!(s.loaded, BTreeSet::from([open[0].clone()]));
        // A session from before `loaded` reads as nothing loaded.
        let json = fs::read_to_string(&p).unwrap();
        let mut old: serde_json::Value = serde_json::from_str(&json).unwrap();
        old.as_object_mut().unwrap().remove("loaded");
        fs::write(&p, old.to_string()).unwrap();
        let Loaded::Restored(s) = load(&p) else {
            panic!()
        };
        assert!(s.loaded.is_empty());
    }

    #[test]
    fn session_round_trips_incoming() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let mut w = sample();
        let doc = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt02/edit";
        w.queue("B", doc, None).unwrap();
        w.receive("B", None, "mailto:x").unwrap();
        save(&p, &w, [], []).unwrap();
        let Loaded::Restored(s) = load(&p) else {
            panic!()
        };
        assert_eq!(s.incoming.len(), 1, "an empty queue is not written");
        assert_eq!(s.incoming["B"].len(), 2);
        let mut fresh = w.clone();
        for win in &mut fresh.windows {
            win.incoming.clear();
        }
        fresh.restore(s.active, s.recent, s.incoming);
        assert_eq!(fresh, w);
    }

    #[test]
    fn windows_manifest_lists_windows_and_groups_in_order() {
        let mut w = sample();
        let f4 = url("https://drive.google.com/drive/folders/f4");
        let Some(Opened::Added(h4)) = w.new_group("A", &f4) else {
            panic!()
        };
        w.set_title(&w.windows[0].groups[0].head.id.clone(), "Club (interim)");
        w.set_title(&h4, "Bulletins");
        let colour_of = |t: &str| (t == "A").then(|| "#17458f".to_string());
        let m: serde_json::Value = serde_json::from_str(&windows_manifest(&w, colour_of)).unwrap();
        assert_eq!(
            m,
            serde_json::json!({
                "version": 1,
                "windows": [
                    { "title": "A", "colour": "#17458f", "groups": ["Club (interim)", "Bulletins"] },
                    { "title": "B", "colour": null, "groups": ["https://drive.google.com/drive/folders/f3"] },
                ],
            })
        );
    }

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/windows.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "two");
        assert!(!dir.path().join("sub/windows.json.tmp").exists());
    }

    #[test]
    fn a_session_without_incoming_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        fs::write(&p, r#"{"version":2,"active":{},"recent":[]}"#).unwrap();
        let Loaded::Restored(s) = load(&p) else {
            panic!()
        };
        assert!(s.incoming.is_empty());
    }

    #[test]
    fn a_v1_session_loads_as_v1() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let key = r#"{"store":"default","provider":"head","id":"https://drive.google.com/drive/folders/f1"}"#;
        let head = format!(
            r#"{{"id":"t-1","home_url":"https://drive.google.com/drive/folders/f1","title":"Auction","key":{key},"detached":false}}"#
        );
        let group = format!(r#"{{"head":{head},"members":[],"collapsed":true,"store":"default"}}"#);
        fs::write(
            &p,
            format!(
                r#"{{"version":1,"workspace":{{"groups":[{group}],"recent":[],"active":"t-1","next_id":1}}}}"#
            ),
        )
        .unwrap();
        let Loaded::V1(v1) = load(&p) else { panic!() };
        assert_eq!(v1.active.as_deref(), Some("t-1"));
        assert_eq!(v1.groups[0].head.title, "Auction");
        assert!(v1.groups[0].collapsed);
        assert!(
            p.exists(),
            "a v1 session stays where it is for the migration"
        );
    }

    #[test]
    fn missing_file_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("session.json")), Loaded::Fresh);
    }

    #[test]
    fn garbage_is_quarantined_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        fs::write(&p, "not json{").unwrap();
        let Loaded::Quarantined { moved_to } = load(&p) else {
            panic!()
        };
        assert!(!p.exists());
        assert!(moved_to
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("session.json.bad-"));
        assert_eq!(fs::read_to_string(moved_to).unwrap(), "not json{");
    }

    #[test]
    fn newer_version_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        save(&p, &sample(), [], []).unwrap();
        let v3 = fs::read_to_string(&p)
            .unwrap()
            .replacen("\"version\": 2", "\"version\": 3", 1);
        assert!(v3.contains("\"version\": 3"));
        fs::write(&p, v3).unwrap();
        assert!(matches!(load(&p), Loaded::Quarantined { .. }));
        assert!(!p.exists());
    }

    #[test]
    fn save_is_atomic() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let w = sample();
        save(&p, &w, [], []).unwrap();
        assert!(!dir.path().join("session.json.tmp").exists());
        let before = fs::read_to_string(&p).unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let r = save(&p, &Workspace::default(), [], []);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(r.is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), before);
        assert!(!dir.path().join("session.json.tmp").exists());
    }

    fn chmod(p: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn unmovable_garbage_is_unpersistable_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        fs::write(&p, "garbage").unwrap();
        chmod(dir.path(), 0o555);
        let r = load(&p);
        chmod(dir.path(), 0o755);
        assert!(matches!(r, Loaded::Unpersistable { .. }));
        assert_eq!(fs::read_to_string(&p).unwrap(), "garbage");
    }

    #[test]
    fn an_unreadable_parent_is_unpersistable_not_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let p = sub.join("session.json");
        fs::write(&p, "x").unwrap();
        chmod(&sub, 0o000);
        let r = load(&p);
        chmod(&sub, 0o755);
        assert!(matches!(r, Loaded::Unpersistable { .. }));
        assert_eq!(fs::read_to_string(&p).unwrap(), "x");
    }

    #[test]
    fn save_creates_a_missing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b/session.json");
        save(&p, &sample(), [], []).unwrap();
        assert!(matches!(load(&p), Loaded::Restored(_)));
    }

    #[test]
    fn a_second_quarantine_in_the_same_second_keeps_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let mut moved = vec![];
        for body in ["one", "two", "three"] {
            fs::write(&p, body).unwrap();
            let Loaded::Quarantined { moved_to } = load(&p) else {
                panic!()
            };
            moved.push((moved_to, body));
        }
        for (m, body) in moved {
            assert_eq!(fs::read_to_string(m).unwrap(), body);
        }
    }
}
