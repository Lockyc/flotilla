//! Saving the session, and `windows.json` with it: debounced after each change, and once more at
//! exit.

use flotilla_core::model::{TabId, Workspace};
use flotilla_core::session::{self, Loaded};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// How long after the last change the session is written.
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// The session file and the debounce counter. `path` is `None` when nothing may be saved this run
/// (`Loaded::Unpersistable`, or no app data dir): saving would overwrite a file that was never read.
pub struct Persist {
    path: Option<PathBuf>,
    generation: AtomicU64,
    /// Serialises writers: the debounce thread and the exit save share `<path>.tmp`.
    writing: Mutex<()>,
}

impl Persist {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            generation: AtomicU64::new(0),
            writing: Mutex::new(()),
        }
    }

    /// Persists to `path` unless `loaded` says the file there could not be read or moved aside, or
    /// is a version 1 session whose groups are not yet in the config (saving would replace them).
    pub fn for_load(path: Option<PathBuf>, loaded: &Loaded) -> Self {
        let unpersistable = matches!(loaded, Loaded::Unpersistable { .. } | Loaded::V1(_));
        Self::new(path.filter(|_| !unpersistable))
    }

    /// Records a change. The ticket it returns is due only if no later change arrives before the
    /// debounce elapses; `None` when this run does not persist.
    pub fn schedule(&self) -> Option<u64> {
        self.path.as_ref()?;
        Some(self.generation.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// Whether `ticket` is still the latest change.
    pub fn due(&self, ticket: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == ticket
    }

    /// Writes the session of the workspace and loaded tabs `snapshot` returns, then `windows.json`
    /// beside it from that workspace and the config's window accents
    /// ([`session::windows_manifest`]), both taken inside the writer lock. A no-op (and no
    /// snapshot) when this run does not persist. The error is the error bar's text, naming which
    /// file failed.
    pub fn save<C: Fn(&str) -> Option<String>>(
        &self,
        snapshot: impl FnOnce() -> (Workspace, HashSet<TabId>, C),
    ) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        // Snapshot under the writer lock: a writer that waited here then writes the state as of
        // now, never one older than what the writer before it wrote.
        let _w = self.writing.lock().unwrap();
        let (ws, loaded, colour_of) = snapshot();
        session::save(path, &ws, &loaded).map_err(|e| format!("Couldn't save the session: {e}"))?;
        let manifest = session::windows_manifest(&ws, colour_of);
        session::write_atomic(&manifest_path(path), manifest.as_bytes())
            .map_err(|e| format!("Couldn't publish the window list: {e}"))
    }
}

/// `windows.json`, beside the session file.
fn manifest_path(session: &Path) -> PathBuf {
    session.with_file_name("windows.json")
}

/// The error-bar text for what `session::load` found, if the user needs telling.
pub fn load_notice(loaded: &Loaded) -> Option<String> {
    match loaded {
        Loaded::Fresh | Loaded::Restored(_) | Loaded::V1(_) => None,
        Loaded::Quarantined { moved_to } => {
            let name = moved_to.file_name().map_or_else(
                || moved_to.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            Some(format!(
                "Couldn't read the last session — saved it as {name} and started fresh."
            ))
        }
        Loaded::Unpersistable { reason } => Some(unpersistable_notice(reason)),
    }
}

/// The error-bar text for a run that will not save, naming why.
pub fn unpersistable_notice(reason: &str) -> String {
    format!("Couldn't use the last session ({reason}). Changes this session won't be saved.")
}

/// Saves the session `DEBOUNCE` after the last call. Safe from any thread; the save itself runs on
/// a worker thread, which locks `ws` only to clone it.
pub fn mark_dirty(app: &AppHandle) {
    let state = app.state::<crate::state::AppState>();
    let Some(ticket) = state.persist.schedule() else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(DEBOUNCE);
        let state = app.state::<crate::state::AppState>();
        if state.persist.due(ticket) {
            save_now(&app);
        }
    });
}

