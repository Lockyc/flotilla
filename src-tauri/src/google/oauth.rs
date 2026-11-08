//! Connecting a Google account: the installed-app OAuth flow with a loopback redirect. Consent
//! runs in the default browser, never a flotilla webview, because Google refuses sign-in inside
//! embedded webviews.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use url::Url;

use super::accounts::Keychain;
use super::http::Transport;
use super::Client;

const AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN: &str = "https://oauth2.googleapis.com/token";
const ABOUT: &str = "https://www.googleapis.com/drive/v3/about?fields=user(emailAddress)";
const SCOPE: &str = "https://www.googleapis.com/auth/drive.metadata.readonly";

/// How long the browser has to come back with consent.
const WAIT: Duration = Duration::from_secs(5 * 60);

/// The most read of a loopback request's line, and again of its headers.
const REQUEST_LIMIT: u64 = 8 * 1024;

/// The most time one loopback request's line and headers together may take to arrive.
const REQUEST_BUDGET: Duration = Duration::from_secs(5);

/// A PKCE pair: the verifier stays here, the S256 challenge goes in the consent URL.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// The verifier is the secret half of the pair, so it never reaches a log.
impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .finish()
    }
}

impl Pkce {
    pub fn new() -> Pkce {
        Pkce::from_verifier(random_token())
    }

    pub fn from_verifier(verifier: String) -> Pkce {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Pkce {
            verifier,
            challenge,
        }
    }
}

/// 32 random bytes, base64url: 43 characters, inside PKCE's verifier alphabet and length.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS random source");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The page that asks for consent. `prompt=consent` makes Google issue a refresh token even for
/// an account that consented before.
pub fn consent_url(client: &Client, redirect: &str, pkce: &Pkce, state: &str) -> Url {
    Url::parse_with_params(
        AUTH,
        [
            ("client_id", client.id),
            ("redirect_uri", redirect),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", pkce.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ],
    )
    .expect("a constant base URL")
}

/// What one request to the loopback redirect carries.
#[derive(Debug, PartialEq, Eq)]
pub enum Callback {
    Code(String),
    Denied(String),
    /// Not this flow's redirect: a wrong or missing `state`, or no code or error.
    Mismatch,
}

/// Read an HTTP request line (`GET /?state=…&code=… HTTP/1.1`) against the flow's `state`.
pub fn parse_callback(request_line: &str, state: &str) -> Callback {
    let Some(target) = request_line.split_whitespace().nth(1) else {
        return Callback::Mismatch;
    };
    let Ok(url) = Url::parse("http://127.0.0.1").and_then(|base| base.join(target)) else {
        return Callback::Mismatch;
    };
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    if param("state").as_deref() != Some(state) {
        return Callback::Mismatch;
    }
    match (param("code"), param("error")) {
        (Some(code), _) => Callback::Code(code),
        (None, Some(error)) => Callback::Denied(error),
        (None, None) => Callback::Mismatch,
    }
}

/// Connect an account: consent in the default browser, then store its refresh token under its
/// email, which is returned. Blocks until the browser comes back or [`WAIT`] passes, which is
/// the error `"timed out"` (the user walked away, so a caller raises no notice for it).
pub fn connect(
    client: &Client,
    http: &dyn Transport,
    keychain: &Keychain,
) -> Result<String, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let redirect = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().map_err(|e| e.to_string())?.port()
    );
    let pkce = Pkce::new();
    let state = random_token();
    shell_core::web::escape::escape_to_default_browser(
        consent_url(client, &redirect, &pkce, &state).as_str(),
    );
    let code = await_code(&listener, &state, Instant::now() + WAIT)?;
    redeem(client, http, keychain, &code, &redirect, &pkce.verifier)
}

