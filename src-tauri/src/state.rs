//! The shell's managed state.

use crate::menu::MenuKey;
use crate::persist::Persist;
use flotilla_core::model::{TabId, Workspace};
use shell_core::compositing::HoleRect;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Lock discipline: `ws` is never held while calling a webview API (`add_child`, `navigate`,
/// `close`, show/hide): lock, copy or decide, drop, then act. WebKit runs `intercept` on the main
/// thread from inside those calls' callbacks, and it locks `ws`. Nor is any of these locks (or the
/// gate) held across a `WindowSet` call that reaches the host (`materialize`, `apply`,
/// `open_or_focus`, `rename`, `delete`): the host takes them.
///
/// Lock order: `gate`, then `ws`, then `created`, then `warm`, then `hole`, then `failed`; and `Persist`'s
/// writer lock before `ws` (the session snapshot is taken inside it). `menu`, `notice` and
/// `chrome_queue` are each taken alone.
pub struct AppState {
    /// Serialises config writes and reloads (`crate::write::Gate`).
    pub gate: crate::write::Gate,
    pub ws: Mutex<Workspace>,
    /// Each open window's last-reported content hole, by window label: where its new content
    /// webviews are placed.
    pub hole: Mutex<HashMap<String, HoleRect>>,
    /// The tabs whose content webview exists in each open window, by window label.
    pub created: Mutex<HashMap<String, HashSet<TabId>>>,
    /// Tabs the session saved as loaded whose webview is not created yet: each is created, hidden,
    /// when its window next syncs open. Saved as loaded along with `created`.
    pub warm: Mutex<HashSet<TabId>>,
    /// Tabs whose webview could not be created; not retried (or re-announced) until the tab is
    /// next selected, or sent Home while active.
    pub failed: Mutex<HashSet<TabId>>,
    /// What the menu bar was last built from (`menu::refresh`).
    pub menu: Mutex<Option<MenuKey>>,
    /// The app-wide config in force (the last good one).
    pub config: Mutex<crate::config::Config>,
    pub persist: Persist,
    /// The error bar's notices (`tabs::notice`).
    pub notice: Mutex<Notices>,
    /// Events for chromes still mounting (`ChromeQueue`).
    pub chrome_queue: Mutex<ChromeQueue>,
}

/// Events for a window whose chrome is not listening yet: a spine command sent from a pop-out to
/// the window its tab came from, when that window had to be reopened for it. Handed over when
/// the chrome mounts (`commands::startup_notice`).
#[derive(Debug, Default)]
pub struct ChromeQueue {
    held: HashMap<String, Vec<(String, serde_json::Value)>>,
}

impl ChromeQueue {
    pub fn hold(&mut self, label: &str, event: &str, payload: serde_json::Value) {
        self.held
            .entry(label.to_string())
            .or_default()
            .push((event.to_string(), payload));
    }

    /// Window `label`'s held events, in order, once.
    pub fn take(&mut self, label: &str) -> Vec<(String, serde_json::Value)> {
        self.held.remove(label).unwrap_or_default()
    }
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
    fn events_for_a_mounting_chrome_are_handed_over_once_in_order() {
        let mut q = ChromeQueue::default();
        q.hold("a", "nav-tab", serde_json::json!({ "dir": 1 }));
        q.hold("a", "jump-tab", serde_json::json!(3));
        q.hold("b", "check-update", serde_json::json!(null));
        let a = q.take("a");
        assert_eq!(
            a.iter().map(|(e, _)| e.as_str()).collect::<Vec<_>>(),
            ["nav-tab", "jump-tab"]
        );
        assert!(q.take("a").is_empty());
        assert_eq!(q.take("b").len(), 1);
    }

    #[test]
    fn a_quiet_launch_mounts_to_nothing() {
        assert_eq!(Notices::default().mount(), None);
    }
}
