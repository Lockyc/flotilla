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
/// folders and `open?id=` are not documents here, and only a view-shaped tail after the id (none,
/// `edit`, `view`, `viewform`) is: published pages (`/d/e/…`) and embed shapes arrive as iframe
/// navigations inside a document, and `export`, `copy` and the like are actions on it.
pub struct Google;

impl Recogniser for Google {
    fn name(&self) -> &'static str {
        "google"
    }

    fn document_id(&self, url: &Url) -> Option<String> {
        if !google_host(url) {
            return None;
        }
        let segments: Vec<&str> = url.path_segments()?.collect();
        let at = segments.iter().position(|s| editor(s).is_some())?;
        // `/<kind>[/u/N]/d/<id>`
        let mut rest = &segments[at + 1..];
        if let ["u", _, tail @ ..] = rest {
            rest = tail;
        }
        let ["d", id, after @ ..] = rest else {
            return None;
        };
        let viewed = match after {
            [] | [""] => true,
            [tail] | [tail, ""] => matches!(*tail, "edit" | "view" | "viewform"),
            _ => false,
        };
        (viewed && valid_file_id(id)).then(|| id.to_string())
    }
}

fn google_host(url: &Url) -> bool {
    matches!(url.host_str(), Some("docs.google.com" | "drive.google.com"))
}

/// What a Google URL shows, which picks its sidebar glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Doc,
    Sheet,
    Slides,
    Form,
    Folder,
    /// Any other Drive file, or a share link that does not say which it is.
    Drive,
}

/// The kind a document path's editor segment (`/<editor>[/u/N]/d/<id>`) names.
fn editor(segment: &str) -> Option<Kind> {
    Some(match segment {
        "document" => Kind::Doc,
        "spreadsheets" => Kind::Sheet,
        "presentation" => Kind::Slides,
        "forms" => Kind::Form,
        "file" => Kind::Drive,
        _ => return None,
    })
}