/// Accept requests on the loopback listener until one carries this flow's `state`, answering
/// each. A request that isn't the redirect (a favicon fetch, a stale tab) is answered and
/// skipped rather than ending the flow.
fn await_code(listener: &TcpListener, state: &str, deadline: Instant) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    loop {
        match listener.accept() {
            Ok((stream, _)) => match answer(stream, state, REQUEST_BUDGET) {
                Some(Callback::Code(code)) => return Ok(code),
                Some(Callback::Denied(error)) => return Err(format!("consent denied: {error}")),
                Some(Callback::Mismatch) | None => {}
            },
            // No connection yet, or one the peer dropped before it was accepted (a cancelled
            // preconnect, a closed tab): the browser can still come back, so keep waiting.
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionReset
                        | ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.to_string()),
        }
        if Instant::now() >= deadline {
            return Err("timed out".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Read one request and reply with a page for the browser tab. `None` when the connection sent
/// no request (a browser's speculative preconnect). The request line and the headers are each
/// read to at most [`REQUEST_LIMIT`] bytes, and both within `budget`, so an endless or trickling
/// line ends as a `Mismatch`; the headers
/// are drained before replying because closing a socket with unread input resets it, and the
/// browser then shows a connection error instead of the page.
fn answer(stream: TcpStream, state: &str, budget: Duration) -> Option<Callback> {
    stream.set_nonblocking(false).ok()?;
    let budgeted = Budgeted {
        stream: &stream,
        deadline: Instant::now() + budget,
    };
    let mut reader = BufReader::new(budgeted.take(REQUEST_LIMIT));
    let mut line = String::new();
    let read = reader.read_line(&mut line);
    if line.is_empty() {
        return None;
    }
    let callback = if read.is_ok() && line.ends_with('\n') {
        parse_callback(&line, state)
    } else {
        Callback::Mismatch
    };
    reader.get_mut().set_limit(REQUEST_LIMIT);
    let mut header = String::new();
    while matches!(reader.read_line(&mut header), Ok(n) if n > 0) && header.trim_end() != "" {
        header.clear();
    }
    let (status, text) = match &callback {
        Callback::Code(_) => ("200 OK", "You can close this tab and return to flotilla."),
        Callback::Denied(_) => (
            "200 OK",
            "flotilla was not given access. You can close this tab.",
        ),
        Callback::Mismatch => (
            "400 Bad Request",
            "This is not the sign-in flotilla is waiting for.",
        ),
    };
    let body = format!("<!doctype html><meta charset=utf-8><title>flotilla</title><p>{text}</p>");
    let _ = write!(
        &stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    Some(callback)
}

/// A stream whose reads all share one deadline: each read waits at most for the time left, and
/// none starts once it has passed. A per-read timeout alone would let a client trickling a
/// byte at a time hold [`await_code`] far past [`WAIT`].
struct Budgeted<'a> {
    stream: &'a TcpStream,
    deadline: Instant,
}

impl Read for Budgeted<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

/// Exchange the code for tokens, read the account's email, and store its refresh token.
fn redeem(
    client: &Client,
    http: &dyn Transport,
    keychain: &Keychain,
    code: &str,
    redirect: &str,
    verifier: &str,
) -> Result<String, String> {
    let (status, body) = http.post_form(
        TOKEN,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect),
            ("client_id", client.id),
            ("client_secret", client.secret),
            ("code_verifier", verifier),
        ],
    )?;
    let tokens = json(&body);
    if status != 200 {
        return Err(format!(
            "token exchange failed ({status}{})",
            tokens["error"]
                .as_str()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        ));
    }
    let (Some(access), Some(refresh)) = (
        tokens["access_token"].as_str(),
        tokens["refresh_token"].as_str(),
    ) else {
        return Err("token exchange returned no refresh token".into());
    };
    let (status, body) = http.get(ABOUT, access)?;
    if status != 200 {
        return Err(format!("reading the account failed ({status})"));
    }
    let Some(email) = json(&body)["user"]["emailAddress"]
        .as_str()
        .map(str::to_owned)
    else {
        return Err("the account has no email address".into());
    };
    keychain.save(&email, refresh)?;
    Ok(email)
}

