//! flotilla's config: one TOML file of app-wide settings and the windows, groups and documents
//! (`[[window]]` / `[[window.group]]` / `[[window.group.tab]]`), hot-reloaded.

use config_core::session::resolve;
use config_core::{Colour, Density, TabDigitKeys};
use flotilla_core::identity::{identity, listed, DocKey};
use flotilla_core::model::{ConfigGroup, ConfigTab, ConfigWindow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use url::Url;

/// The starter config written by "Create a starter config"; every key it holds is a default.
pub const DEFAULT_CONFIG: &str = include_str!("../../src/default-config.toml");

/// Env var that overrides the config path.
const CONFIG_ENV: &str = "FLOTILLA_CONFIG";

/// The directory under `~/.config` holding the config file.
const CONFIG_DIR: &str = "flotilla";

/// `deny_unknown_fields`: a mistyped key is an error the sidebar shows, with the last good config
/// kept, rather than a setting that silently does nothing.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Chrome sizing. Hot-reloads.
    pub density: Density,
    /// Forces a dark appearance. Hot-reloads.
    pub dark_mode: bool,
    /// What ⌘1/⌘2 do: jump to tabs 1 and 2, or cycle next/previous. Hot-reloads.
    pub tab_digit_keys: TabDigitKeys,
    /// Whether the chrome checks for updates by itself. Read when the chrome mounts.
    pub auto_update: bool,
    /// Hosts whose invalid TLS certificate is accepted. Applied at launch only.
    pub allow_insecure: Vec<String>,
    /// Reformat the file in house style after each clean reload.
    pub format_on_save: bool,
    /// Pin the Open section in every window whose own `open_tabs_section` is unset. Hot-reloads.
    pub open_tabs_section: bool,
    /// What a collapsed sidebar shows down the window's left edge. Hot-reloads.
    pub rail: Rail,
    /// The app-wide login session: the bottom of the cascade a tab's own, its group's and its
    /// window's `session` override (`config_core::session`).
    pub session: Option<String>,
    /// The windows, in file order; `parse` normalises each `colour` to `#rrggbb`. A file with no
    /// `[[window]]` has none; the default (a missing file, or the starter) has [`STARTER_WINDOW`].
    #[serde(rename = "window", default)]
    pub windows: Vec<FlotillaWindow>,
}

/// chrome-core's collapsed-sidebar rail (`RAIL_STYLES`), as the lowercase token its DTO takes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rail {
    /// The strip's window colour carried down the edge, with an expand chevron.
    #[default]
    Handle,
    /// The bare strip.
    Edge,
    /// A mark per tab: active, loaded, attention.
    Map,
}

/// The title of the starter config's one window.
pub const STARTER_WINDOW: &str = "flotilla";

/// One `[[window]]`: config-core's shared keys, groups headed by a URL, documents as tabs.
pub type FlotillaWindow = config_core::Window<DocTab, WindowExtras, FlotillaGroup>;

/// A `[[window]]`'s own keys, beside config-core's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowExtras {
    /// Overrides the global `open_tabs_section` for this window.
    pub open_tabs_section: Option<bool>,
    /// The login session the window's groups and documents open in, unless they set their own.
    pub session: Option<String>,
    /// The `[[window.tab]]`s: documents in no group (the window's Unsorted).
    #[serde(rename = "tab")]
    pub unsorted: Vec<DocTab>,
}

/// A `[[window.group]]`'s own keys, beside config-core's `name` and `tab`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlotillaGroup {
    /// The group's head: a Drive folder or a document. Its Home.
    pub url: String,
    /// The login session the group's head and documents open in, unless a document sets its
    /// own; unset, the window's.
    #[serde(default)]
    pub session: Option<String>,
    /// The sidebar shows the head alone.
    #[serde(default)]
    pub collapsed: bool,
    /// The head is popped out; reopens in its own window.
    #[serde(default)]
    pub detached: bool,
    /// `name` is the user's: the folder's title no longer replaces it.
    #[serde(default)]
    pub named: bool,
}

