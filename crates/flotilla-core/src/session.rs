//! Persisting the workspace: one JSON file the shell points us at. Keys are derived data and are
//! re-derived on load (`Workspace::rekey_and_repair`), never trusted from disk.

use crate::model::Workspace;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SESSION_VERSION: u32 = 1;

/// What `load` found at the session path.
///
/// After `Unpersistable` the shell must not call `save`: the file at the path is still there and
/// unread, and saving would overwrite it (a newer build's session, say).
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    Fresh,
    Restored(Workspace),
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

pub fn load(path: &Path) -> Loaded {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Loaded::Fresh,
        Err(e) => return quarantine(path, format!("cannot read {}: {e}", path.display())),
    };
    let current =
        serde_json::from_slice::<Header>(&bytes).is_ok_and(|h| h.version <= SESSION_VERSION);
    if current {
        if let Ok(f) = serde_json::from_slice::<File>(&bytes) {
            let mut ws = f.workspace;
            ws.rekey_and_repair();
            return Loaded::Restored(ws);
        }
    }
    quarantine(path, format!("cannot parse {}", path.display()))
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

/// Writes `<path>.tmp`, fsyncs it, then renames over `path`, so a crash leaves the old file or the
/// new one, never half of either.
pub fn save(path: &Path, ws: &Workspace) -> std::io::Result<()> {
    let file = File {
        version: SESSION_VERSION,
        workspace: ws.clone(),
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
    workspace: Workspace,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Opened;
    use url::Url;

    fn sample() -> Workspace {
        let mut w = Workspace::default();
        let f = Url::parse("https://drive.google.com/drive/folders/f1").unwrap();
        let d =
            Url::parse("https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit").unwrap();
        let Opened::Added(h) = w.new_group(&f, "work") else {
            panic!()
        };
        let Some(Opened::Added(m)) = w.open_from(&h, &d) else {
            panic!()
        };
        w.set_title(&m, "Budget");
        w.set_detached(&m, true);
        w.set_collapsed(&h, true);
        let g2 = Url::parse("https://drive.google.com/drive/folders/f2").unwrap();
        let Opened::Added(h2) = w.new_group(&g2, "default") else {
            panic!()
        };
        w.close_group(&h2);
        w.activate(&m);
        w.set_collapsed(&h, true);
        w
    }

    #[test]
    fn round_trip_preserves_groups_recent_active_collapsed_detached() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let w = sample();
        save(&p, &w).unwrap();
        assert_eq!(load(&p), Loaded::Restored(w));
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
        let v2 = fs::read_to_string(&p)
            .unwrap()
            .replacen("\"version\": 1", "\"version\": 2", 1);
        assert!(v2.contains("\"version\": 2"));
        fs::write(&p, v2).unwrap();
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

    #[test]
    fn load_rekeys_and_repairs_a_tampered_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let w = sample();
        save(&p, &w).unwrap();
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        v["workspace"]["groups"][0]["head"]["key"]["id"] = "bogus".into();
        v["workspace"]["next_id"] = 0.into();
        fs::write(&p, v.to_string()).unwrap();
        assert_eq!(load(&p), Loaded::Restored(w));
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