/// The body as JSON, or `null` when it isn't, so a missing field reads the same either way.
fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::accounts::Scratch;
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn pkce_debug_hides_the_verifier() {
        let shown = format!("{:?}", Pkce::from_verifier("secret-verifier".into()));
        assert!(!shown.contains("secret-verifier"));
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn an_endless_request_line_is_a_mismatch() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let line = format!("GET /?state=S&code=C&pad={}", "x".repeat(10 * 1024));
            s.write_all(line.as_bytes()).unwrap();
            s.shutdown(std::net::Shutdown::Write).unwrap();
            let mut reply = String::new();
            std::io::Read::read_to_string(&mut s, &mut reply).unwrap();
            reply
        });
        let (stream, _) = listener.accept().unwrap();
        assert_eq!(
            answer(stream, "S", REQUEST_BUDGET),
            Some(Callback::Mismatch)
        );
        assert!(browser.join().unwrap().starts_with("HTTP/1.1 400"));
    }

    #[test]
    fn a_trickling_request_ends_at_the_budget() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            for byte in b"GET /?state=S&code=C" {
                if s.write_all(&[*byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let (stream, _) = listener.accept().unwrap();
        let started = Instant::now();
        let got = answer(stream, "S", Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(got, Some(Callback::Mismatch));
        browser.join().unwrap();
    }

    const CLIENT: Client = Client {
        id: "cid",
        secret: "csecret",
    };

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pkce_new_is_fresh_and_well_formed() {
        let (a, b) = (Pkce::new(), Pkce::new());
        assert_ne!(a.verifier, b.verifier);
        assert_eq!(a.verifier.len(), 43);
        assert_eq!(Pkce::from_verifier(a.verifier.clone()), a);
    }

    #[test]
    fn consent_url_carries_the_flow() {
        let pkce = Pkce::from_verifier("v".into());
        let url = consent_url(&CLIENT, "http://127.0.0.1:4321", &pkce, "st");
        assert_eq!(
            url.as_str().split('?').next(),
            Some("https://accounts.google.com/o/oauth2/v2/auth")
        );
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(
            q["scope"],
            "https://www.googleapis.com/auth/drive.metadata.readonly"
        );
        assert_eq!(q["code_challenge"], pkce.challenge);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["prompt"], "consent");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:4321");
        assert_eq!(q["state"], "st");
        assert_eq!(q["client_id"], "cid");
    }

    #[test]
    fn callback_parsing() {
        assert_eq!(
            parse_callback("GET /?state=S&code=C HTTP/1.1", "S"),
            Callback::Code("C".into())
        );
        assert_eq!(
            parse_callback("GET /?state=T&code=C HTTP/1.1", "S"),
            Callback::Mismatch
        );
        assert_eq!(
            parse_callback("GET /?state=S&error=access_denied", "S"),
            Callback::Denied("access_denied".into())
        );
        assert_eq!(parse_callback("GET / HTTP/1.1", "S"), Callback::Mismatch);
        assert_eq!(parse_callback("", "S"), Callback::Mismatch);
    }

    /// Send one request line to the listener and return the reply.
    fn request(port: u16, line: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "{line}\r\nHost: x\r\n\r\n").unwrap();
        let mut reply = String::new();
        std::io::Read::read_to_string(&mut s, &mut reply).unwrap();
        reply
    }

    #[test]
    fn await_code_skips_strays_and_takes_the_redirect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = std::thread::spawn(move || {
            drop(TcpStream::connect(("127.0.0.1", port)).unwrap());
            let stray = request(port, "GET /favicon.ico HTTP/1.1");
            let ok = request(port, "GET /?state=S&code=C HTTP/1.1");
            (stray, ok)
        });
        let got = await_code(&listener, "S", Instant::now() + Duration::from_secs(10));
        let (stray, ok) = browser.join().unwrap();
        assert_eq!(got, Ok("C".into()));
        assert!(stray.starts_with("HTTP/1.1 400"));
        assert!(ok.starts_with("HTTP/1.1 200"));
        assert!(ok.contains("You can close this tab and return to flotilla."));
    }

    #[test]
    fn await_code_reports_denial_and_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser =
            std::thread::spawn(move || request(port, "GET /?state=S&error=access_denied HTTP/1.1"));
        let got = await_code(&listener, "S", Instant::now() + Duration::from_secs(10));
        browser.join().unwrap();
        assert_eq!(got, Err("consent denied: access_denied".into()));
        assert_eq!(
            await_code(&listener, "S", Instant::now()),
            Err("timed out".into())
        );
    }

    /// Answers each request from a script and records what it was sent.
    struct Fake {
        token: (u16, &'static str),
        about: (u16, &'static str),
        seen: RefCell<Vec<String>>,
    }

    impl Transport for Fake {
        fn get(&self, url: &str, bearer: &str) -> Result<(u16, String), String> {
            self.seen.borrow_mut().push(format!("GET {url} {bearer}"));
            Ok((self.about.0, self.about.1.into()))
        }
        fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String), String> {
            let form: Vec<_> = form.iter().map(|(k, v)| format!("{k}={v}")).collect();
            self.seen
                .borrow_mut()
                .push(format!("POST {url} {}", form.join("&")));
            Ok((self.token.0, self.token.1.into()))
        }
    }

    #[test]
    fn redeem_stores_the_refresh_token_under_the_email() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.redeem_stores");
        let kc = &scratch.0;
        let fake = Fake {
            token: (200, r#"{"access_token":"at","refresh_token":"rt"}"#),
            about: (200, r#"{"user":{"emailAddress":"me@x"}}"#),
            seen: RefCell::default(),
        };
        let got = redeem(&CLIENT, &fake, kc, "C", "http://127.0.0.1:9", "ver");
        assert_eq!(got, Ok("me@x".into()));
        assert_eq!(kc.refresh_token("me@x").as_deref(), Some("rt"));
        assert_eq!(
            *fake.seen.borrow(),
            [
                "POST https://oauth2.googleapis.com/token grant_type=authorization_code&code=C\
                 &redirect_uri=http://127.0.0.1:9&client_id=cid&client_secret=csecret&code_verifier=ver",
                "GET https://www.googleapis.com/drive/v3/about?fields=user(emailAddress) at",
            ]
        );
    }

    #[test]
    fn redeem_fails_without_saving() {
        let scratch = Scratch::new("au.lsjc.flotilla.test.redeem_fails");
        let kc = &scratch.0;
        let refused = Fake {
            token: (400, r#"{"error":"invalid_grant"}"#),
            about: (200, r#"{"user":{"emailAddress":"me@x"}}"#),
            seen: RefCell::default(),
        };
        assert_eq!(
            redeem(&CLIENT, &refused, kc, "C", "r", "v"),
            Err("token exchange failed (400: invalid_grant)".into())
        );
        let no_refresh = Fake {
            token: (200, r#"{"access_token":"at"}"#),
            about: (200, r#"{"user":{"emailAddress":"me@x"}}"#),
            seen: RefCell::default(),
        };
        assert!(redeem(&CLIENT, &no_refresh, kc, "C", "r", "v").is_err());
        assert!(kc.list().is_empty());
    }
}
