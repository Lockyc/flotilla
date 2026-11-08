//! Connected Google accounts: one Keychain generic-password item each (account = the email,
//! password = the refresh token), so the Keychain is the only list of accounts.

use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
use security_framework::passwords;

/// One Keychain service's accounts.
pub struct Keychain {
    pub service: &'static str,
}

/// The service flotilla's connected accounts live under.
pub const APP: Keychain = Keychain {
    service: "au.lsjc.flotilla.google",
};

impl Keychain {
    /// Every connected account's email, sorted. Empty when there are none (the search answers
    /// `errSecItemNotFound`), and also on any other search error (a locked Keychain, refused
    /// access), since no account is usable then either.
    pub fn list(&self) -> Vec<String> {
        let found = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(self.service)
            .load_attributes(true)
            .limit(Limit::All)
            .search()
            .unwrap_or_default();
        let mut emails: Vec<String> = found
            .iter()
            .filter_map(|item| item.simplify_dict()?.remove("acct"))
            .collect();
        emails.sort();
        emails
    }

    /// The account's refresh token, or `None` when it is not connected.
    pub fn refresh_token(&self, email: &str) -> Option<String> {
        let bytes = passwords::get_generic_password(self.service, email).ok()?;
        String::from_utf8(bytes).ok()
    }

    /// Store the account's refresh token, replacing any it had.
    pub fn save(&self, email: &str, refresh: &str) -> Result<(), String> {
        passwords::set_generic_password(self.service, email, refresh.as_bytes())
            .map_err(|e| e.to_string())
    }

    /// Disconnect the account; a no-op when it is not connected.
    pub fn forget(&self, email: &str) {
        let _ = passwords::delete_generic_password(self.service, email);
    }
}

/// A test's own Keychain service, emptied when the test starts and again when it ends, even on
/// a failed assertion, so no test leaves items behind or sees another's.
#[cfg(test)]
pub struct Scratch(pub Keychain);

#[cfg(test)]
impl Scratch {
    pub fn new(service: &'static str) -> Scratch {
        let scratch = Scratch(Keychain { service });
        scratch.empty();
        scratch
    }

    fn empty(&self) {
        for email in self.0.list() {
            self.0.forget(&email);
        }
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        self.empty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_round_trip() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.accounts_round_trip");
        let kc = &scratch.0;
        kc.save("a@x", "r1").unwrap();
        kc.save("b@x", "r2").unwrap();
        assert_eq!(kc.list(), ["a@x", "b@x"]);
        assert_eq!(kc.refresh_token("a@x").as_deref(), Some("r1"));
        kc.save("a@x", "r3").unwrap();
        assert_eq!(kc.refresh_token("a@x").as_deref(), Some("r3"));
        assert_eq!(kc.list(), ["a@x", "b@x"]);
        kc.forget("a@x");
        assert_eq!(kc.refresh_token("a@x"), None);
        assert_eq!(kc.list(), ["b@x"]);
        kc.forget("b@x");
        assert!(kc.list().is_empty());
    }
}
