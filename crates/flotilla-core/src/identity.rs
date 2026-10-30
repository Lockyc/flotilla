//! Document identity: which open document a URL is, so a document is open at most once.

use serde::{Deserialize, Serialize};
use url::Url;

/// A document, scoped to the store (profile) it is open in.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocKey {
    pub store: String,
    pub provider: String,
    pub id: String,
}

/// Recognises one provider's document URLs.
pub trait Recogniser {
    fn name(&self) -> &'static str;
    /// The provider's stable id for the document `url` opens, or `None` when it is not a document.
    fn document_id(&self, url: &Url) -> Option<String>;
}

/// Google Drive files: Docs, Sheets, Slides and Forms editors and `drive.google.com/file/d/…`.
///
/// Ports `fileID`/`validFileID` of Manicule's `GoogleLink.swift`, its Swift counterpart; flotilla's
/// core is Rust and cannot link a Swift package, so the rule exists in both. Unlike Manicule,
/// folders and `open?id=` are not documents here, and neither are published pages (`/d/e/…`) or
/// embed shapes, which arrive as iframe navigations inside a document.
pub struct Google;

impl Recogniser for Google {
    fn name(&self) -> &'static str {
        "google"
    }

    fn document_id(&self, url: &Url) -> Option<String> {
        if !matches!(url.host_str(), Some("docs.google.com" | "drive.google.com")) {
            return None;
        }
        let segments: Vec<&str> = url.path_segments()?.collect();
        let at = segments.iter().position(|s| {
            matches!(
                *s,
                "document" | "spreadsheets" | "presentation" | "forms" | "file"
            )
        })?;
        // `/<kind>[/u/N]/d/<id>`
        let mut rest = &segments[at + 1..];
        if let ["u", _, tail @ ..] = rest {
            rest = tail;
        }
        let ["d", id, after @ ..] = rest else {
            return None;
        };
        if *id == "e"
            || matches!(
                after.first(),
                Some(&("preview" | "pubhtml" | "htmlembed" | "embed" | "htmlview"))
            )
        {
            return None;
        }
        valid_file_id(id).then(|| id.to_string())
    }
}

