//! Moving a version 1 session into the config, once. A v1 session file held the one window's
//! groups; now groups live in the config and the session holds only run state. [`migrate`] writes
//! the v1 groups into the config's window named by the old top-level `title`, moves the old
//! top-level `title`/`colour` onto that window, and replaces the session file with a v2 one,
//! which loads as restored from then on.

use crate::config::{self, STARTER_WINDOW};
use crate::write::{self, Doc, Write};
use config_core::toml_edit::DocumentMut;
use flotilla_core::identity::{identity, tab_id, DocKey};
use flotilla_core::model::Workspace;
use flotilla_core::session::{self, Session, V1Session};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use url::Url;

/// Migrates `v1` into the config at `config_path` (seeded with the starter first when missing)
/// and writes its run state as a v2 session at `session_path`. A group or document the config
/// already holds is not added again, so a migration cut short and run again adds nothing twice.
pub fn migrate(config_path: &Path, session_path: &Path, v1: &V1Session) -> Result<Session, String> {
    config::create_starter(config_path)?;
    let (title, colour) = take_root_window_keys(config_path)?;
    let title = title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| STARTER_WINDOW.to_string());
    let cfg = config::load(config_path)?;
    let edit = |r: Result<(), config_core::EditError>| r.map_err(|e| format!("config: {e}"));
    match cfg.windows.iter().find(|w| w.title.trim() == title) {
        None => edit(config_core::add_window(
            config_path,
            &title,
            colour.as_deref(),
        ))?,
        Some(w) if w.colour.is_none() && colour.is_some() => edit(config_core::edit::edit_window(
            config_path,
            &title,
            None,
            Some(colour.as_deref()),
        ))?,
        Some(_) => {}
    }

    let cfg = config::load(config_path)?;
    let ws = Workspace::from_config(&cfg.workspace_windows());
    let mut open: HashSet<DocKey> = ws.tabs().map(|t| t.key.clone()).collect();
    let mut names: Vec<String> = crate::tabs::window(&ws, &title)
        .map(|w| w.groups.iter().map(|g| g.head.title.clone()).collect())
        .unwrap_or_default();
    let key = |store: &str, url: &str| Url::parse(url).ok().map(|u| identity(store, &u));
    for g in &v1.groups {
        let Some(head) = key(&g.store, &g.head.home_url) else {
            continue;
        };
        if !open.insert(head) {
            continue;
        }
        let docs = g
            .members
            .iter()
            .filter(|m| key(&g.store, &m.home_url).is_some_and(|k| open.insert(k)))
            .map(|m| Doc {
                title: m.title.clone(),
                url: m.home_url.clone(),
                detached: m.detached,
            })
            .collect();
        let taken: Vec<&str> = names.iter().map(String::as_str).collect();
        let name = write::unique_group_name(&taken, &g.head.title);
        write::apply(
            config_path,
            &Write::NewGroup {
                window: title.clone(),
                name: name.clone(),
                url: g.head.home_url.clone(),
                store: g.store.clone(),
                collapsed: g.collapsed,
                detached: g.head.detached,
                docs,
            },
        )
        .map_err(|e| format!("config: {e}"))?;
        names.push(name);
    }

    // The old active tab, by the id its document has now.
    let active_id = v1.active.as_ref().and_then(|a| {
        v1.groups.iter().find_map(|g| {
            std::iter::once(&g.head)
                .chain(&g.members)
                .find(|t| &t.id == a)
                .and_then(|t| key(&g.store, &t.home_url))
        })
    });
    let s = Session {
        active: active_id
            .map(|k| BTreeMap::from([(title.clone(), tab_id(&k))]))
            .unwrap_or_default(),
        recent: v1.recent.clone(),
    };
    session::save_session(session_path, &s).map_err(|e| format!("session: {e}"))?;
    Ok(s)
}

