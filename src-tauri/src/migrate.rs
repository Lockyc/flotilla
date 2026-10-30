//! A version 1 session file holds its one window's groups; [`migrate`] moves them into the config,
//! where groups live, and leaves a v2 session (each window's active tab and Recent) in its place.
//! The window that takes them is the one the v1 config's top-level `title` names; that `title` and
//! the top-level `colour` move onto it.

use crate::config::{self, STARTER_WINDOW};
use crate::write::{self, Doc, Write};
use config_core::toml_edit::DocumentMut;
use flotilla_core::identity::{identity, page_title, tab_id, DocKey};
use flotilla_core::model::{Group, Workspace};
use flotilla_core::session::{self, Session, V1Session};
use std::collections::BTreeMap;
use std::path::Path;
use url::Url;

/// Migrates `v1` into the config at `config_path` (seeded with the starter first when missing)
/// and writes its run state as a v2 session at `session_path`.
///
/// Every step reads the files as they are, so a migration cut short at any point and run again
/// ends where an uninterrupted one would: what the config already holds is not added again, the
/// top-level `title` and `colour` are removed only by the last config write (until then they still
/// name the window), and after it the window holding the v1 groups is found by them.
pub fn migrate(config_path: &Path, session_path: &Path, v1: &V1Session) -> Result<Session, String> {
    migrate_until(config_path, session_path, v1, usize::MAX)
}

/// [`migrate`], stopping before step `stop` (the tests' interruption).
fn migrate_until(
    config_path: &Path,
    session_path: &Path,
    v1: &V1Session,
    stop: usize,
) -> Result<Session, String> {
    let at = |n: usize| {
        if n >= stop {
            Err(format!("stopped before step {n}"))
        } else {
            Ok(())
        }
    };
    let edit = |r: Result<(), config_core::EditError>| r.map_err(|e| format!("config: {e}"));

    at(0)?;
    config::create_starter(config_path)?;
    let (old_title, colour) = root_window_keys(config_path)?;
    let ws = load(config_path)?;
    let title = old_title
        .or_else(|| holding_window(&ws, v1))
        .unwrap_or_else(|| STARTER_WINDOW.to_string());

    at(1)?;
    let cfg = load_config(config_path)?;
    match cfg.windows.iter().find(|w| w.title.trim() == title) {
        None => edit(config_core::add_window(
            config_path,
            &title,
            colour.as_deref(),
        ))?,
        Some(w) if w.colour.is_none() && colour.is_some() => edit(config_core::edit_window(
            config_path,
            &title,
            None,
            Some(colour.as_deref()),
        ))?,
        Some(_) => {}
    }

    for (i, g) in v1.groups.iter().enumerate() {
        at(2 + i)?;
        add_group(config_path, &title, g)?;
    }

    at(2 + v1.groups.len())?;
    for key in ["title", "colour"] {
        edit(config_core::remove_key(config_path, key).map(|_| ()))?;
    }

    at(3 + v1.groups.len())?;
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
        recent: v1
            .recent
            .iter()
            .cloned()
            .map(|mut g| {
                for t in std::iter::once(&mut g.head).chain(&mut g.members) {
                    t.title = page_title(&t.title).to_string();
                }
                g
            })
            .collect(),
    };
    session::save_session(session_path, &s).map_err(|e| format!("session: {e}"))?;
    Ok(s)
}

fn key(store: &str, url: &str) -> Option<DocKey> {
    Url::parse(url).ok().map(|u| identity(store, &u))
}

/// Adds v1 group `g` to window `window`, with the documents of it the config does not hold. A
/// group whose head the config already has (an earlier run, or one the user added) takes the
/// documents into the group that holds that head instead.
fn add_group(config_path: &Path, window: &str, g: &Group) -> Result<(), String> {
    let ws = load(config_path)?;
    let open = |k: &DocKey| ws.tabs().any(|t| &t.key == k);
    let docs: Vec<Doc> = g
        .members
        .iter()
        .filter(|m| key(&g.store, &m.home_url).is_some_and(|k| !open(&k)))
        .map(|m| Doc {
            title: page_title(&m.title).to_string(),
            url: m.home_url.clone(),
            detached: m.detached,
        })
        .collect();
    let Some(head) = key(&g.store, &g.head.home_url) else {
        return Ok(());
    };
    let apply = |w: &Write| write::apply(config_path, w).map_err(|e| format!("config: {e}"));
    let holder = ws
        .find(&tab_id(&head))
        .map(|(wi, grp, _)| (ws.windows[wi].title.clone(), grp.head.title.clone()));
    if let Some((window, group)) = holder {
        for doc in docs {
            apply(&Write::AddDoc {
                window: window.clone(),
                group: group.clone(),
                doc,
            })?;
        }
        return Ok(());
    }
    let names: Vec<&str> = crate::tabs::window(&ws, window)
        .map(|w| w.groups.iter().map(|g| g.head.title.as_str()).collect())
        .unwrap_or_default();
    apply(&Write::NewGroup {
        window: window.to_string(),
        name: write::unique_group_name(&names, page_title(&g.head.title)),
        url: g.head.home_url.clone(),
        store: g.store.clone(),
        collapsed: g.collapsed,
        detached: g.head.detached,
        docs,
    })
}

