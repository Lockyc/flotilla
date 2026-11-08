//! Locating a Drive file: the folders above it, asked of each connected account in turn, so a
//! handed document can be sorted into the group whose folder holds it.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use flotilla_core::identity::valid_file_id;

use super::accounts::Keychain;
use super::http::Transport;
use super::oauth::TOKEN;
use super::{failed, reason, Client};

const FILES: &str = "https://www.googleapis.com/drive/v3/files";

/// Folders walked before giving up; real trees are far shallower.
const MAX_DEPTH: usize = 100;

/// Where a file turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum Located {
    /// The folder ids above the file, nearest first, ending where the walk stopped. `"root"`
    /// closes the list when the walk reached the account's My Drive root.
    Ancestors(Vec<String>),
    /// Accounts answered, and none can see the file.
    NotVisible,
    /// No connected account to ask.
    NoAccount,
    /// No account answered and at least one failed in transit or with a server error.
    Failed(String),
}

/// Asks the connected accounts where a file lives. Access tokens and root ids are cached in
/// memory for the life of the locator and never written anywhere.
pub struct Locator<T: Transport> {
    transport: T,
    keychain: Keychain,
    client: Client,
    tokens: Mutex<HashMap<String, (String, Instant)>>,
    roots: Mutex<HashMap<String, String>>,
}

/// What one account made of the file.
enum Asked {
    Found(Vec<String>),
    NotVisible,
    Revoked,
    Failed(String),
}

impl<T: Transport> Locator<T> {
    pub fn new(transport: T, keychain: Keychain, client: Client) -> Self {
        Locator {
            transport,
            keychain,
            client,
            tokens: Mutex::default(),
            roots: Mutex::default(),
        }
    }

    /// The folders above `file_id` as the first account that can see it reports them, walking
    /// parents upward until `stop` accepts an id or a folder has none. The second value lists
    /// the accounts forgotten because Google revoked their grant.
    pub fn ancestors(&self, file_id: &str, stop: &dyn Fn(&str) -> bool) -> (Located, Vec<String>) {
        let mut dropped = Vec::new();
        let mut asked = 0;
        let mut failure = None;
        for email in self.keychain.list() {
            match self.ask(&email, file_id, stop) {
                Asked::Found(ids) => return (Located::Ancestors(ids), dropped),
                Asked::NotVisible => asked += 1,
                Asked::Revoked => {
                    self.keychain.forget(&email);
                    dropped.push(email);
                }
                Asked::Failed(why) => failure = Some(why),
            }
        }
        let located = match (failure, asked) {
            (Some(why), _) => Located::Failed(why),
            (None, 0) => Located::NoAccount,
            (None, _) => Located::NotVisible,
        };
        (located, dropped)
    }

    fn ask(&self, email: &str, file_id: &str, stop: &dyn Fn(&str) -> bool) -> Asked {
        let token = match self.token(email) {
            Ok(Some(token)) => token,
            Ok(None) => return Asked::Revoked,
            Err(why) => return Asked::Failed(why),
        };
        let mut token = token;
        let mut chain: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut current = file_id.to_owned();
        loop {
            // An id that is not a Drive id is never put in a request path. A parent that fails
            // this ends the walk (the My Drive root id can be shorter than a file id).
            if !valid_file_id(&current) {
                if chain.is_empty() {
                    return Asked::NotVisible;
                }
                break;
            }
            if chain.len() >= MAX_DEPTH || !seen.insert(current.clone()) {
                return Asked::Failed("folder chain too deep".into());
            }
            match self.parent(email, &mut token, &current) {
                Ok(Lookup::Hidden) if chain.is_empty() => return Asked::NotVisible,
                Ok(Lookup::Hidden) => return Asked::Found(chain),
                Ok(Lookup::Parent(Some(parent))) => {
                    chain.push(parent.clone());
                    if stop(&parent) {
                        return Asked::Found(chain);
                    }
                    current = parent;
                }
                Ok(Lookup::Parent(None)) => break,
                Err(why) => return Asked::Failed(why),
            }
        }
        match self.root(email, &mut token) {
            Ok(root) if chain.last() == Some(&root) => chain.push("root".into()),
            Ok(_) => {}
            Err(why) => return Asked::Failed(why),
        }
        Asked::Found(chain)
    }

