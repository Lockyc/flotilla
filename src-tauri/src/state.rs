//! The shell's managed state.

use crate::persist::Persist;
use flotilla_core::model::{TabId, Workspace};
use shell_core::compositing::HoleRect;
use std::collections::HashSet;
use std::sync::Mutex;

/// Lock discipline: `ws` is never held while calling a webview API (`add_child`, `navigate`,
/// `close`, show/hide): lock, copy or decide, drop, then act. WebKit runs `intercept` on the main
/// thread from inside those calls' callbacks, and it locks `ws`.
pub struct AppState {
    pub ws: Mutex<Workspace>,
    /// The chrome's last-reported content hole, where new content webviews are placed.
    pub hole: Mutex<HoleRect>,
    /// Tabs whose content webview exists in the main window. Restored tabs start absent and are
    /// created when first shown.
    pub created: Mutex<HashSet<TabId>>,
    pub persist: Persist,
    /// The launch-time notice for the error bar (a quarantined or unpersistable session).
    pub notice: Option<String>,
}