/// Takes the v1 top-level `title` and `colour` out of the config at `path` and returns them; a
/// colour that does not parse is dropped. config-core has no edit that removes a root key, so this
/// edits the document directly, keeping every comment, and writes it the same way config-core
/// does (a temporary file renamed over the original).
fn take_root_window_keys(path: &Path) -> Result<(Option<String>, Option<String>), String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("config: {e}"))?;
    let mut doc: DocumentMut = src.parse().map_err(|e| format!("config: {e}"))?;
    // The comments above a removed key stay, above whatever now comes first after it.
    let mut carried = String::new();
    let mut take = |key: &str| {
        if let Some(p) = doc
            .as_table()
            .key(key)
            .and_then(|k| k.leaf_decor().prefix())
            .and_then(|p| p.as_str())
        {
            carried.push_str(p);
        }
        doc.remove(key)
            .and_then(|item| item.as_str().map(str::to_string))
    };
    let title = take("title");
    let colour = take("colour")
        .filter(|c| !c.trim().is_empty())
        .filter(|c| config_core::Colour::parse(c).is_ok());
    if !carried.is_empty() {
        carry_prefix(&mut doc, &carried);
    }
    let out = doc.to_string();
    if out != src {
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, &out)
            .and_then(|_| std::fs::rename(&tmp, path))
            .map_err(|e| format!("config: {e}"))?;
    }
    Ok((title, colour))
}