/// The kind of Google page `url` is: a document's editor, a Drive view (`/drive/…`) as a folder,
/// an `open?id=` link as a Drive file; `None` off Google or on any other Google page.
pub fn kind(url: &Url) -> Option<Kind> {
    if !google_host(url) {
        return None;
    }
    if let Some(k) = url.path_segments()?.find_map(editor) {
        return Some(k);
    }
    match url.path() {
        "/open" => Some(Kind::Drive),
        p if url.host_str() == Some("drive.google.com") && p.starts_with("/drive") => {
            Some(Kind::Folder)
        }
        _ => None,
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

/// The key `url` has as a group head: [`doc_key`], or for a Drive `open?id=<id>` share link the
/// file `<id>` names. Only the link's redirect reveals whether that is a document or a folder, so
/// the head is keyed by the id either way: a document's redirect then keys as the head itself
/// (no member), and a folder's is no document at all. Members still need [`doc_key`].
pub fn head_doc_key(store: &str, url: &Url) -> Option<DocKey> {
    doc_key(store, url).or_else(|| {
        if !google_host(url) || url.path() != "/open" {
            return None;
        }
        let (_, id) = url.query_pairs().find(|(k, _)| k == "id")?;
        valid_file_id(&id).then(|| DocKey {
            store: store.to_string(),
            provider: Google.name().to_string(),
            id: id.into_owned(),
        })
    })
}

/// A tab's identity within its group's login `store`: the document it opens (`head_doc_key`, which
/// for a member is its `doc_key`), else (a head that is not a document) its `head_key`. The one
/// rule open-once compares by — in the workspace and across the config's windows — so a document
/// is the same identity whether it heads a group or is a member, and the same URL in two stores
/// is two.
pub fn identity(store: &str, url: &Url) -> DocKey {
    head_doc_key(store, url).unwrap_or_else(|| DocKey {
        store: store.to_string(),
        provider: HEAD_PROVIDER.to_string(),
        id: head_key(url),
    })
}

/// 64-bit FNV-1a. Spelled out rather than `DefaultHasher`, whose output may change between Rust
/// releases: tab ids built from it persist in the session file.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A tab's id, `"t-<16 hex>"`: the [`fnv1a`] of its key's store, provider and id, NUL-separated.
pub fn tab_id(key: &DocKey) -> String {
    let bytes = [key.store.as_str(), &key.provider, &key.id].join("\0");
    format!("t-{:016x}", fnv1a(bytes.as_bytes()))
}

/// Whether `s` has a [`tab_id`]'s shape.
pub fn is_tab_id(s: &str) -> bool {
    s.strip_prefix("t-")
        .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

const NOISE_PARAMS: &[&str] = &["usp", "pli", "authuser", "ouid", "resourcekey"];

/// What a Google page adds to its own address once loaded or signed in: the open document tab
/// (`?tab=t.0`) and the account segment (`/u/<n>`).
const GOOGLE_NOISE_PARAMS: &[&str] = &["tab"];

/// The app name a Google page appends to its title.
const GOOGLE_APP_TITLES: &[&str] = &[
    " - Google Drive",
    " - Google Docs",
    " - Google Sheets",
    " - Google Slides",
    " - Google Forms",
];

/// A page's reported title as a name: without the Google app name it ends with ("Budget - Google
/// Sheets" is "Budget"). A title that is only an app name is kept.
pub fn page_title(reported: &str) -> &str {
    GOOGLE_APP_TITLES
        .iter()
        .find_map(|s| reported.strip_suffix(s))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(reported)
}

/// The address to keep as a home for the page at `url`: without its fragment, and on a Google
/// page without the document tab it adds on load (`?tab=…`). Everything else is the page's own
/// address, kept as the user saw it.
pub fn home_url(url: &Url) -> Url {
    let mut home = url.clone();
    home.set_fragment(None);
    if google_host(url) {
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| !GOOGLE_NOISE_PARAMS.contains(&k.as_ref()))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if kept.is_empty() {
            home.set_query(None);
        } else if kept.len() != url.query_pairs().count() {
            home.query_pairs_mut().clear().extend_pairs(kept);
        }
    }
    home
}

/// A URL normalised for comparing pages: lowercase host without `www.`, no fragment, no trailing
/// `/`, none of the sharing/account query params, and on a Google page none of what it adds to
/// its address on load. Two addresses with one key are the same page: one head, and a tab at its
/// home.
pub fn head_key(url: &Url) -> String {
    let host = url.host_str().unwrap_or("").to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let google = google_host(url);
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    let mut segments: Vec<&str> = url.path().split('/').collect();
    if google {
        // The account segment only when an id follows (`/folders/<id>`, `/d/<id>`): an id names
        // the same file for every account, but `/drive/u/1/my-drive` is that account's own page.
        let account = segments
            .windows(2)
            .position(|w| w[0] == "u" && w[1].parse::<u32>().is_ok())
            .filter(|at| {
                segments[at + 2..]
                    .iter()
                    .any(|s| matches!(*s, "folders" | "d"))
            });
        if let Some(at) = account {
            segments.drain(at..at + 2);
        }
    }
    let path = segments.join("/");
    let path = path.trim_end_matches('/');
    let noise = |k: &str| NOISE_PARAMS.contains(&k) || (google && GOOGLE_NOISE_PARAMS.contains(&k));
    let query: Vec<&str> = url
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty() && !noise(p.split('=').next().unwrap_or("")))
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
            format!("https://docs.google.com/forms/d/{ID}/viewform"),
            format!("https://docs.google.com/document/d/{ID}"),
            format!("https://docs.google.com/document/d/{ID}/"),
            format!("https://docs.google.com/document/d/{ID}/edit/"),
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
            // Downloads and copies are actions on the document, not the document.
            format!("https://docs.google.com/document/d/{ID}/export?format=pdf"),
            format!("https://docs.google.com/spreadsheets/d/{ID}/export?format=xlsx"),
            format!("https://docs.google.com/document/d/{ID}/copy"),
            format!("https://docs.google.com/presentation/d/{ID}/copy?copyComments=true"),
            format!("https://docs.google.com/document/u/0/d/{ID}/export?format=docx"),
            format!("https://docs.google.com/document/d/{ID}/mobilebasic"),
            format!("https://docs.google.com/document/d/{ID}/edit/extra"),
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
    fn an_open_id_share_link_heads_as_its_file() {
        let head = |s: &str| head_doc_key("default", &Url::parse(s).unwrap()).map(|k| k.id);
        for u in [
            format!("https://drive.google.com/open?id={ID}"),
            format!("https://drive.google.com/open?usp=sharing&id={ID}"),
            format!("https://docs.google.com/open?id={ID}"),
            format!("https://docs.google.com/document/d/{ID}/edit"),
        ] {
            assert_eq!(head(&u).as_deref(), Some(ID), "{u}");
        }
        for u in [
            "https://drive.google.com/open?id=short".to_string(),
            "https://drive.google.com/open".into(),
            format!("https://drive.google.com/drive/folders/{ID}"),
            format!("https://example.com/open?id={ID}"),
        ] {
            assert_eq!(head(&u), None, "{u}");
        }
    }

    #[test]
    fn tab_ids_are_the_fnv1a_of_the_key() {
        assert_eq!(fnv1a(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a(b"a"), 0xaf63dc4c8601ec8c);
        let k = DocKey {
            store: "default".into(),
            provider: "google".into(),
            id: ID.into(),
        };
        let id = tab_id(&k);
        assert_eq!(
            id,
            format!(
                "t-{:016x}",
                fnv1a(format!("default\0google\0{ID}").as_bytes())
            )
        );
        assert_eq!(id.len(), 18);
        assert!(is_tab_id(&id));
        assert!(!is_tab_id("t-3") && !is_tab_id("-interim") && !is_tab_id("t-0123456789abcdeg"));
        let other = DocKey {
            store: "work".into(),
            ..k
        };
        assert_ne!(tab_id(&other), id);
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

    #[test]
    fn a_page_title_drops_the_google_app_name() {
        for (reported, name) in [
            ("Rotary (interim) - Google Drive", "Rotary (interim)"),
            (
                "Rotary Example – Budget - Google Sheets",
                "Rotary Example – Budget",
            ),
            ("Minutes - Google Docs", "Minutes"),
            ("2026-11-02 Bulletin - Google Slides", "2026-11-02 Bulletin"),
            ("RSVP - Google Forms", "RSVP"),
            ("Example Domain", "Example Domain"),
            ("Google Drive", "Google Drive"),
        ] {
            assert_eq!(page_title(reported), name);
        }
    }

    #[test]
    fn a_home_is_the_page_without_what_it_adds_on_load() {
        let home = |s: &str| home_url(&Url::parse(s).unwrap()).to_string();
        let doc = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit";
        assert_eq!(home(&format!("{doc}?tab=t.0#heading=h.x")), doc);
        assert_eq!(
            home("https://drive.google.com/drive/u/0/folders/X?usp=sharing"),
            "https://drive.google.com/drive/u/0/folders/X?usp=sharing"
        );
        // Off Google, only the fragment goes.
        assert_eq!(
            home("https://example.com/a?tab=2#top"),
            "https://example.com/a?tab=2"
        );
    }

    #[test]
    fn head_key_ignores_what_google_adds_on_load() {
        const DOC: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit";
        assert_eq!(head(&format!("{DOC}?tab=t.0#heading=h.x")), head(DOC));
        assert_eq!(
            head("https://drive.google.com/drive/u/0/folders/X"),
            head("https://drive.google.com/drive/folders/X")
        );
        // Another view of the document is another page.
        assert_ne!(head(&DOC.replace("/edit", "/view")), head(DOC));
        // An account segment with no id after it names that account's own page.
        assert_ne!(
            head("https://drive.google.com/drive/u/0/my-drive"),
            head("https://drive.google.com/drive/u/1/my-drive")
        );
        assert_eq!(
            head(&DOC.replace("/document/", "/document/u/1/")),
            head(DOC)
        );
        // Off Google, `tab` and `/u/<n>` are the page's own.
        assert_ne!(
            head("https://example.com/a?tab=2"),
            head("https://example.com/a")
        );
        assert_ne!(
            head("https://example.com/u/0/a"),
            head("https://example.com/a")
        );
    }

    fn kind_of(s: &str) -> Option<Kind> {
        kind(&Url::parse(s).unwrap())
    }

    #[test]
    fn kind_names_what_a_google_url_shows() {
        for (u, k) in [
            (
                format!("https://docs.google.com/document/d/{ID}/edit"),
                Kind::Doc,
            ),
            (
                format!("https://docs.google.com/u/1/document/d/{ID}/edit"),
                Kind::Doc,
            ),
            (
                format!("https://docs.google.com/spreadsheets/d/{ID}/edit#gid=0"),
                Kind::Sheet,
            ),
            (
                format!("https://docs.google.com/presentation/d/{ID}/edit"),
                Kind::Slides,
            ),
            (
                format!("https://docs.google.com/forms/d/{ID}/viewform"),
                Kind::Form,
            ),
            (
                format!("https://drive.google.com/drive/folders/{ID}"),
                Kind::Folder,
            ),
            (
                format!("https://drive.google.com/drive/u/0/folders/{ID}"),
                Kind::Folder,
            ),
            (
                "https://drive.google.com/drive/my-drive".into(),
                Kind::Folder,
            ),
            (
                format!("https://drive.google.com/file/d/{ID}/view"),
                Kind::Drive,
            ),
            (
                format!("https://drive.google.com/open?id={ID}"),
                Kind::Drive,
            ),
        ] {
            assert_eq!(kind_of(&u), Some(k), "{u}");
        }
        for u in [
            "https://example.com/document/d/x",
            "https://docs.google.com/",
            "https://www.google.com/",
        ] {
            assert_eq!(kind_of(u), None, "{u}");
        }
    }
}