/// A `[[window.group.tab]]`: a document opened from its group.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocTab {
    /// The sidebar name: the document's own title, unless `named`.
    pub title: String,
    /// The document, and its Home.
    pub url: String,
    /// Popped out; reopens in its own window.
    #[serde(default)]
    pub detached: bool,
    /// `title` is the user's: the document's title no longer replaces it.
    #[serde(default)]
    pub named: bool,
    /// The login session the document opens in; unset, its group's (or, in Unsorted, its
    /// window's).
    #[serde(default)]
    pub session: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            windows: vec![config_core::Window {
                title: STARTER_WINDOW.into(),
                width: config_core::default_window_width(),
                height: config_core::default_window_height(),
                colour: None,
                open_on_start: true,
                groups: vec![],
                extra: WindowExtras::default(),
            }],
            density: Density::Comfortable,
            dark_mode: false,
            tab_digit_keys: TabDigitKeys::Jump,
            auto_update: true,
            allow_insecure: Vec::new(),
            format_on_save: false,
            open_tabs_section: false,
            rail: Rail::Handle,
            session: None,
        }
    }
}

impl Config {
    /// The accent of the window titled `title`, `None` when neutral (or no such window).
    pub fn colour_of(&self, title: &str) -> Option<String> {
        self.windows
            .iter()
            .find(|w| w.title.trim() == title.trim())
            .and_then(|w| w.colour.clone())
    }

    /// Whether the window titled `title` pins the Open section: its own `open_tabs_section`, else
    /// the global one.
    pub fn open_section_of(&self, title: &str) -> bool {
        self.windows
            .iter()
            .find(|w| w.title.trim() == title.trim())
            .and_then(|w| w.extra.open_tabs_section)
            .unwrap_or(self.open_tabs_section)
    }

    /// The windows as the workspace model reads them.
    pub fn workspace_windows(&self) -> Vec<ConfigWindow> {
        self.windows
            .iter()
            .map(|w| ConfigWindow {
                title: w.title.clone(),
                store: self.window_store(w),
                unsorted: w.extra.unsorted.iter().map(config_tab).collect(),
                groups: w
                    .groups
                    .iter()
                    .map(|g| ConfigGroup {
                        name: g.name.clone(),
                        url: g.extra.url.clone(),
                        session: g.extra.session.clone(),
                        collapsed: g.extra.collapsed,
                        detached: g.extra.detached,
                        named: g.extra.named,
                        tabs: g.tabs.iter().map(config_tab).collect(),
                    })
                    .collect(),
            })
            .collect()
    }

    /// The store window `w` resolves to: its `session`, else the app's, else the default.
    fn window_store(&self, w: &FlotillaWindow) -> String {
        resolve([w.extra.session.as_deref(), self.session.as_deref()])
    }
}

fn config_tab(t: &DocTab) -> ConfigTab {
    ConfigTab {
        title: t.title.clone(),
        url: t.url.clone(),
        detached: t.detached,
        named: t.named,
        session: t.session.clone(),
    }
}

/// Parses config source; the message is what the sidebar shows. Beyond config-core's shared
/// window checks, every `url` is an http(s) address and a document is open once: no two heads or
/// tabs, in any window, share an identity (`flotilla_core::identity::identity`), and a list of
/// tabs holds a document once in any session (`flotilla_core::identity::listed`).
pub fn parse(src: &str) -> Result<Config, String> {
    let mut cfg: Config = toml::from_str(src).map_err(|e| format!("config: {}", e.message()))?;
    config_core::validate_windows(&cfg.windows).map_err(|e| format!("config: {e}"))?;
    for w in &mut cfg.windows {
        if let Some(c) = &w.colour {
            w.colour = Some(Colour::parse(c).map_err(|e| format!("config: {e}"))?.hex());
        }
    }
    let mut seen: HashMap<DocKey, String> = HashMap::new();
    let mut claim = |at: String, store: &str, url: &str| -> Result<(DocKey, String), String> {
        let parsed = Url::parse(url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .ok_or_else(|| format!("config: {at}: `{url}` is not an http(s) address"))?;
        let key = identity(store, &parsed);
        match seen.insert(key.clone(), at.clone()) {
            Some(first) => Err(format!(
                "config: {first} and {at} are the same document, which opens once"
            )),
            None => Ok((key, at)),
        }
    };
    let once_listed = |list: &[(DocKey, String)]| -> Result<(), String> {
        for (i, (key, at)) in list.iter().enumerate() {
            if let Some((_, first)) = list[..i].iter().find(|(k, _)| listed(k) == listed(key)) {
                return Err(format!(
                    "config: {first} and {at} are the same document, which a list holds once"
                ));
            }
        }
        Ok(())
    };
    for w in &cfg.windows {
        let window = w.title.trim();
        let window_store = cfg.window_store(w);
        let under = |own: &Option<String>, place: &str| resolve([own.as_deref(), Some(place)]);
        let unsorted = w
            .extra
            .unsorted
            .iter()
            .map(|t| {
                let at = format!("`{}` (window `{window}`)", t.title.trim());
                claim(at, &under(&t.session, &window_store), &t.url)
            })
            .collect::<Result<Vec<_>, _>>()?;
        once_listed(&unsorted)?;
        for g in &w.groups {
            let group = g.name.trim();
            let at = |name: &str| format!("`{name}` (window `{window}`, group `{group}`)");
            let group_store = under(&g.extra.session, &window_store);
            claim(at(group), &group_store, &g.extra.url)?;
            let members = g
                .tabs
                .iter()
                .map(|t| claim(at(t.title.trim()), &under(&t.session, &group_store), &t.url))
                .collect::<Result<Vec<_>, _>>()?;
            once_listed(&members)?;
        }
    }
    Ok(cfg)
}

/// Where the config lives: `$FLOTILLA_CONFIG` if set, else `~/.config/flotilla/config.toml`.
pub fn config_path() -> PathBuf {
    config_core::resolve_config_path(CONFIG_ENV, CONFIG_DIR)
}

/// Loads the config at `path`. A missing file is the default config; an unreadable or invalid one
/// is an error.
pub fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(src) => parse(&src),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("config: {}: {e}", path.display())),
    }
}

