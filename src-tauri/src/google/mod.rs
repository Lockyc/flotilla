//! The Google OAuth client flotilla ships with, baked in at build time by `build.rs`, and the
//! accounts connected through it.

pub mod accounts;
pub mod drive;
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

/// The first `reason` of a Google API error body (`accessNotConfigured`,
/// `insufficientFilePermissions`, …); `None` when the body carries none.
pub(crate) fn reason(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    json["error"]["errors"][0]["reason"]
        .as_str()
        .map(str::to_owned)
}

/// `"<what> failed (<status>)"`, carrying Google's reason and message when the body has them, so
/// a misconfigured project or a refused account says why. Error bodies hold no credentials.
pub(crate) fn failed(what: &str, status: u16, body: &str) -> String {
    let json: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let message = json["error"]["message"].as_str();
    let detail = match (reason(body), message) {
        (Some(r), Some(m)) => format!(" {r}: {m}"),
        (Some(r), None) => format!(" {r}"),
        (None, Some(m)) => format!(": {m}"),
        (None, None) => String::new(),
    };
    format!("{what} failed ({status}{detail})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_carries_what_google_said() {
        let both = r#"{"error":{"message":"Nope.","errors":[{"reason":"dailyLimitExceeded"}]}}"#;
        assert_eq!(
            failed("reading a file", 403, both),
            "reading a file failed (403 dailyLimitExceeded: Nope.)"
        );
        assert_eq!(
            failed(
                "x",
                403,
                r#"{"error":{"errors":[{"reason":"domainPolicy"}]}}"#
            ),
            "x failed (403 domainPolicy)"
        );
        assert_eq!(
            failed("x", 500, r#"{"error":{"message":"Backend Error"}}"#),
            "x failed (500: Backend Error)"
        );
        assert_eq!(failed("x", 502, "<html>"), "x failed (502)");
        assert_eq!(reason(both).as_deref(), Some("dailyLimitExceeded"));
        assert_eq!(reason("{}"), None);
    }

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
