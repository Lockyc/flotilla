//! What each link or navigation does. The shell calls `decide` from shell-core's
//! `ContentHooks.intercept`; a document other than the tab's own opens as a member, everything
//! else passes through to shell-core's routing.

use crate::identity::doc_key;
use crate::model::{Opened, TabId, Workspace};
use url::Url;

/// How the shell was asked to go to the URL; mirrors shell-core's `LinkOrigin` so core stays
/// tauri-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    NewWindow,
    Click,
    Navigation,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Open(Opened),
    PassThrough,
}

pub fn decide(ws: &mut Workspace, from: &TabId, url: &Url, _origin: Origin) -> Decision {
    let Some((group, tab)) = ws.find(from) else {
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

    const ORIGINS: [Origin; 3] = [Origin::NewWindow, Origin::Click, Origin::Navigation];

    fn doc(id: &str) -> Url {
        Url::parse(&format!("https://docs.google.com/document/d/{id}/edit")).unwrap()
    }
    const A: &str = "1AbCdEfGhIjKlMnOpQrStA1";
    const B: &str = "1AbCdEfGhIjKlMnOpQrStB2";

    fn head(w: &mut Workspace) -> TabId {
        let u = Url::parse("https://drive.google.com/drive/folders/fold1").unwrap();
        match w.new_group(&u, "default") {
            Opened::Added(i) | Opened::Raised(i) => i,
        }
    }

    #[test]
    fn doc_link_from_head_opens_member() {
        for o in ORIGINS {
            let mut w = Workspace::default();
            let h = head(&mut w);
            assert!(matches!(
                decide(&mut w, &h, &doc(A), o),
                Decision::Open(Opened::Added(_))
            ));
        }
    }

    #[test]
    fn same_doc_redirect_passes_through() {
        let mut w = Workspace::default();
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
        let mut w = Workspace::default();
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
        let mut w = Workspace::default();
        let h = head(&mut w);
        let u = Url::parse("https://drive.google.com/drive/folders/other").unwrap();
        assert_eq!(
            decide(&mut w, &h, &u, Origin::Navigation),
            Decision::PassThrough
        );
    }

    #[test]
    fn non_doc_passes_through() {
        let mut w = Workspace::default();
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