/// Writes the session immediately (the exit path, and the end of a debounce). A failure is shown
/// in the error bar.
pub fn save_now(app: &AppHandle) {
    let state = app.state::<crate::state::AppState>();
    let snapshot = || {
        let ws = state.ws.lock().unwrap().clone();
        let mut loaded: HashSet<TabId> = state
            .created
            .lock()
            .unwrap()
            .values()
            .flatten()
            .cloned()
            .collect();
        loaded.extend(state.warm.lock().unwrap().iter().cloned());
        let cfg = state.config.lock().unwrap().clone();
        (ws, loaded, move |title: &str| cfg.colour_of(title))
    };
    if let Err(msg) = state.persist.save(snapshot) {
        crate::tabs::notice(app, msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::testing::{new_group, one_window};
    use flotilla_core::model::Opened;
    use flotilla_core::session::{Session, V1Session};

    fn sample() -> Workspace {
        let mut ws = one_window();
        let Opened::Added(_) = new_group(&mut ws, "https://drive.google.com/drive/folders/f1")
        else {
            panic!()
        };
        ws
    }

    fn neutral(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn only_the_latest_change_is_due() {
        let p = Persist::new(Some(PathBuf::from("/nonexistent/session.json")));
        let first = p.schedule().unwrap();
        let second = p.schedule().unwrap();
        assert!(!p.due(first));
        assert!(p.due(second));
    }

    #[test]
    fn save_round_trips_through_session_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let p = Persist::new(Some(path.clone()));
        let ws = sample();
        let loaded: HashSet<TabId> = ws.tabs().map(|t| t.id.clone()).collect();
        p.save(|| (ws.clone(), loaded.clone(), neutral)).unwrap();
        assert_eq!(
            session::load(&path),
            Loaded::Restored(Session::with_loaded(&ws, &loaded))
        );
    }

    #[test]
    fn save_writes_the_windows_manifest_beside_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let p = Persist::new(Some(dir.path().join("session.json")));
        let ws = sample();
        let blue = |_: &str| Some("#17458f".to_string());
        p.save(|| (ws.clone(), HashSet::new(), blue)).unwrap();
        let written = std::fs::read_to_string(dir.path().join("windows.json")).unwrap();
        assert_eq!(written, session::windows_manifest(&ws, blue));
    }

    #[test]
    fn each_file_names_its_own_failure() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where windows.json goes: the session saves, the manifest can't.
        std::fs::create_dir(dir.path().join("windows.json")).unwrap();
        let p = Persist::new(Some(dir.path().join("session.json")));
        let e = p.save(|| (sample(), HashSet::new(), neutral)).unwrap_err();
        assert!(e.starts_with("Couldn't publish the window list: "), "{e}");
        let p = Persist::new(Some(dir.path().join("windows.json/x/session.json")));
        std::fs::write(dir.path().join("windows.json/x"), "").unwrap();
        let e = p.save(|| (sample(), HashSet::new(), neutral)).unwrap_err();
        assert!(e.starts_with("Couldn't save the session: "), "{e}");
    }

    /// The snapshot is taken inside the writer lock, so a write that waited on another can never
    /// land an older snapshot after a newer one.
    #[test]
    fn the_snapshot_is_taken_while_holding_the_writer_lock() {
        let dir = tempfile::tempdir().unwrap();
        let p = Persist::new(Some(dir.path().join("session.json")));
        p.save(|| {
            assert!(p.writing.try_lock().is_err());
            (sample(), HashSet::new(), neutral)
        })
        .unwrap();
    }

    #[test]
    fn an_unpersistable_run_never_writes() {
        let p = Persist::new(None);
        assert_eq!(p.schedule(), None);
        p.save::<fn(&str) -> Option<String>>(|| {
            panic!("an unpersistable run must not even snapshot")
        })
        .unwrap();
    }

    #[test]
    fn only_an_unpersistable_load_disables_saving() {
        let p = || Some(PathBuf::from("/x/session.json"));
        assert!(Persist::for_load(p(), &Loaded::Fresh).schedule().is_some());
        let q = Loaded::Quarantined {
            moved_to: PathBuf::from("/x/session.json.bad-1"),
        };
        assert!(Persist::for_load(p(), &q).schedule().is_some());
        let u = Loaded::Unpersistable { reason: "r".into() };
        assert_eq!(Persist::for_load(p(), &u).schedule(), None);
        let v1 = Loaded::V1(V1Session {
            groups: vec![],
            recent: vec![],
            active: None,
        });
        assert_eq!(Persist::for_load(p(), &v1).schedule(), None);
    }

    #[test]
    fn notices_name_the_quarantine_file_and_the_reason() {
        assert_eq!(load_notice(&Loaded::Fresh), None);
        assert_eq!(load_notice(&Loaded::Restored(Session::default())), None);
        let q = Loaded::Quarantined {
            moved_to: PathBuf::from("/x/au.lsjc.flotilla/session.json.bad-1700000000"),
        };
        assert_eq!(
            load_notice(&q).unwrap(),
            "Couldn't read the last session — saved it as session.json.bad-1700000000 and started fresh."
        );
        let u = Loaded::Unpersistable {
            reason: "cannot read session.json: permission denied".into(),
        };
        let text = load_notice(&u).unwrap();
        assert!(text.contains("cannot read session.json: permission denied"));
        assert!(text.contains("won't be saved"));
    }
}