/// Drive file ids are long runs of ASCII letters, digits, `-` and `_`.
fn valid_file_id(id: &str) -> bool {
    id.len() >= 20
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The `DocKey` provider the model gives non-document heads; no recogniser may take this name.
pub const HEAD_PROVIDER: &str = "head";

const RECOGNISERS: &[&dyn Recogniser] = &[&Google];

/// The key of the document `url` opens in `store`, from the first recogniser that knows it.
pub fn doc_key(store: &str, url: &Url) -> Option<DocKey> {
    RECOGNISERS.iter().find_map(|r| {
        r.document_id(url).map(|id| DocKey {
            store: store.to_string(),
            provider: r.name().to_string(),
            id,
        })
    })
}

const NOISE_PARAMS: &[&str] = &["usp", "pli", "authuser", "ouid", "resourcekey"];

/// A URL normalised for comparing heads: lowercase host without `www.`, no fragment, no trailing
/// `/`, and none of the sharing/account query params.
pub fn head_key(url: &Url) -> String {
    let host = url.host_str().unwrap_or("").to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    let path = url.path().trim_end_matches('/');
    let query: Vec<&str> = url
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty() && !NOISE_PARAMS.contains(&p.split('=').next().unwrap_or("")))
        .collect();
    let query = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", query.join("&"))
    };
    format!("{}://{host}{port}{path}{query}", url.scheme())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "1AbCdEfGhIjKlMnOpQrStUv";

    fn key(store: &str, s: &str) -> Option<DocKey> {
        doc_key(store, &Url::parse(s).unwrap())
    }

    fn id_of(s: &str) -> Option<String> {
        key("default", s).map(|k| k.id)
    }

    #[test]
    fn document_shapes_key_to_the_file_id() {
        for u in [
            format!("https://docs.google.com/document/d/{ID}/edit"),
            format!("https://docs.google.com/document/d/{ID}/edit?usp=sharing"),
            format!("https://docs.google.com/document/d/{ID}/edit#heading=h.x"),
            format!("https://docs.google.com/spreadsheets/d/{ID}/edit#gid=0"),
            format!("https://docs.google.com/document/u/1/d/{ID}/view"),
            format!("https://docs.google.com/u/1/document/d/{ID}/edit"),
            format!("https://docs.google.com/presentation/d/{ID}/edit"),
            format!("https://docs.google.com/forms/d/{ID}/edit"),
            format!("https://drive.google.com/file/d/{ID}/view"),
        ] {
            let k = key("default", &u).unwrap_or_else(|| panic!("no key for {u}"));
            assert_eq!(k.id, ID, "{u}");
            assert_eq!(k.provider, "google");
            assert_eq!(k.store, "default");
        }
    }

    #[test]
    fn non_document_shapes_have_no_key() {
        for u in [
            format!("https://drive.google.com/drive/folders/{ID}"),
            format!("https://drive.google.com/drive/u/0/folders/{ID}"),
            format!("https://drive.google.com/open?id={ID}"),
            format!("https://docs.google.com/open?id={ID}"),
            "https://docs.google.com/forms/d/e/1FAIpQLSeAbCdEfGhIjKlMnOpQrStUvWxYz/viewform".into(),
            "https://docs.google.com/document/d/e/2PACX-1vAbCdEfGhIjKlMnOpQrStUvWxYz/pub".into(),
            format!("https://drive.google.com/file/d/{ID}/preview"),
            format!("https://docs.google.com/document/d/{ID}/preview"),
            format!("https://docs.google.com/spreadsheets/d/{ID}/pubhtml"),
            format!("https://docs.google.com/document/d/{ID}/htmlembed"),
            format!("https://docs.google.com/document/d/{ID}/embed"),
            format!("https://docs.google.com/presentation/d/{ID}/htmlview"),
            "https://docs.google.com/document/d/ABC123/edit".into(),
            "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQ/edit".into(),
            "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrS!Uv/edit".into(),
            format!("https://example.com/document/d/{ID}"),
            format!("https://www.google.com/document/d/{ID}"),
            format!("https://notdocs.google.com/document/d/{ID}/edit"),
            format!("https://docs.google.com/maps/d/{ID}/edit"),
        ] {
            assert_eq!(key("default", &u), None, "{u}");
        }
        assert_eq!(id_of("https://docs.google.com/open?id=O1"), None);
    }

    #[test]
    fn keys_are_scoped_by_store() {
        let u = format!("https://docs.google.com/document/d/{ID}/edit");
        assert_ne!(key("default", &u), key("work", &u));
        assert_eq!(key("work", &u), key("work", &u));
    }

    #[test]
    fn dockey_round_trips_through_serde() {
        let k = key(
            "default",
            &format!("https://docs.google.com/document/d/{ID}/edit"),
        )
        .unwrap();
        let back: DocKey = serde_json::from_str(&serde_json::to_string(&k).unwrap()).unwrap();
        assert_eq!(k, back);
    }

    fn head(s: &str) -> String {
        head_key(&Url::parse(s).unwrap())
    }

    #[test]
    fn no_recogniser_takes_the_head_provider_name() {
        assert!(RECOGNISERS.iter().all(|r| r.name() != HEAD_PROVIDER));
    }

    #[test]
    fn head_key_ignores_noise() {
        assert_eq!(
            head("https://drive.google.com/drive/folders/X?usp=sharing"),
            head("https://drive.google.com/drive/folders/X/")
        );
        assert_eq!(
            head("https://WWW.Example.com/a/b/?x=1&usp=sharing&pli=1&authuser=2&ouid=3&resourcekey=k#frag"),
            "https://example.com/a/b?x=1"
        );
        assert_ne!(
            head("https://example.com/a?x=1"),
            head("https://example.com/a?x=2")
        );
        assert_eq!(head("https://example.com/"), "https://example.com");
    }
}
