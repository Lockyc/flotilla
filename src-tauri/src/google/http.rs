//! The HTTP seam the Google calls go through, so the flows above it test against a fake.

use std::time::Duration;

/// The two request shapes the Google calls need. Each answers `(status, body)`; `Err` is a
/// transport failure (no response at all).
pub trait Transport {
    /// `GET url` with `Authorization: Bearer <bearer>`.
    fn get(&self, url: &str, bearer: &str) -> Result<(u16, String), String>;
    /// `POST url` with a form-encoded body.
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String), String>;
}

/// The real transport. Blocking, so never call it on an async runtime's thread.
pub struct Http(reqwest::blocking::Client);

impl Http {
    /// reqwest is built with `rustls-no-provider`, so a process-wide rustls provider must be
    /// installed before the first client: ring, whichever of this and the updater runs first.
    pub fn new() -> Result<Self, String> {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            // Losing a race to another installer is fine: a provider is then installed either way.
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map(Http)
            .map_err(|e| e.to_string())
    }
}

impl Transport for Http {
    fn get(&self, url: &str, bearer: &str) -> Result<(u16, String), String> {
        answer(self.0.get(url).bearer_auth(bearer).send())
    }

    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String), String> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        answer(
            self.0
                .post(url)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(body)
                .send(),
        )
    }
}

/// The status and body; an error drops its URL so a failure message stays short.
fn answer(sent: reqwest::Result<reqwest::blocking::Response>) -> Result<(u16, String), String> {
    let resp = sent.map_err(|e| e.without_url().to_string())?;
    let status = resp.status().as_u16();
    let body = resp.text().map_err(|e| e.without_url().to_string())?;
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_builds_with_a_crypto_provider() {
        assert!(Http::new().is_ok());
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
