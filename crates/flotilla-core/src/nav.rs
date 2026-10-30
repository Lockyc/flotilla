//! What each link or navigation does. The shell calls `decide` from shell-core's
//! `ContentHooks.intercept`; a document other than the tab's own opens as a member, everything
//! else passes through to shell-core's routing.

use crate::identity::doc_key;
use crate::model::{Opened, TabId, Workspace};
use url::Url;

/// How the shell was asked to go to the URL; mirrors shell-core's `LinkOrigin` so core stays
/// tauri-free. The rule in `decide` is origin-independent; the parameter carries the origin for
/// callers and logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    NewWindow,
    Click,
    Navigation,
}

/// What the shell does with the navigation.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// The workspace already added or raised a tab for the document; the shell consumes the
    /// navigation and shows that tab.
    Open(Opened),
    /// Not ours: shell-core's own routing runs.
    PassThrough,
}

/// The URL a typed or pasted new-group address names. Only web pages can head a group.
pub fn parse_group_url(input: &str) -> Result<Url, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("Paste a folder or page link to open it as a group.".into());
    }
    match Url::parse(s) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => Ok(u),
        _ => Err(format!("Not a web link: {s}")),
    }
}

/// Decides what `url`, requested from tab `from`, does. On `Open` this has already mutated `ws`;
/// the caller must then consume the navigation (return true from shell-core's intercept) so
/// WebKit does not also load the URL into the `from` webview. Creating or raising the webviews is
/// the caller's, deferred off the policy callback.
pub fn decide(ws: &mut Workspace, from: &TabId, url: &Url, _origin: Origin) -> Decision {
    let Some((_, group, tab)) = ws.find(from) else {
        return Decision::PassThrough;
    };
    match doc_key(&group.store, url) {
        Some(key) if key != tab.key => ws
            .open_from(from, url)
            .map_or(Decision::PassThrough, Decision::Open),
        _ => Decision::PassThrough,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ConfigWindow;

    const ORIGINS: [Origin; 3] = [Origin::NewWindow, Origin::Click, Origin::Navigation];

    fn doc(id: &str) -> Url {
        Url::parse(&format!("https://docs.google.com/document/d/{id}/edit")).unwrap()
    }
    const A: &str = "1AbCdEfGhIjKlMnOpQrStA1";
    const B: &str = "1AbCdEfGhIjKlMnOpQrStB2";

    fn one_window() -> Workspace {
        Workspace::from_config(&[ConfigWindow {
            title: "W".into(),
            groups: vec![],
        }])
    }

    fn new_group(w: &mut Workspace, u: &Url) -> Opened {
        w.new_group("W", u, "default").unwrap()
    }

    fn head(w: &mut Workspace) -> TabId {
        let u = Url::parse("https://drive.google.com/drive/folders/fold1").unwrap();
        match new_group(w, &u) {
            Opened::Added(i) | Opened::Raised(i) => i,
        }
    }

    /// A pasted `open?id=` share link heads one group: its redirect to the document is the head
    /// itself, not a member, and a document already open is raised instead of headed twice.
    #[test]
    fn an_open_id_head_redirecting_to_its_document_stays_one_tab() {
        let share = Url::parse(&format!("https://drive.google.com/open?id={A}")).unwrap();
        let mut w = one_window();
        let Opened::Added(h) = new_group(&mut w, &share) else {
            panic!()
        };
        for o in ORIGINS {
            assert_eq!(decide(&mut w, &h, &doc(A), o), Decision::PassThrough);
        }
        assert!(w.windows[0].groups[0].members.is_empty());
        assert_eq!(new_group(&mut w, &doc(A)), Opened::Raised(h.clone()));

        let mut w = one_window();
        let f = head(&mut w);
        let Some(Opened::Added(m)) = w.open_from(&f, &doc(B)) else {
            panic!()
        };
        let share = Url::parse(&format!("https://drive.google.com/open?id={B}")).unwrap();
        assert_eq!(new_group(&mut w, &share), Opened::Raised(m));
    }

    #[test]
    fn doc_link_from_head_opens_member() {
        for o in ORIGINS {
            let mut w = one_window();
            let h = head(&mut w);
            assert!(matches!(
                decide(&mut w, &h, &doc(A), o),
                Decision::Open(Opened::Added(_))
            ));
        }
    }

    #[test]
    fn doc_open_elsewhere_raises_it() {
        let mut w = one_window();
        let h = head(&mut w);
        let Some(Opened::Added(m)) = w.open_from(&h, &doc(A)) else {
            panic!()
        };
        let other = Url::parse("https://drive.google.com/drive/folders/fold2").unwrap();
        let Opened::Added(h2) = new_group(&mut w, &other) else {
            panic!()
        };
        assert_eq!(
            decide(&mut w, &h2, &doc(A), Origin::Click),
            Decision::Open(Opened::Raised(m))
        );
        assert!(w.find(&h2).is_some_and(|(_, g, _)| g.members.is_empty()));
    }

    #[test]
    fn same_doc_redirect_passes_through() {
        let mut w = one_window();
        let h = head(&mut w);
        let Some(Opened::Added(m)) = w.open_from(&h, &doc(A)) else {
            panic!()
        };
        let u = Url::parse(&format!(
            "https://docs.google.com/document/d/{A}/edit?usp=x"
        ))
        .unwrap();
        for o in ORIGINS {
            assert_eq!(decide(&mut w, &m, &u, o), Decision::PassThrough);
        }
    }

    #[test]
    fn other_doc_navigation_from_member_opens_member() {
        let mut w = one_window();
        let h = head(&mut w);
        let Some(Opened::Added(m)) = w.open_from(&h, &doc(A)) else {
            panic!()
        };
        assert!(matches!(
            decide(&mut w, &m, &doc(B), Origin::Navigation),
            Decision::Open(Opened::Added(_))
        ));
    }

    #[test]
    fn folder_navigation_in_head_passes_through() {
        let mut w = one_window();
        let h = head(&mut w);
        let u = Url::parse("https://drive.google.com/drive/folders/other").unwrap();
        assert_eq!(
            decide(&mut w, &h, &u, Origin::Navigation),
            Decision::PassThrough
        );
    }

    #[test]
    fn group_urls_must_be_web_links() {
        assert_eq!(
            parse_group_url("  https://drive.google.com/drive/folders/f1 ")
                .unwrap()
                .as_str(),
            "https://drive.google.com/drive/folders/f1"
        );
        assert!(parse_group_url("").is_err());
        assert_eq!(
            parse_group_url("drive.google.com/x").unwrap_err(),
            "Not a web link: drive.google.com/x"
        );
        assert!(parse_group_url("file:///etc/passwd").is_err());
        assert!(parse_group_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn non_doc_passes_through() {
        let mut w = one_window();
        let h = head(&mut w);
        let u = Url::parse("https://example.com/").unwrap();
        for o in ORIGINS {
            assert_eq!(decide(&mut w, &h, &u, o), Decision::PassThrough);
        }
        assert_eq!(
            decide(&mut w, &"t-99".into(), &doc(A), Origin::Click),
            Decision::PassThrough
        );
    }
}
