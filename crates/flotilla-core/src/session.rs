//! Persisting the workspace: one JSON file the shell points us at. Keys are derived data and are
//! re-derived on load (`Workspace::rekey_and_repair`), never trusted from disk.

use crate::model::Workspace;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SESSION_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    Fresh,
    Restored(Workspace),
    /// The file was unreadable or from a newer build; it was moved here, not deleted.
    Quarantined {
        moved_to: PathBuf,
    },
}

pub fn load(path: &Path) -> Loaded {
    let Ok(text) = fs::read_to_string(path) else {
        return if path.exists() {
            quarantine(path)
        } else {
            Loaded::Fresh
        };
    };
    match serde_json::from_str::<File>(&text) {
        Ok(f) if f.version <= SESSION_VERSION => {
            let mut ws = f.workspace;
            ws.rekey_and_repair();
            Loaded::Restored(ws)
        }
        _ => quarantine(path),
    }
}

fn quarantine(path: &Path) -> Loaded {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".bad-{secs}"));
    let moved_to = PathBuf::from(name);
    match fs::rename(path, &moved_to) {
        Ok(()) => Loaded::Quarantined { moved_to },
        Err(_) => Loaded::Fresh,
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
        fs::write(&p, r#"{"version":2,"workspace":{}}"#).unwrap();
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
}