/// Writes the starter config unless a file is already there. `Ok(true)` when written.
pub fn create_starter(path: &Path) -> Result<bool, String> {
    config_core::write_default_config(path, DEFAULT_CONFIG).map_err(|e| e.to_string())
}

/// Flips `tab_digit_keys` in the file (the Cycle Tabs toggle), seeding the starter config first
/// when there is none. The watcher's reload then applies it.
pub fn toggle_tab_digit_keys(path: &Path) -> Result<(), String> {
    create_starter(path)?;
    config_core::toggle_tab_digit_keys(path)
        .map(|_| ())
        .map_err(|e| format!("config: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every top-level key the app reads; the starter config sets each one.
    const KEYS: [&str; 9] = [
        "window",
        "density",
        "dark_mode",
        "tab_digit_keys",
        "auto_update",
        "allow_insecure",
        "format_on_save",
        "open_tabs_section",
        "rail",
    ];
    /// Every `[[window]]`, `[[window.group]]` and `[[window.group.tab]]` key; the starter
    /// config's commented example documents each one.
    const WINDOW_KEYS: [&str; 9] = [
        "title",
        "colour",
        "width",
        "height",
        "open_on_start",
        "open_tabs_section",
        "session",
        "tab",
        "group",
    ];
    const GROUP_KEYS: [&str; 7] = [
        "name",
        "url",
        "session",
        "collapsed",
        "detached",
        "named",
        "tab",
    ];
    const TAB_KEYS: [&str; 5] = ["title", "url", "detached", "named", "session"];

    const FOLDER: &str = "https://drive.google.com/drive/folders/0AbCdEfGhIjKlMnOpQ";
    const SHEET: &str = "https://docs.google.com/spreadsheets/d/1AbCdEfGhIjKlMnOpQrSt04/edit";
    const SLIDES: &str = "https://docs.google.com/presentation/d/1AbCdEfGhIjKlMnOpQrSt05/edit";

    /// The starter's commented `[[window]]` example, uncommented.
    fn starter_example() -> String {
        let at = DEFAULT_CONFIG
            .find("# [[window]]")
            .expect("starter config documents [[window]]");
        DEFAULT_CONFIG[at..]
            .lines()
            .map(|l| l.strip_prefix('#').unwrap_or(l))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn assert_keys(table: &toml::Table, keys: &[&str], what: &str) {
        for key in keys {
            assert!(table.contains_key(*key), "starter {what} lacks `{key}`");
        }
        assert_eq!(
            table.len(),
            keys.len(),
            "starter {what} has an unlisted key"
        );
    }

    #[test]
    fn parses_every_documented_key() {
        let table: toml::Table = DEFAULT_CONFIG.parse().unwrap();
        assert_keys(&table, &KEYS, "config");
        // deny_unknown_fields makes this fail for any key the struct does not know.
        assert_eq!(parse(DEFAULT_CONFIG).unwrap(), Config::default());

        let example = starter_example();
        let table: toml::Table = example.parse().unwrap();
        let window = table["window"].as_array().unwrap()[0].as_table().unwrap();
        assert_keys(window, &WINDOW_KEYS, "[[window]]");
        let group = window["group"].as_array().unwrap()[0].as_table().unwrap();
        assert_keys(group, &GROUP_KEYS, "[[window.group]]");
        let tab = group["tab"].as_array().unwrap()[0].as_table().unwrap();
        assert_keys(tab, &TAB_KEYS, "[[window.group.tab]]");
        let loose = window["tab"].as_array().unwrap()[0].as_table().unwrap();
        assert_keys(loose, &TAB_KEYS, "[[window.tab]]");
        let cfg = parse(&example).unwrap();
        assert_eq!(cfg.windows[0].groups[0].tabs.len(), 1);
        assert_eq!(cfg.windows[0].extra.unsorted.len(), 1);
    }

    #[test]
    fn every_key_takes_effect() {
        let cfg = parse(&format!(
            "density = \"compact\"\ndark_mode = true\ntab_digit_keys = \"cycle\"\n\
             auto_update = false\nallow_insecure = [\"10.0.0.1\"]\nformat_on_save = true\n\
             open_tabs_section = true\nrail = \"map\"\nsession = \"app\"\n\
             [[window]]\ntitle = \"Club\"\ncolour = \"#2A6F4E\"\nwidth = 1200\nheight = 800\n\
             open_on_start = false\nopen_tabs_section = false\nsession = \"window\"\n\
             [[window.group]]\nname = \"Newsletters\"\nurl = \"{FOLDER}\"\nsession = \"club\"\n\
             collapsed = true\ndetached = true\nnamed = true\n\
             [[window.group.tab]]\ntitle = \"Budget\"\nurl = \"{SHEET}\"\ndetached = true\n\
             named = true\nsession = \"tab\"\n"
        ))
        .unwrap();
        assert_eq!(cfg.session.as_deref(), Some("app"));
        assert_eq!(cfg.windows[0].extra.session.as_deref(), Some("window"));
        assert_eq!(cfg.density, Density::Compact);
        assert_eq!(cfg.rail, Rail::Map);
        assert!(cfg.tab_digit_keys.is_cycle());
        assert!(cfg.dark_mode && cfg.format_on_save && !cfg.auto_update);
        assert_eq!(cfg.allow_insecure, vec!["10.0.0.1".to_string()]);
        let w = &cfg.windows[0];
        assert_eq!(w.title, "Club");
        assert_eq!(w.colour.as_deref(), Some("#2a6f4e"));
        assert_eq!((w.width, w.height, w.open_on_start), (1200, 800, false));
        assert!(cfg.open_tabs_section && !cfg.open_section_of("Club"));
        let g = &w.groups[0];
        assert_eq!(g.name, "Newsletters");
        assert_eq!(
            g.extra,
            FlotillaGroup {
                url: FOLDER.into(),
                session: Some("club".into()),
                collapsed: true,
                detached: true,
                named: true,
            }
        );
        assert_eq!(
            g.tabs,
            vec![DocTab {
                title: "Budget".into(),
                url: SHEET.into(),
                detached: true,
                named: true,
                session: Some("tab".into()),
            }]
        );
    }

    #[test]
    fn the_starter_window_is_the_default_and_a_file_without_windows_has_none() {
        assert_eq!(Config::default().windows[0].title, STARTER_WINDOW);
        let cfg = parse("[[window]]\ntitle = \"Club\"\ncolour = \"#2A6F4E\"\n").unwrap();
        assert_eq!(cfg.colour_of(" Club ").as_deref(), Some("#2a6f4e"));
        assert_eq!(cfg.colour_of("Other"), None);
        assert!(parse("dark_mode = true\n").unwrap().windows.is_empty());
    }

    #[test]
    fn a_window_inherits_the_open_section_unless_it_sets_its_own() {
        let cfg = parse(
            "open_tabs_section = true\n[[window]]\ntitle = \"A\"\n\
             [[window]]\ntitle = \"B\"\nopen_tabs_section = false\n",
        )
        .unwrap();
        assert!(cfg.open_section_of("A"));
        assert!(!cfg.open_section_of("B"));
        let cfg = parse("[[window]]\ntitle = \"A\"\nopen_tabs_section = true\n").unwrap();
        assert!(cfg.open_section_of("A"));
        assert!(!Config::default().open_section_of(STARTER_WINDOW));
    }

    #[test]
    fn workspace_windows_mirror_the_config() {
        let cfg = parse(&format!(
            "[[window]]\ntitle = \" Club \"\n\
             [[window.group]]\nname = \"A\"\nurl = \"{FOLDER}\"\ncollapsed = true\n\
             [[window.group.tab]]\ntitle = \"Budget\"\nurl = \"{SHEET}\"\ndetached = true\n"
        ))
        .unwrap();
        assert_eq!(
            cfg.workspace_windows(),
            vec![ConfigWindow {
                store: "default".into(),
                unsorted: vec![],
                title: " Club ".into(),
                groups: vec![ConfigGroup {
                    name: "A".into(),
                    url: FOLDER.into(),
                    session: None,
                    collapsed: true,
                    detached: false,
                    named: false,
                    tabs: vec![ConfigTab {
                        title: "Budget".into(),
                        url: SHEET.into(),
                        detached: true,
                        named: false,
                        session: None,
                    }],
                }],
            }]
        );
    }

    #[test]
    fn parses_windows_groups_and_docs() {
        let cfg = parse(&format!(
            "[[window]]\ntitle = \"Club\"\n\
             [[window.group]]\nname = \"Club drive\"\nurl = \"{FOLDER}\"\n\
             [[window.group.tab]]\ntitle = \"Budget\"\nurl = \"{SHEET}\"\n\
             [[window.group.tab]]\ntitle = \"Newsletter\"\nurl = \"{SLIDES}\"\n\
             [[window]]\ntitle = \"Home\"\nsession = \"home\"\n\
             [[window.group]]\nname = \"Budget\"\nurl = \"{SHEET}\"\n"
        ))
        .unwrap();
        assert_eq!(cfg.windows.len(), 2);
        let club = &cfg.windows[0].groups[0];
        assert_eq!(club.extra.session, None);
        assert!(!club.extra.collapsed);
        let titles: Vec<_> = club.tabs.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Budget", "Newsletter"]);
        assert!(club.tabs.iter().all(|t| !t.detached));
        // The same document in another store is another account's copy, not a second one.
        assert_eq!(cfg.windows[1].groups[0].extra.url, SHEET);
    }

    #[test]
    fn duplicate_doc_in_config_is_an_error() {
        let view = SHEET.replace("/edit", "/view");
        let err = parse(&format!(
            "[[window]]\ntitle = \"Club\"\n\
             [[window.group]]\nname = \"A\"\nurl = \"{FOLDER}\"\n\
             [[window.group.tab]]\ntitle = \"Budget\"\nurl = \"{SHEET}\"\n\
             [[window.group]]\nname = \"B\"\nurl = \"{view}\"\n"
        ))
        .unwrap_err();
        assert!(err.contains("`Budget`") && err.contains("`B`"), "{err}");
    }

    #[test]
    fn a_list_holds_a_document_once_in_any_session() {
        let list = |place: &str, tab: &str| {
            parse(&format!(
                "[[window]]\ntitle = \"Club\"\n{place}\
                 {tab}\ntitle = \"Budget\"\nurl = \"{SHEET}\"\n\
                 {tab}\ntitle = \"Mine\"\nurl = \"{SHEET}\"\nsession = \"me\"\n"
            ))
        };
        let group = format!("[[window.group]]\nname = \"A\"\nurl = \"{FOLDER}\"\n");
        let err = list(&group, "[[window.group.tab]]").unwrap_err();
        assert!(err.contains("`Budget`") && err.contains("`Mine`"), "{err}");
        let err = list("", "[[window.tab]]").unwrap_err();
        assert!(err.contains("`Budget`") && err.contains("`Mine`"), "{err}");
        // In two lists, each session's copy is its own.
        assert!(parse(&format!(
            "[[window]]\ntitle = \"Club\"\n\
             [[window.tab]]\ntitle = \"Mine\"\nurl = \"{SHEET}\"\nsession = \"me\"\n\
             [[window.group]]\nname = \"A\"\nurl = \"{FOLDER}\"\n\
             [[window.group.tab]]\ntitle = \"Budget\"\nurl = \"{SHEET}\"\n"
        ))
        .is_ok());
    }

    #[test]
    fn duplicate_doc_across_windows_is_an_error() {
        let err = parse(&format!(
            "[[window]]\ntitle = \"Club\"\n\
             [[window.group]]\nname = \"A\"\nurl = \"{FOLDER}\"\n\
             [[window.group.tab]]\ntitle = \"Budget\"\nurl = \"{SHEET}\"\n\
             [[window]]\ntitle = \"Home\"\n\
             [[window.group]]\nname = \"B\"\nurl = \"{FOLDER}\"\n"
        ))
        .unwrap_err();
        assert!(
            err.contains("`A`") && err.contains("`B`") && err.contains("`Home`"),
            "{err}"
        );
    }

    #[test]
    fn a_document_opens_once_per_resolved_session() {
        // Two windows in two sessions hold the same folder and document.
        let two = |second: &str| {
            parse(&format!(
                "session = \"app\"\n\
                 [[window]]\ntitle = \"Admin\"\nsession = \"admin\"\n\
                 [[window.group]]\nname = \"A\"\nurl = \"{FOLDER}\"\n\
                 [[window.group.tab]]\ntitle = \"Budget\"\nurl = \"{SHEET}\"\n\
                 [[window]]\ntitle = \"Me\"\n{second}\
                 [[window.group]]\nname = \"B\"\nurl = \"{FOLDER}\"\n\
                 [[window.group.tab]]\ntitle = \"Budget\"\nurl = \"{SHEET}\"\n"
            ))
        };
        let cfg = two("").unwrap();
        let stores: Vec<_> = cfg
            .workspace_windows()
            .into_iter()
            .map(|w| w.store)
            .collect();
        assert_eq!(stores, ["admin", "app"]);
        // Naming the same session again makes them one copy, an error.
        assert!(two("session = \"admin\"\n").is_err());
        // A blank session falls through to the app's.
        assert!(two("session = \" \"\n").is_ok());
    }

    #[test]
    fn a_url_must_be_http() {
        let err = parse(
            "[[window]]\ntitle = \"W\"\n[[window.group]]\nname = \"A\"\nurl = \"file:///etc\"\n",
        )
        .unwrap_err();
        assert!(err.contains("file:///etc"), "{err}");
        assert!(parse(&format!(
            "[[window]]\ntitle = \"W\"\n[[window.group]]\nname = \"A\"\nurl = \"{FOLDER}\"\n\
             [[window.group.tab]]\ntitle = \"T\"\nurl = \"not a url\"\n"
        ))
        .is_err());
    }

    #[test]
    fn top_level_title_is_now_unknown() {
        assert!(parse("title = \"club\"").is_err());
        assert!(parse("colour = \"#2a6f4e\"").is_err());
        // A stray key in a group or tab is an error too, not a silently ignored setting.
        assert!(parse(&format!(
            "[[window]]\ntitle = \"W\"\n[[window.group]]\nname = \"A\"\nurl = \"{FOLDER}\"\n\
             colour = \"#fff\"\n"
        ))
        .is_err());
        // The shared window checks run: two windows with one title.
        assert!(parse("[[window]]\ntitle = \"W\"\n[[window]]\ntitle = \" W \"\n").is_err());
    }

    #[test]
    fn a_mistyped_key_is_an_error_and_a_missing_file_is_the_default() {
        assert!(parse("densty = \"compact\"").is_err());
        let unnamed = Config::default();
        assert_eq!(unnamed.colour_of(STARTER_WINDOW), None);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load(&dir.path().join("none.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn toggling_the_digit_mode_seeds_a_missing_config_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flotilla").join("config.toml");
        toggle_tab_digit_keys(&path).unwrap();
        let src = std::fs::read_to_string(&path).unwrap();
        // The starter's documentation survives; only the value changed.
        assert!(src.starts_with("# flotilla config"), "{src}");
        assert!(parse(&src).unwrap().tab_digit_keys.is_cycle());
        toggle_tab_digit_keys(&path).unwrap();
        assert!(!load(&path).unwrap().tab_digit_keys.is_cycle());
    }

    #[test]
    fn the_starter_config_never_replaces_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flotilla").join("config.toml");
        assert!(create_starter(&path).unwrap());
        std::fs::write(&path, "dark_mode = true\n").unwrap();
        assert!(!create_starter(&path).unwrap());
        assert!(load(&path).unwrap().dark_mode);
    }
}
