//! Locating a Drive file: the folders above it, asked of each connected account in turn, so a
//! handed document can be sorted into the group whose folder holds it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::accounts::Keychain;
use super::http::Transport;
use super::oauth::TOKEN;
use super::Client;

const FILES: &str = "https://www.googleapis.com/drive/v3/files";

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
        let mut chain: Vec<String> = Vec::new();
        let mut current = file_id.to_owned();
        loop {
            match self.parent(&token, &current) {
                Ok(Lookup::Hidden) => return Asked::NotVisible,
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
        match self.root(email, &token) {
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
            return Ok(None);
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

    /// The first parent of a file or folder.
    fn parent(&self, token: &str, id: &str) -> Result<Lookup, String> {
        let url = format!("{FILES}/{id}?fields=id,parents&supportsAllDrives=true");
        let (status, body) = self.transport.get(&url, token)?;
        match status {
            200 => Ok(Lookup::Parent(
                json(&body)["parents"][0].as_str().map(str::to_owned),
            )),
            403 | 404 => Ok(Lookup::Hidden),
            _ => Err(format!("reading a file failed ({status})")),
        }
    }

    /// The account's My Drive root id, fetched once per locator.
    fn root(&self, email: &str, token: &str) -> Result<String, String> {
        if let Some(id) = self.roots.lock().unwrap().get(email) {
            return Ok(id.clone());
        }
        let url = format!("{FILES}/root?fields=id");
        let (status, body) = self.transport.get(&url, token)?;
        let id = json(&body)["id"].as_str().map(str::to_owned);
        let (200, Some(id)) = (status, id) else {
            return Err(format!("reading the Drive root failed ({status})"));
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
    /// Refused or unknown to this account.
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

    /// Answers by `(bearer, url)`; the refresh grant answers a token named for the refresh token.
    #[derive(Default)]
    struct Fake {
        gets: HashMap<(String, String), (u16, String)>,
        refused: HashMap<String, (u16, String)>,
        seen: RefCell<Vec<String>>,
    }

    impl Fake {
        fn file(mut self, refresh: &str, id: &str, status: u16, parents: &[&str]) -> Self {
            let body = if status == 200 {
                let list: Vec<String> = parents.iter().map(|p| format!("\"{p}\"")).collect();
                format!(r#"{{"id":"{id}","parents":[{}]}}"#, list.join(","))
            } else {
                "{}".into()
            };
            let url = format!("{FILES}/{id}?fields=id,parents&supportsAllDrives=true");
            self.gets
                .insert((format!("tok-{refresh}"), url), (status, body));
            self
        }

        fn root(mut self, refresh: &str, id: &str) -> Self {
            let url = format!("{FILES}/root?fields=id");
            self.gets.insert(
                (format!("tok-{refresh}"), url),
                (200, format!(r#"{{"id":"{id}"}}"#)),
            );
            self
        }

        fn refusing(mut self, refresh: &str, status: u16, body: &str) -> Self {
            self.refused.insert(refresh.into(), (status, body.into()));
            self
        }
    }

    impl Transport for Fake {
        fn get(&self, url: &str, bearer: &str) -> Result<(u16, String), String> {
            self.seen.borrow_mut().push(url.to_owned());
            self.gets
                .get(&(bearer.to_owned(), url.to_owned()))
                .cloned()
                .ok_or_else(|| "connection refused".to_owned())
        }

        fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String), String> {
            assert_eq!(url, TOKEN);
            let refresh = form.iter().find(|(k, _)| *k == "refresh_token").unwrap().1;
            if let Some(answer) = self.refused.get(refresh) {
                return Ok(answer.clone());
            }
            Ok((
                200,
                format!(r#"{{"access_token":"tok-{refresh}","expires_in":3600}}"#),
            ))
        }
    }

    fn tree(refresh: &str) -> Fake {
        Fake::default()
            .file(refresh, "D1", 200, &["P1"])
            .file(refresh, "P1", 200, &["P2"])
            .file(refresh, "P2", 200, &[])
            .root(refresh, "P2")
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
        let (located, dropped) = locator(tree("ra"), &scratch).ancestors("D1", &|_| false);
        assert_eq!(ids(located), ["P1", "P2", "root"]);
        assert!(dropped.is_empty());
    }

    #[test]
    fn stops_at_a_head() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_stops_at_a_head");
        scratch.0.save("a@x", "ra").unwrap();
        let loc = locator(tree("ra"), &scratch);
        let (located, _) = loc.ancestors("D1", &|id| id == "P1");
        assert_eq!(ids(located), ["P1"]);
        assert!(!loc
            .transport
            .seen
            .borrow()
            .iter()
            .any(|u| u.contains("/P2?")));
    }

    #[test]
    fn tries_the_next_account() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_tries_the_next_account");
        scratch.0.save("a@x", "ra").unwrap();
        scratch.0.save("b@x", "rb").unwrap();
        let fake = tree("rb").file("ra", "D1", 404, &[]);
        let (located, _) = locator(fake, &scratch).ancestors("D1", &|_| false);
        assert_eq!(ids(located), ["P1", "P2", "root"]);
    }

    #[test]
    fn nobody_can_see_it() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_nobody_can_see_it");
        scratch.0.save("a@x", "ra").unwrap();
        let fake = Fake::default().file("ra", "D1", 403, &[]);
        let (located, _) = locator(fake, &scratch).ancestors("D1", &|_| false);
        assert_eq!(located, Located::NotVisible);
    }

    #[test]
    fn no_accounts() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_no_accounts");
        let (located, dropped) = locator(Fake::default(), &scratch).ancestors("D1", &|_| false);
        assert_eq!(located, Located::NoAccount);
        assert!(dropped.is_empty());
    }

    #[test]
    fn revoked_account_is_forgotten() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_revoked_account");
        scratch.0.save("a@x", "ra").unwrap();
        scratch.0.save("b@x", "rb").unwrap();
        let fake = tree("rb").refusing("ra", 400, r#"{"error":"invalid_grant"}"#);
        let (located, dropped) = locator(fake, &scratch).ancestors("D1", &|_| false);
        assert_eq!(ids(located), ["P1", "P2", "root"]);
        assert_eq!(dropped, ["a@x"]);
        assert_eq!(scratch.0.list(), ["b@x"]);
    }

    #[test]
    fn a_failing_account_is_reported_when_none_answers() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_failing_account");
        scratch.0.save("a@x", "ra").unwrap();
        // The file is unknown to the fake, so the call fails in transit.
        let (located, dropped) = locator(Fake::default(), &scratch).ancestors("D1", &|_| false);
        assert_eq!(located, Located::Failed("connection refused".into()));
        assert!(dropped.is_empty());
        assert_eq!(scratch.0.list(), ["a@x"]);
    }

    #[test]
    fn tokens_and_root_are_fetched_once() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.drive_cache");
        scratch.0.save("a@x", "ra").unwrap();
        let loc = locator(tree("ra"), &scratch);
        loc.ancestors("D1", &|_| false);
        loc.ancestors("D1", &|_| false);
        let roots = loc
            .transport
            .seen
            .borrow()
            .iter()
            .filter(|u| u.contains("/root?"))
            .count();
        assert_eq!(roots, 1);
    }
}