    /// A valid access token for the account: the cached one until it expires, else a refresh.
    /// `None` when Google reports the grant revoked.
    fn token(&self, email: &str) -> Result<Option<String>, String> {
        if let Some((token, expiry)) = self.tokens.lock().unwrap().get(email) {
            if *expiry > Instant::now() {
                return Ok(Some(token.clone()));
            }
        }
        let Some(refresh) = self.keychain.refresh_token(email) else {
            return Err("couldn't read the account's credentials".into());
        };
        let (status, body) = self.transport.post_form(
            TOKEN,
            &[
                ("client_id", self.client.id),
                ("client_secret", self.client.secret),
                ("refresh_token", &refresh),
                ("grant_type", "refresh_token"),
            ],
        )?;
        let json = json(&body);
        if status == 400 && json["error"] == "invalid_grant" {
            return Ok(None);
        }
        let (200, Some(access)) = (status, json["access_token"].as_str()) else {
            return Err(format!("refreshing a token failed ({status})"));
        };
        let lifetime = json["expires_in"].as_u64().unwrap_or(3600);
        // Expire a minute early so a token never dies mid-walk.
        let expiry = Instant::now() + Duration::from_secs(lifetime.saturating_sub(60));
        self.tokens
            .lock()
            .unwrap()
            .insert(email.to_owned(), (access.to_owned(), expiry));
        Ok(Some(access.to_owned()))
    }

    /// `GET url`; a 401 drops the cached token and retries once with a fresh one, left in
    /// `token`.
    fn get(&self, email: &str, token: &mut String, url: &str) -> Result<(u16, String), String> {
        let first = self.transport.get(url, token)?;
        if first.0 != 401 {
            return Ok(first);
        }
        self.tokens.lock().unwrap().remove(email);
        match self.token(email)? {
            Some(fresh) => *token = fresh,
            None => return Ok(first),
        }
        self.transport.get(url, token)
    }

    /// The first parent of a file or folder; `id` must already be a valid Drive id.
    fn parent(&self, email: &str, token: &mut String, id: &str) -> Result<Lookup, String> {
        let url = format!("{FILES}/{id}?fields=id,parents&supportsAllDrives=true");
        let (status, body) = self.get(email, token, &url)?;
        match status {
            200 => Ok(Lookup::Parent(
                json(&body)["parents"][0].as_str().map(str::to_owned),
            )),
            404 => Ok(Lookup::Hidden),
            403 => match reason(&body).as_deref() {
                Some("insufficientFilePermissions" | "notFound") => Ok(Lookup::Hidden),
                Some("rateLimitExceeded" | "userRateLimitExceeded") => {
                    Err("Drive rate limit reached".into())
                }
                _ => Err(failed("reading a file", status, &body)),
            },
            _ => Err(failed("reading a file", status, &body)),
        }
    }

    /// The account's My Drive root id, fetched once per locator.
    fn root(&self, email: &str, token: &mut String) -> Result<String, String> {
        if let Some(id) = self.roots.lock().unwrap().get(email) {
            return Ok(id.clone());
        }
        let url = format!("{FILES}/root?fields=id");
        let (status, body) = self.get(email, token, &url)?;
        let id = json(&body)["id"].as_str().map(str::to_owned);
        let (200, Some(id)) = (status, id) else {
            return Err(failed("reading the Drive root", status, &body));
        };
        self.roots
            .lock()
            .unwrap()
            .insert(email.to_owned(), id.clone());
        Ok(id)
    }
}

/// One parent lookup's answer.
enum Lookup {
    /// The first parent, or `None` for an item with none.
    Parent(Option<String>),
    /// Unknown to this account (404), or a 403 whose reason is about this file alone. Any other
    /// 403 (the project's Drive API off, the account's scopes, a domain policy, a quota) is a
    /// failure, since it hides every file alike.
    Hidden,
}

