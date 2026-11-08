//! The Google OAuth client flotilla ships with, baked in at build time by `build.rs`, and the
//! accounts connected through it.

// No caller yet; the Drive routing that consumes this module removes this.
#![allow(dead_code)]

pub mod accounts;
pub mod http;
pub mod oauth;

/// An installed-app OAuth client: Google treats its secret as public, so it is baked into the binary.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Client {
    pub id: &'static str,
    pub secret: &'static str,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("id", &self.id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// The baked client, or `None` in a build made without one (Drive features then stay off).
pub fn client() -> Option<Client> {
    client_from(
        env!("FLOTILLA_GOOGLE_CLIENT_ID"),
        env!("FLOTILLA_GOOGLE_CLIENT_SECRET"),
    )
}

fn client_from(id: &'static str, secret: &'static str) -> Option<Client> {
    (!id.is_empty() && !secret.is_empty()).then_some(Client { id, secret })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_needs_both_halves() {
        assert_eq!(client_from("", ""), None);
        assert_eq!(client_from("x", ""), None);
        assert_eq!(client_from("", "y"), None);
        assert_eq!(
            client_from("x", "y"),
            Some(Client {
                id: "x",
                secret: "y"
            })
        );
    }
}
