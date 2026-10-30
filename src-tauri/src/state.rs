//! The shell's managed state.

use crate::menu::MenuState;
use crate::persist::Persist;
use flotilla_core::model::{TabId, Workspace};
use shell_core::compositing::HoleRect;
use std::collections::HashSet;
use std::sync::Mutex;

/// Lock discipline: `ws` is never held while calling a webview API (`add_child`, `navigate`,
/// `close`, show/hide): lock, copy or decide, drop, then act. WebKit runs `intercept` on the main
/// thread from inside those calls' callbacks, and it locks `ws`.
///
/// Lock order: `ws`, then `created`, then `failed`; and `Persist`'s writer lock before `ws` (the
/// session snapshot is taken inside it). `menu` is taken alone.
pub struct AppState {
    pub ws: Mutex<Workspace>,
    /// The chrome's last-reported content hole, where new content webviews are placed.
    pub hole: Mutex<HoleRect>,
    /// Tabs whose content webview exists in the main window. Restored tabs start absent and are
    /// created when first shown.
    pub created: Mutex<HashSet<TabId>>,
    /// Tabs whose webview could not be created; not retried (or re-announced) until the tab is
    /// next selected, or sent Home while active.
    pub failed: Mutex<HashSet<TabId>>,
    /// What the menu bar was last built from (`menu::refresh`).
    pub menu: Mutex<Option<MenuState>>,
    pub persist: Persist,
    /// The launch-time notice for the error bar (a quarantined or unpersistable session).
    pub notice: Option<String>,
}
