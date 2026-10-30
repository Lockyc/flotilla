//! flotilla's app-wide config: one TOML file of settings, hot-reloaded. The workspace itself is
//! not configured here; it forms as you work and lives in the session file.

use config_core::{Colour, Density, TabDigitKeys};
use serde::Deserialize;
use std::path::{Path, PathBuf};

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
    /// The window's name, empty for none. Hot-reloads.
    pub title: String,
    /// The window's `#rgb` / `#rrggbb` accent, empty for neutral; `parse` normalises it to
    /// `#rrggbb`. Hot-reloads.
    pub colour: String,
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            title: String::new(),
            colour: String::new(),
            density: Density::Comfortable,
            dark_mode: false,
            tab_digit_keys: TabDigitKeys::Jump,
            auto_update: true,
            allow_insecure: Vec::new(),
            format_on_save: false,
        }
    }
}

impl Config {
    /// The window's title: its name, or the app's when it has none.
    pub fn window_title(&self) -> String {
        if self.title.is_empty() {
            "flotilla".into()
        } else {
            self.title.clone()
        }
    }

    /// The window's accent, `None` when neutral.
    pub fn accent(&self) -> Option<String> {
        (!self.colour.is_empty()).then(|| self.colour.clone())
    }
}

/// Parses config source; the message is what the sidebar shows.
pub fn parse(src: &str) -> Result<Config, String> {
    let mut cfg: Config = toml::from_str(src).map_err(|e| format!("config: {}", e.message()))?;
    if !cfg.colour.is_empty() {
        cfg.colour = Colour::parse(&cfg.colour)
            .map_err(|e| format!("config: {e}"))?
            .hex();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key the app reads; the starter config documents each one.
    const KEYS: [&str; 8] = [
        "title",
        "colour",
        "density",
        "dark_mode",
        "tab_digit_keys",
        "auto_update",
        "allow_insecure",
        "format_on_save",
    ];

    #[test]
    fn parses_every_documented_key() {
        let table: toml::Table = DEFAULT_CONFIG.parse().unwrap();
        for key in KEYS {
            assert!(table.contains_key(key), "starter config lacks `{key}`");
        }
        assert_eq!(
            table.len(),
            KEYS.len(),
            "starter config has an unlisted key"
        );
        // deny_unknown_fields makes this fail for any key the struct does not know.
        assert_eq!(parse(DEFAULT_CONFIG).unwrap(), Config::default());
    }

    #[test]
    fn every_key_takes_effect() {
        let cfg = parse(
            "title = \"rotary\"\ncolour = \"#17458F\"\n\
             density = \"compact\"\ndark_mode = true\ntab_digit_keys = \"cycle\"\n\
             auto_update = false\nallow_insecure = [\"10.0.0.1\"]\nformat_on_save = true\n",
        )
        .unwrap();
        assert_eq!(cfg.window_title(), "rotary");
        assert_eq!(cfg.accent().as_deref(), Some("#17458f"));
        assert_eq!(cfg.density, Density::Compact);
        assert!(cfg.tab_digit_keys.is_cycle());
        assert!(cfg.dark_mode && cfg.format_on_save && !cfg.auto_update);
        assert_eq!(cfg.allow_insecure, vec!["10.0.0.1".to_string()]);
    }

    #[test]
    fn a_mistyped_key_is_an_error_and_a_missing_file_is_the_default() {
        assert!(parse("densty = \"compact\"").is_err());
        assert!(parse("colour = \"blue\"").is_err());
        let unnamed = Config::default();
        assert_eq!(
            (unnamed.window_title().as_str(), unnamed.accent()),
            ("flotilla", None)
        );
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load(&dir.path().join("none.toml")).unwrap(),
            Config::default()
        );
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