/// The body as JSON, or `null` when it isn't.
fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::accounts::Scratch;
    use super::*;
    use std::cell::RefCell;

    const CLIENT: Client = Client {
        id: "id",
        secret: "secret",
    };

    // Real Drive ids are 20+ characters, which is what the locator accepts.
    const D1: &str = "D1aaaaaaaaaaaaaaaaaaaa";
    const P1: &str = "P1aaaaaaaaaaaaaaaaaaaa";
    const P2: &str = "P2aaaaaaaaaaaaaaaaaaaa";

    /// Answers by `(bearer, url)`; the refresh grant answers a token named for the refresh token
    /// (`tok-<refresh>`, then `tok-<refresh>-2` for the second grant), and a bearer listed as
    /// expired answers 401 to everything.
    #[derive(Default)]
    struct Fake {
        gets: HashMap<(String, String), (u16, String)>,
        refused: HashMap<String, (u16, String)>,
        expired: Vec<String>,
        seen: RefCell<Vec<String>>,
        grants: RefCell<Vec<String>>,
    }

    impl Fake {
        fn file(self, refresh: &str, id: &str, status: u16, parents: &[&str]) -> Self {
            self.file_with(refresh, id, status, parents, "{}")
        }

        fn file_with(
            mut self,
            refresh: &str,
            id: &str,
            status: u16,
            parents: &[&str],
            failure: &str,
        ) -> Self {
            let body = if status == 200 {
                let list: Vec<String> = parents.iter().map(|p| format!("\"{p}\"")).collect();
                format!(r#"{{"id":"{id}","parents":[{}]}}"#, list.join(","))
            } else {
                failure.into()
            };
            let url = format!("{FILES}/{id}?fields=id,parents&supportsAllDrives=true");
            for bearer in [format!("tok-{refresh}"), format!("tok-{refresh}-2")] {
                self.gets
                    .insert((bearer, url.clone()), (status, body.clone()));
            }
            self
        }

        fn root(mut self, refresh: &str, id: &str) -> Self {
            let url = format!("{FILES}/root?fields=id");
            for bearer in [format!("tok-{refresh}"), format!("tok-{refresh}-2")] {
                self.gets
                    .insert((bearer, url.clone()), (200, format!(r#"{{"id":"{id}"}}"#)));
            }
            self
        }

        fn refusing(mut self, refresh: &str, status: u16, body: &str) -> Self {
            self.refused.insert(refresh.into(), (status, body.into()));
            self
        }

        fn expiring(mut self, refresh: &str) -> Self {
            self.expired.push(format!("tok-{refresh}"));
            self
        }
    }

    impl Transport for Fake {
        fn get(&self, url: &str, bearer: &str) -> Result<(u16, String), String> {
            self.seen.borrow_mut().push(url.to_owned());
            if self.expired.iter().any(|b| b == bearer) {
                return Ok((401, "{}".into()));
            }
            self.gets
                .get(&(bearer.to_owned(), url.to_owned()))
                .cloned()
                .ok_or_else(|| "connection refused".to_owned())
        }

        fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String), String> {
            assert_eq!(url, TOKEN);
            let refresh = form.iter().find(|(k, _)| *k == "refresh_token").unwrap().1;
            self.grants.borrow_mut().push(refresh.to_owned());
            if let Some(answer) = self.refused.get(refresh) {
                return Ok(answer.clone());
            }
            let nth = self
                .grants
                .borrow()
                .iter()
                .filter(|r| *r == refresh)
                .count();
            let suffix = if nth > 1 {
                format!("-{nth}")
            } else {
                String::new()
            };
            Ok((
                200,
                format!(r#"{{"access_token":"tok-{refresh}{suffix}","expires_in":3600}}"#),
            ))
        }
    }

    fn tree(refresh: &str) -> Fake {
        Fake::default()
            .file(refresh, D1, 200, &[P1])
            .file(refresh, P1, 200, &[P2])
            .file(refresh, P2, 200, &[])
            .root(refresh, P2)
    }

    fn locator(fake: Fake, scratch: &Scratch) -> Locator<Fake> {
        Locator::new(
            fake,
            Keychain {
                service: scratch.0.service,
            },
            CLIENT,
        )
    }

    fn ids(located: Located) -> Vec<String> {
        match located {
            Located::Ancestors(ids) => ids,
            other => panic!("expected ancestors, got {other:?}"),
        }
    }

    #[test]
    fn walks_to_the_top() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_walks_to_the_top");
        scratch.0.save("a@x", "ra").unwrap();
        let (located, dropped) = locator(tree("ra"), &scratch).ancestors(D1, &|_| false);
        assert_eq!(ids(located), [P1, P2, "root"]);
        assert!(dropped.is_empty());
    }

    #[test]
    fn stops_at_a_head() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_stops_at_a_head");
        scratch.0.save("a@x", "ra").unwrap();
        let loc = locator(tree("ra"), &scratch);
        let (located, _) = loc.ancestors(D1, &|id| id == P1);
        assert_eq!(ids(located), [P1]);
        assert!(!loc
            .transport
            .seen
            .borrow()
            .iter()
            .any(|u| u.contains(&format!("/{P2}?"))));
    }

    #[test]
    fn tries_the_next_account() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_tries_the_next_account");
        scratch.0.save("a@x", "ra").unwrap();
        scratch.0.save("b@x", "rb").unwrap();
        let fake = tree("rb").file("ra", D1, 404, &[]);
        let (located, _) = locator(fake, &scratch).ancestors(D1, &|_| false);
        assert_eq!(ids(located), [P1, P2, "root"]);
    }

    /// Google's 403 body naming `reason`.
    fn refused(reason: &str) -> String {
        format!(r#"{{"error":{{"code":403,"message":"m","errors":[{{"reason":"{reason}"}}]}}}}"#)
    }

    /// Unknown to the account (404), or refused for this file alone.
    #[test]
    fn nobody_can_see_it() {
        for (n, (status, body)) in [
            (404, "{}".to_string()),
            (403, refused("insufficientFilePermissions")),
            (403, refused("notFound")),
        ]
        .into_iter()
        .enumerate()
        {
            let scratch = Scratch::new(
                [
                    "au.lsjc.flotilla.test.drive_nobody_can_see_it_0",
                    "au.lsjc.flotilla.test.drive_nobody_can_see_it_1",
                    "au.lsjc.flotilla.test.drive_nobody_can_see_it_2",
                ][n],
            );
            scratch.0.save("a@x", "ra").unwrap();
            let fake = Fake::default().file_with("ra", D1, status, &[], &body);
            let (located, _) = locator(fake, &scratch).ancestors(D1, &|_| false);
            assert_eq!(located, Located::NotVisible, "{status} {body}");
        }
    }

    /// A 403 that is about the project or the account, not the file, fails with Google's words.
    #[test]
    fn any_other_refusal_is_a_failure_with_googles_reason() {
        let body = r#"{"error":{"code":403,"message":"Google Drive API has not been used in project 1 before or it is disabled.","errors":[{"reason":"accessNotConfigured"}]}}"#;
        let fake = Fake::default().file_with("ra", D1, 403, &[], body);
        let (located, _) = locate("au.lsjc.flotilla.test.drive_not_configured", fake, D1);
        assert_eq!(
            located,
            Located::Failed(
                "reading a file failed (403 accessNotConfigured: Google Drive API has not been \
                 used in project 1 before or it is disabled.)"
                    .into()
            )
        );
        let fake = Fake::default().file_with("ra", D1, 403, &[], "{}");
        let (located, _) = locate("au.lsjc.flotilla.test.drive_bare_403", fake, D1);
        assert_eq!(
            located,
            Located::Failed("reading a file failed (403)".into())
        );
    }

    #[test]
    fn no_accounts() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_no_accounts");
        let (located, dropped) = locator(Fake::default(), &scratch).ancestors(D1, &|_| false);
        assert_eq!(located, Located::NoAccount);
        assert!(dropped.is_empty());
    }

    #[test]
    fn revoked_account_is_forgotten() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_revoked_account");
        scratch.0.save("a@x", "ra").unwrap();
        scratch.0.save("b@x", "rb").unwrap();
        let fake = tree("rb").refusing("ra", 400, r#"{"error":"invalid_grant"}"#);
        let (located, dropped) = locator(fake, &scratch).ancestors(D1, &|_| false);
        assert_eq!(ids(located), [P1, P2, "root"]);
        assert_eq!(dropped, ["a@x"]);
        assert_eq!(scratch.0.list(), ["b@x"]);
    }

    #[test]
    fn a_failing_account_is_reported_when_none_answers() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_failing_account");
        scratch.0.save("a@x", "ra").unwrap();
        // The file is unknown to the fake, so the call fails in transit.
        let (located, dropped) = locator(Fake::default(), &scratch).ancestors(D1, &|_| false);
        assert_eq!(located, Located::Failed("connection refused".into()));
        assert!(dropped.is_empty());
        assert_eq!(scratch.0.list(), ["a@x"]);
    }

    #[test]
    fn tokens_and_root_are_fetched_once() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_cache");
        scratch.0.save("a@x", "ra").unwrap();
        let loc = locator(tree("ra"), &scratch);
        loc.ancestors(D1, &|_| false);
        loc.ancestors(D1, &|_| false);
        let roots = loc
            .transport
            .seen
            .borrow()
            .iter()
            .filter(|u| u.contains("/root?"))
            .count();
        assert_eq!(roots, 1);
        assert_eq!(*loc.transport.grants.borrow(), ["ra"]);
    }

    /// One account `ra`, a locator over `fake`, and what it says of `id`.
    fn locate(test: &'static str, fake: Fake, id: &str) -> (Located, Locator<Fake>) {
        let scratch = Scratch::new(test);
        scratch.0.save("a@x", "ra").unwrap();
        let loc = locator(fake, &scratch);
        let (located, _) = loc.ancestors(id, &|_| false);
        (located, loc)
    }

    #[test]
    fn an_id_with_a_slash_is_never_requested() {
        let (located, loc) = locate(
            "au.lsjc.flotilla.test.drive_slash_id",
            tree("ra"),
            "x/../about",
        );
        assert_eq!(located, Located::NotVisible);
        assert!(loc.transport.seen.borrow().is_empty());
    }

    #[test]
    fn an_invalid_parent_ends_the_walk() {
        let fake = Fake::default()
            .file("ra", D1, 200, &["../about"])
            .root("ra", P2);
        let (located, loc) = locate("au.lsjc.flotilla.test.drive_bad_parent", fake, D1);
        assert_eq!(ids(located), ["../about"]);
        assert!(!loc
            .transport
            .seen
            .borrow()
            .iter()
            .any(|u| u.contains("about")));
    }

    #[test]
    fn a_folder_loop_fails() {
        let fake = Fake::default()
            .file("ra", D1, 200, &[P1])
            .file("ra", P1, 200, &[P2])
            .file("ra", P2, 200, &[P1]);
        let (located, _) = locate("au.lsjc.flotilla.test.drive_loop", fake, D1);
        assert_eq!(located, Located::Failed("folder chain too deep".into()));
    }

    #[test]
    fn a_hidden_parent_ends_the_walk_with_what_was_seen() {
        let fake = Fake::default()
            .file("ra", D1, 200, &[P1])
            .file("ra", P1, 404, &[]);
        let (located, loc) = locate("au.lsjc.flotilla.test.drive_hidden_parent", fake, D1);
        assert_eq!(ids(located), [P1]);
        assert!(!loc
            .transport
            .seen
            .borrow()
            .iter()
            .any(|u| u.contains("/root?")));
    }

    #[test]
    fn a_rate_limit_is_a_failure_not_a_hidden_file() {
        let limited = r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#;
        let fake = Fake::default().file_with("ra", D1, 403, &[], limited);
        let (located, _) = locate("au.lsjc.flotilla.test.drive_rate_limit", fake, D1);
        assert!(matches!(located, Located::Failed(_)), "{located:?}");
    }

    #[test]
    fn a_rejected_token_is_refreshed_once() {
        let fake = tree("ra").expiring("ra");
        let (located, loc) = locate("au.lsjc.flotilla.test.drive_401", fake, D1);
        assert_eq!(ids(located), [P1, P2, "root"]);
        assert_eq!(*loc.transport.grants.borrow(), ["ra", "ra"]);
    }

    #[test]
    fn a_file_with_no_parents_has_no_ancestors() {
        let fake = Fake::default().file("ra", D1, 200, &[]).root("ra", P2);
        let (located, _) = locate("au.lsjc.flotilla.test.drive_orphan", fake, D1);
        assert_eq!(ids(located), Vec::<String>::new());
    }

    #[test]
    fn a_shared_drive_top_is_not_the_root() {
        let fake = tree("ra").root("ra", "0Aother");
        let (located, _) = locate("au.lsjc.flotilla.test.drive_shared_top", fake, D1);
        assert_eq!(ids(located), [P1, P2]);
    }
}
