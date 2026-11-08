//! Which group a located document belongs to: the one whose head shows the folder nearest above
//! it.

use crate::identity::head_container;
use crate::model::{TabId, Workspace};
use url::Url;

/// The head id of the first group, in any window, whose head container equals an entry of
/// `ancestors` (nearest first, possibly ending in the alias `"root"`) for `provider`. The nearest
/// ancestor with a group decides, so a nested folder's own group beats its parent's.
pub fn route(ws: &Workspace, provider: &str, ancestors: &[String]) -> Option<TabId> {
    let heads: Vec<(TabId, String)> = ws
        .windows
        .iter()
        .flat_map(|w| &w.groups)
        .filter_map(|g| {
            let url = Url::parse(&g.head.home_url).ok()?;
            let (p, container) = head_container(&url)?;
            (p == provider).then(|| (g.head.id.clone(), container))
        })
        .collect();
    ancestors.iter().find_map(|a| {
        heads
            .iter()
            .find(|(_, container)| container == a)
            .map(|(id, _)| id.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConfigGroup, ConfigWindow};

    fn window(title: &str, folders: &[&str]) -> ConfigWindow {
        ConfigWindow {
            title: title.into(),
            unsorted: vec![],
            groups: folders
                .iter()
                .map(|f| ConfigGroup {
                    name: (*f).into(),
                    url: format!("https://drive.google.com/drive/{f}"),
                    store: "default".into(),
                    ..Default::default()
                })
                .collect(),
        }
    }

    fn chain(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).into()).collect()
    }

    fn head(ws: &Workspace, wi: usize, gi: usize) -> TabId {
        ws.windows[wi].groups[gi].head.id.clone()
    }

    #[test]
    fn nearest_ancestor_wins() {
        let ws = Workspace::from_config(&[window("W", &["folders/CLUB", "folders/BULL"])]);
        let got = route(&ws, "google", &chain(&["Y2026", "BULL", "CLUB", "root"]));
        assert_eq!(got, Some(head(&ws, 0, 1)));
    }

    #[test]
    fn no_ancestor_is_a_head() {
        let ws = Workspace::from_config(&[window("W", &["folders/CLUB"])]);
        assert_eq!(route(&ws, "google", &chain(&["X", "root"])), None);
    }

    #[test]
    fn my_drive_head_matches_root() {
        let ws = Workspace::from_config(&[window("W", &["my-drive"])]);
        let got = route(&ws, "google", &chain(&["A", "root"]));
        assert_eq!(got, Some(head(&ws, 0, 0)));
    }

    #[test]
    fn heads_in_other_windows_count() {
        let ws = Workspace::from_config(&[window("A", &[]), window("B", &["folders/CLUB"])]);
        let got = route(&ws, "google", &chain(&["CLUB"]));
        assert_eq!(got, Some(head(&ws, 1, 0)));
    }
}