/// Puts `prefix` (comment lines) in front of the first entry `doc` now starts with: a root key, or
/// the first table.
fn carry_prefix(doc: &mut DocumentMut, prefix: &str) {
    use config_core::toml_edit::Item;
    let Some((mut key, item)) = doc.as_table_mut().iter_mut().next() else {
        return;
    };
    let joined = |own: Option<&str>| format!("{prefix}{}", own.unwrap_or(""));
    match item {
        Item::Value(_) => {
            let own = key
                .leaf_decor()
                .prefix()
                .and_then(|p| p.as_str())
                .map(str::to_string);
            key.leaf_decor_mut().set_prefix(joined(own.as_deref()));
        }
        Item::Table(t) => {
            let own = t
                .decor()
                .prefix()
                .and_then(|p| p.as_str())
                .map(str::to_string);
            t.decor_mut().set_prefix(joined(own.as_deref()));
        }
        Item::ArrayOfTables(a) => {
            if let Some(t) = a.get_mut(0) {
                let own = t
                    .decor()
                    .prefix()
                    .and_then(|p| p.as_str())
                    .map(str::to_string);
                t.decor_mut().set_prefix(joined(own.as_deref()));
            }
        }
        Item::None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_core::model::{Group, Tab};
    use flotilla_core::session::Loaded;
    use std::path::PathBuf;

    const ROTARY: &str = "https://drive.google.com/drive/folders/0AExampleFolderRoot";
    const BUDGET: &str =
        "https://docs.google.com/spreadsheets/d/1ExampleSheetId0000000000000000000000000000/edit";
    const BULLETINS: &str = "https://drive.google.com/drive/folders/1BulletinsFolderIdAbCdEfGh";
    const BULLETIN: &str =
        "https://docs.google.com/presentation/d/1ExampleSlidesId000000000000000000000000000/edit";
    const OLD: &str = "https://drive.google.com/drive/folders/1OldClosedFolderAbCdEfGhIj";

    fn tab(id: &str, url: &str, title: &str, detached: bool) -> Tab {
        Tab {
            id: id.into(),
            home_url: url.into(),
            title: title.into(),
            key: identity("default", &Url::parse(url).unwrap()),
            detached,
        }
    }

    fn group(head: Tab, members: Vec<Tab>, collapsed: bool) -> Group {
        Group {
            head,
            members,
            collapsed,
            store: "default".into(),
        }
    }

    /// The user's session today: the Rotary drive root with its Budget sheet, the Bulletins
    /// folder (collapsed) with the 2026-11-02 Bulletin popped out, the Budget active, and one
    /// group in Recent.
    fn v1() -> V1Session {
        V1Session {
            groups: vec![
                group(
                    tab("t-1", ROTARY, "Rotary (interim)", false),
                    vec![tab("t-2", BUDGET, "Rotary Example – Budget", false)],
                    false,
                ),
                group(
                    tab("t-3", BULLETINS, "Bulletins", false),
                    vec![tab("t-4", BULLETIN, "2026-11-02 Bulletin", true)],
                    true,
                ),
            ],
            recent: vec![group(tab("t-5", OLD, "Old", false), vec![], false)],
            active: Some("t-2".into()),
        }
    }

    fn paths() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("flotilla").join("config.toml");
        let session = dir.path().join("session.json");
        (dir, cfg, session)
    }

    fn config(path: &Path) -> crate::config::Config {
        crate::config::load(path).unwrap()
    }

    fn id(url: &str) -> String {
        tab_id(&identity("default", &Url::parse(url).unwrap()))
    }

    #[test]
    fn v1_session_migrates_into_one_window() {
        let (_d, cfg_path, session_path) = paths();
        let s = migrate(&cfg_path, &session_path, &v1()).unwrap();
        // A missing config was seeded: the groups went into its `flotilla` window.
        let cfg = config(&cfg_path);
        assert_eq!(cfg.windows.len(), 1);
        let w = &cfg.windows[0];
        assert_eq!(w.title, "flotilla");
        let names: Vec<&str> = w.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["Rotary (interim)", "Bulletins"]);
        assert_eq!(w.groups[0].extra.url, ROTARY);
        assert_eq!(w.groups[0].tabs[0].title, "Rotary Example – Budget");
        assert!(w.groups[1].extra.collapsed);
        assert!(w.groups[1].tabs[0].detached);
        // The run state: the old active tab, by its new id, and Recent.
        assert_eq!(s.active.get("flotilla"), Some(&id(BUDGET)));
        assert_eq!(s.recent.len(), 1);
        let Loaded::Restored(saved) = session::load(&session_path) else {
            panic!("the session is v2 now")
        };
        assert_eq!(saved, s);
        // It restores onto the workspace the config builds.
        let mut ws = Workspace::from_config(&cfg.workspace_windows());
        ws.restore(s.active, s.recent);
        assert_eq!(ws.windows[0].active, Some(id(BUDGET)));
        assert_eq!(ws.recent[0].head.id, id(OLD));
    }

    #[test]
    fn migration_moves_title_and_colour_into_the_window() {
        let (_d, cfg_path, session_path) = paths();
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(
            &cfg_path,
            "# flotilla config\ntitle = \"Rotary\"\ncolour = \"#17458F\"\ndark_mode = true\n",
        )
        .unwrap();
        migrate(&cfg_path, &session_path, &v1()).unwrap();
        let src = std::fs::read_to_string(&cfg_path).unwrap();
        let table: toml::Table = src.parse().unwrap();
        assert!(
            !table.contains_key("title") && !table.contains_key("colour"),
            "{src}"
        );
        assert!(src.starts_with("# flotilla config"), "{src}");
        let cfg = config(&cfg_path);
        assert!(cfg.dark_mode);
        assert_eq!(cfg.windows.len(), 1);
        assert_eq!(cfg.windows[0].title, "Rotary");
        assert_eq!(cfg.windows[0].colour.as_deref(), Some("#17458f"));
        assert_eq!(cfg.windows[0].groups.len(), 2);
    }

    #[test]
    fn an_existing_window_with_the_old_title_takes_the_groups_and_the_colour() {
        let (_d, cfg_path, session_path) = paths();
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(
            &cfg_path,
            "title = \"Rotary\"\ncolour = \"#17458F\"\n[[window]]\ntitle = \"flotilla\"\n\
             [[window]]\ntitle = \"Rotary\"\n",
        )
        .unwrap();
        migrate(&cfg_path, &session_path, &v1()).unwrap();
        let cfg = config(&cfg_path);
        assert_eq!(cfg.windows.len(), 2);
        assert!(cfg.windows[0].groups.is_empty());
        assert_eq!(cfg.windows[1].groups.len(), 2);
        assert_eq!(cfg.windows[1].colour.as_deref(), Some("#17458f"));
    }

    #[test]
    fn migration_runs_once() {
        let (_d, cfg_path, session_path) = paths();
        migrate(&cfg_path, &session_path, &v1()).unwrap();
        // The session file is v2 now: nothing left to migrate.
        assert!(matches!(session::load(&session_path), Loaded::Restored(_)));
        // Run again (a crash before the session was written): nothing is added twice.
        migrate(&cfg_path, &session_path, &v1()).unwrap();
        let cfg = config(&cfg_path);
        assert_eq!(cfg.windows[0].groups.len(), 2);
        assert_eq!(cfg.windows[0].groups[0].tabs.len(), 1);
    }
}
