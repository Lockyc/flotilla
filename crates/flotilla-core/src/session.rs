//! Persisting the run state: one JSON file the shell points us at, holding each window's active
//! tab and Recent. The windows and groups themselves live in the config. Keys and ids in it are
//! derived data, re-derived when applied (`Workspace::restore`), never trusted from disk.

use crate::model::{Group, TabId, Workspace};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SESSION_VERSION: u32 = 2;

/// What a session file keeps: each window's active tab by window title, and Recent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub active: BTreeMap<String, TabId>,
    pub recent: Vec<Group>,
}

impl Session {
    pub fn of(ws: &Workspace) -> Session {
        Session {
            active: ws
                .windows
                .iter()
                .filter_map(|w| Some((w.title.clone(), w.active.clone()?)))
                .collect(),
            recent: ws.recent.clone(),
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
    /// A version 1 file, left in place: its groups are the workspace until they move into the
    /// config.
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

/// Writes `ws`'s [`Session`] to `<path>.tmp`, fsyncs it, then renames over `path`, so a crash
/// leaves the old file or the new one, never half of either.
pub fn save(path: &Path, ws: &Workspace) -> std::io::Result<()> {
    let file = File {
        version: SESSION_VERSION,
        session: Session::of(ws),
    };
    let json = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let written = (|| {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&json)?;
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
            title: t.into(),
            groups: vec![],
        };
        let mut w = Workspace::from_config(&[window("A"), window("B")]);
        let f = url("https://drive.google.com/drive/folders/f1");
        let d = url("https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit");
        let Some(Opened::Added(h)) = w.new_group("A", &f, "work") else {
            panic!()
        };
        let Some(Opened::Added(m)) = w.open_from(&h, &d) else {
            panic!()
        };
        w.set_title(&m, "Budget");
        let g2 = url("https://drive.google.com/drive/folders/f2");
        let Some(Opened::Added(h2)) = w.new_group("B", &g2, "default") else {
            panic!()
        };
        w.close_group(&h2);
        let g3 = url("https://drive.google.com/drive/folders/f3");
        w.new_group("B", &g3, "default").unwrap();
        w.activate(&m);
        w
    }

    #[test]
    fn session_v2_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let w = sample();
        save(&p, &w).unwrap();
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
        fresh.restore(s.active, s.recent);
        assert_eq!(fresh, w);
        let json = fs::read_to_string(&p).unwrap();
        assert!(
            json.contains("\"version\": 2") && !json.contains("\"groups\""),
            "{json}"
        );
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
        save(&p, &sample()).unwrap();
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
        save(&p, &w).unwrap();
        assert!(!dir.path().join("session.json.tmp").exists());
        let before = fs::read_to_string(&p).unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let r = save(&p, &Workspace::default());
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
        save(&p, &sample()).unwrap();
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
