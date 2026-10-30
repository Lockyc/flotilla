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
/// session snapshot is taken inside it). `menu` and `notice` are each taken alone.
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
    /// The app-wide config in force (the last good one).
    pub config: Mutex<crate::config::Config>,
    pub persist: Persist,
    /// The error bar's notices (`tabs::notice`).
    pub notice: Mutex<Notices>,
}

/// The error bar's notices. Until the chrome has mounted (`startup_notice`) nothing is listening,
/// so notices raised at launch (a quarantined session, a broken config, a restored tab that could
/// not be created) are held and handed over then; after it, each is shown as it comes.
#[derive(Debug, Default)]
pub struct Notices {
    held: Option<String>,
    mounted: bool,
}

impl Notices {
    /// `msg` to show now, or `None` when it is held for the mount.
    pub fn raise(&mut self, msg: String) -> Option<String> {
        if self.mounted {
            return Some(msg);
        }
        self.held = Some(match self.held.take() {
            Some(earlier) => format!("{earlier} {msg}"),
            None => msg,
        });
        None
    }

    /// The chrome has mounted: everything held so far, once.
    pub fn mount(&mut self) -> Option<String> {
        self.mounted = true;
        self.held.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_before_the_chrome_mounts_are_handed_over_at_mount() {
        let mut n = Notices::default();
        assert_eq!(n.raise("Session quarantined.".into()), None);
        assert_eq!(n.raise("Couldn't open x: no".into()), None);
        assert_eq!(
            n.mount().as_deref(),
            Some("Session quarantined. Couldn't open x: no")
        );
        assert_eq!(n.raise("later".into()).as_deref(), Some("later"));
        assert_eq!(n.mount(), None);
    }

    #[test]
    fn a_quiet_launch_mounts_to_nothing() {
        assert_eq!(Notices::default().mount(), None);
    }
}
