//! The main window's content webviews: one per shown tab, labelled `main:<TabId>`, created lazily
//! and brought in line with the workspace by [`sync`].

use flotilla_core::model::{TabId, Workspace};
use std::collections::HashSet;

/// The content webview label for tab `id` in the main window.
pub fn label(id: &str) -> String {
    format!("{}:{id}", crate::MAIN_WINDOW)
}

/// The tab a main-window content webview label belongs to.
pub fn tab_id(label: &str) -> Option<TabId> {
    label
        .strip_prefix(crate::MAIN_WINDOW)?
        .strip_prefix(':')
        .map(str::to_string)
}

/// A tab whose webview is to be created.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub id: TabId,
    pub home_url: String,
    /// The group's login store: the webview's session seed.
    pub store: String,
}

/// What [`sync`] does to bring the main window's webviews in line with the workspace.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ViewPlan {
    /// Created webviews whose tab is no longer open here (closed, or popped out).
    pub close: Vec<TabId>,
    /// The active tab, when it has no webview yet. No other tab is ever created.
    pub create: Option<Target>,
    pub show: Option<TabId>,
    /// Every other remaining webview.
    pub hide: Vec<TabId>,
}

pub fn plan(ws: &Workspace, created: &HashSet<TabId>) -> ViewPlan {
    let here = |id: &TabId| ws.find(id).is_some_and(|(_, t)| !t.detached);
    let mut close: Vec<TabId> = created.iter().filter(|id| !here(id)).cloned().collect();
    close.sort();
    let show = ws.active.clone().filter(here);
    let create = show
        .as_ref()
        .filter(|id| !created.contains(*id))
        .and_then(|id| ws.find(id))
        .map(|(g, t)| Target {
            id: t.id.clone(),
            home_url: t.home_url.clone(),
            store: g.store.clone(),
        });
    let mut hide: Vec<TabId> = created
        .iter()
        .filter(|id| here(id) && Some(*id) != show.as_ref())
        .cloned()
        .collect();
    hide.sort();
    ViewPlan {
        close,
        create,
        show,
        hide,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_core::model::Opened;
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    const DOC_A: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit";
    const DOC_B: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt02/edit";

    /// A folder head with two docs; the second doc is active.
    fn ws() -> (Workspace, TabId, TabId, TabId) {
        let mut ws = Workspace::default();
        let Opened::Added(h) =
            ws.new_group(&url("https://drive.google.com/drive/folders/f1"), "default")
        else {
            panic!()
        };
        let Some(Opened::Added(a)) = ws.open_from(&h, &url(DOC_A)) else {
            panic!()
        };
        let Some(Opened::Added(b)) = ws.open_from(&h, &url(DOC_B)) else {
            panic!()
        };
        (ws, h, a, b)
    }

    fn set(ids: &[&TabId]) -> HashSet<TabId> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn labels_round_trip() {
        assert_eq!(label("t-3"), "main:t-3");
        assert_eq!(tab_id("main:t-3").as_deref(), Some("t-3"));
        assert_eq!(tab_id("main"), None);
        assert_eq!(tab_id("other:t-3"), None);
    }

    #[test]
    fn a_restored_workspace_creates_only_the_active_tab() {
        let (ws, _, _, b) = ws();
        let p = plan(&ws, &HashSet::new());
        assert_eq!(
            p,
            ViewPlan {
                close: vec![],
                create: Some(Target {
                    id: b.clone(),
                    home_url: DOC_B.into(),
                    store: "default".into(),
                }),
                show: Some(b),
                hide: vec![],
            }
        );
    }

    #[test]
    fn selecting_a_created_tab_hides_the_others_without_creating() {
        let (mut ws, h, a, b) = ws();
        ws.activate(&a);
        let p = plan(&ws, &set(&[&h, &a, &b]));
        assert_eq!(p.create, None);
        assert_eq!(p.show, Some(a));
        let mut hide = vec![h, b];
        hide.sort();
        assert_eq!(p.hide, hide);
        assert!(p.close.is_empty());
    }

    #[test]
    fn closed_and_popped_out_tabs_lose_their_webview() {
        let (mut ws, h, a, b) = ws();
        ws.close_tab(&b);
        ws.set_detached(&a, true);
        let p = plan(&ws, &set(&[&h, &a, &b]));
        let mut close = vec![a, b];
        close.sort();
        assert_eq!(p.close, close);
        // Closing b activated a, which is now popped out: nothing here to show.
        assert_eq!(p.show, None);
        assert_eq!(p.hide, vec![h]);
    }

    #[test]
    fn nothing_open_shows_nothing() {
        let (mut ws, h, a, b) = ws();
        ws.close_group(&h);
        let p = plan(&ws, &set(&[&h, &a, &b]));
        assert_eq!(p.show, None);
        assert_eq!(p.create, None);
        assert_eq!(p.close.len(), 3);
    }
}