/// The title of the window that already holds a v1 group's head: where an earlier run of the
/// migration, cut short after the top-level `title` went, put them.
fn holding_window(ws: &Workspace, v1: &V1Session) -> Option<String> {
    v1.groups.iter().find_map(|g| {
        let k = key(&g.store, &g.head.home_url)?;
        ws.find(&tab_id(&k))
            .map(|(wi, _, _)| ws.windows[wi].title.clone())
    })
}

/// The v1 config's top-level `title` (blank is none) and `colour` (one that does not parse is
/// none), read without changing the file.
fn root_window_keys(path: &Path) -> Result<(Option<String>, Option<String>), String> {
    let doc = document(path)?;
    let text = |k: &str| {
        doc.get(k)
            .and_then(|i| i.as_str())
            .map(|s| s.trim().to_string())
    };
    let title = text("title").filter(|t| !t.is_empty());
    let colour = text("colour").filter(|c| config_core::Colour::parse(c).is_ok());
    Ok((title, colour))
}

fn document(path: &Path) -> Result<DocumentMut, String> {
    std::fs::read_to_string(path)
        .map_err(|e| format!("config: {e}"))?
        .parse()
        .map_err(|e| format!("config: {e}"))
}

/// The config at `path` as it will be once its top-level `title` and `colour` are gone: those keys
/// are not the config's any more, so they are left out of the parse rather than refused.
fn load_config(path: &Path) -> Result<config::Config, String> {
    let mut doc = document(path)?;
    doc.remove("title");
    doc.remove("colour");
    config::parse(&doc.to_string())
}

fn load(path: &Path) -> Result<Workspace, String> {
    Ok(Workspace::from_config(
        &load_config(path)?.workspace_windows(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_core::model::Tab;
    use flotilla_core::session::Loaded;
    use std::path::PathBuf;

    const CLUB: &str = "https://drive.google.com/drive/folders/0AbCdEfGhIjKlMnOpQ";
    const BUDGET: &str = "https://docs.google.com/spreadsheets/d/1AbCdEfGhIjKlMnOpQrSt04/edit";
    const NEWSLETTERS: &str = "https://drive.google.com/drive/folders/1NewslettersFolderIdAbCdE";
    const NEWSLETTER: &str = "https://docs.google.com/presentation/d/1AbCdEfGhIjKlMnOpQrSt05/edit";
    const FLYER: &str = "https://docs.google.com/document/d/1FairFlyerAbCdEfGhIjKlMn/edit";
    const OLD: &str = "https://drive.google.com/drive/folders/1OldClosedFolderAbCdEfGhIj";
    const OLD_DECK: &str = "https://docs.google.com/presentation/d/1AbCdEfGhIjKlMnOpQrSt06/edit";

    /// A config before the migration:the starter's header, a top-level name and colour.
    const OLD_CONFIG: &str = "# flotilla config — app-wide settings. Edit and save; flotilla \
        hot-reloads.\n\n# The window's name.\ntitle = \"Club\"\n\n# The window's accent colour.\n\
        colour = \"#2a6f4e\"\n\n# Chrome sizing.\ndensity = \"comfortable\"\n";

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

    /// A v1 session: the Club drive root with its Budget sheet, the Newsletters
    /// folder (collapsed) with the 2026-11-02 Newsletter popped out, and a group headed by the
    /// Fair flyer; titles as the pages reported them; the Budget active; one group in
    /// Recent.
    fn v1() -> V1Session {
        V1Session {
            groups: vec![
                group(
                    tab("t-1", CLUB, "Club drive - Google Drive", false),
                    vec![tab("t-2", BUDGET, "Club – Budget - Google Sheets", false)],
                    false,
                ),
                group(
                    tab("t-3", NEWSLETTERS, "Newsletters - Google Drive", false),
                    vec![tab(
                        "t-4",
                        NEWSLETTER,
                        "2026-11-02 Newsletter - Google Slides",
                        true,
                    )],
                    true,
                ),
                group(
                    tab("t-6", FLYER, "Fair flyer - Google Docs", false),
                    vec![],
                    false,
                ),
            ],
            recent: vec![group(
                tab("t-5", OLD, "Old - Google Drive", false),
                vec![tab("t-7", OLD_DECK, "Old deck - Google Slides", false)],
                false,
            )],
            active: Some("t-2".into()),
        }
    }

    fn paths() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("flotilla").join("config.toml");
        let session = dir.path().join("session.json");
        (dir, cfg, session)
    }

    fn write_old_config(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, OLD_CONFIG).unwrap();
    }

    fn config(path: &Path) -> config::Config {
        config::load(path).unwrap()
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
        assert_eq!(names, ["Club drive", "Newsletters", "Fair flyer"]);
        assert_eq!(w.groups[0].extra.url, CLUB);
        assert_eq!(w.groups[0].tabs[0].title, "Club – Budget");
        assert!(w.groups[1].extra.collapsed);
        assert!(w.groups[1].tabs[0].detached);
        assert_eq!(w.groups[1].tabs[0].title, "2026-11-02 Newsletter");
        // The run state: the old active tab, by its new id, and Recent.
        assert_eq!(s.active.get("flotilla"), Some(&id(BUDGET)));
        assert_eq!(s.recent.len(), 1);
        let titles = |g: &Group| {
            std::iter::once(&g.head)
                .chain(&g.members)
                .map(|t| t.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            titles(&s.recent[0]),
            ["Old", "Old deck"],
            "Recent drops page suffixes too"
        );
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
        write_old_config(&cfg_path);
        migrate(&cfg_path, &session_path, &v1()).unwrap();
        let src = std::fs::read_to_string(&cfg_path).unwrap();
        let table: toml::Table = src.parse().unwrap();
        assert!(
            !table.contains_key("title") && !table.contains_key("colour"),
            "{src}"
        );
        // The file's header stays; the removed keys' own comments go with them.
        assert!(src.starts_with("# flotilla config"), "{src}");
        assert!(
            !src.contains("The window's name") && !src.contains("accent colour"),
            "{src}"
        );
        let cfg = config(&cfg_path);
        assert_eq!(cfg.windows.len(), 1);
        assert_eq!(cfg.windows[0].title, "Club");
        assert_eq!(cfg.windows[0].colour.as_deref(), Some("#2a6f4e"));
        assert_eq!(cfg.windows[0].groups.len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn migration_keeps_the_configs_mode_and_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let (d, real, session_path) = paths();
        write_old_config(&real);
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        migrate(&link, &session_path, &v1()).unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let mode = std::fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(config(&real).windows[0].groups.len(), 3);
    }

    #[test]
    fn a_migration_cut_short_anywhere_ends_where_a_whole_one_does() {
        let whole = {
            let (_d, cfg_path, session_path) = paths();
            write_old_config(&cfg_path);
            let s = migrate(&cfg_path, &session_path, &v1()).unwrap();
            (config(&cfg_path), s)
        };
        for stop in 0..=3 + v1().groups.len() {
            let (_d, cfg_path, session_path) = paths();
            write_old_config(&cfg_path);
            assert!(migrate_until(&cfg_path, &session_path, &v1(), stop).is_err());
            let s = migrate(&cfg_path, &session_path, &v1()).unwrap();
            assert_eq!(
                (config(&cfg_path), s),
                whole,
                "cut short before step {stop}"
            );
        }
    }

    #[test]
    fn an_existing_window_with_the_old_title_takes_the_groups_and_the_colour() {
        let (_d, cfg_path, session_path) = paths();
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(
            &cfg_path,
            "title = \"Club\"\ncolour = \"#2A6F4E\"\n[[window]]\ntitle = \"flotilla\"\n\
             [[window]]\ntitle = \"Club\"\n",
        )
        .unwrap();
        migrate(&cfg_path, &session_path, &v1()).unwrap();
        let cfg = config(&cfg_path);
        assert_eq!(cfg.windows.len(), 2);
        assert!(cfg.windows[0].groups.is_empty());
        assert_eq!(cfg.windows[1].groups.len(), 3);
        assert_eq!(cfg.windows[1].colour.as_deref(), Some("#2a6f4e"));
    }

    #[test]
    fn a_group_already_in_the_config_takes_the_documents_it_lacks() {
        let (_d, cfg_path, session_path) = paths();
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        // The Club root is already a group, in another window, without the Budget.
        std::fs::write(
            &cfg_path,
            format!(
                "title = \"flotilla\"\n[[window]]\ntitle = \"flotilla\"\n[[window]]\ntitle = \"Club\"\n\
                 [[window.group]]\nname = \"Root\"\nurl = \"{CLUB}\"\n"
            ),
        )
        .unwrap();
        migrate(&cfg_path, &session_path, &v1()).unwrap();
        let cfg = config(&cfg_path);
        let club = &cfg.windows[1];
        assert_eq!(club.groups[0].name, "Root");
        assert_eq!(club.groups[0].tabs[0].url, BUDGET);
        assert_eq!(cfg.windows[0].groups.len(), 2, "the other v1 groups");
    }

    #[test]
    fn migration_runs_once() {
        let (_d, cfg_path, session_path) = paths();
        migrate(&cfg_path, &session_path, &v1()).unwrap();
        // The session file is v2 now: nothing left to migrate.
        assert!(matches!(session::load(&session_path), Loaded::Restored(_)));
        // Run again anyway: nothing is added twice.
        migrate(&cfg_path, &session_path, &v1()).unwrap();
        let cfg = config(&cfg_path);
        assert_eq!(cfg.windows[0].groups.len(), 3);
        assert_eq!(cfg.windows[0].groups[0].tabs.len(), 1);
    }
}
