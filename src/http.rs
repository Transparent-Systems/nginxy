//! HTTP session: one `reqwest::blocking::Client` per endpoint plus the bearer
//! token, replacing `requests.Session` and its `Authorization` header.
//!
//! Blocking is deliberate — the application is a sleep-loop, not a server, so
//! an async runtime would add a dependency and a scheduler for nothing.

use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::Method;

use crate::settings::Auth;

/// Request timeout, same value as `DEFAULT_TIMEOUT` in `main.py`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// One endpoint. `url` is the API root from `config.yaml` (for example
/// `http://192.0.2.10:81/api`); paths are appended verbatim, exactly as the
/// Python f-strings do.
pub struct Session {
    client: Client,
    pub url: String,
    token: Option<String>,
}

impl Session {
    pub fn new(url: &str) -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .user_agent(concat!("nginxy/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            url: url.to_string(),
            token: None,
        })
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token;
    }

    /// `session.headers.pop("Authorization", None)` — the next call logs in
    /// again. Used after transport errors and by the 12 h refresh.
    pub fn invalidate(&mut self) {
        self.token = None;
    }

    /// A request to `{url}{path}`, carrying the token when there is one.
    pub fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let mut builder = self.client.request(method, format!("{}{}", self.url, path));
        if let Some(token) = &self.token {
            builder = builder.bearer_auth(token);
        }
        builder
    }

    /// Logs in and stores the token; `None` means the attempt failed (the
    /// caller logs the reason, which [`crate::npm`] already did).
    pub fn authenticate(&mut self, auth: &Auth) -> bool {
        let token = crate::npm::request_token(self, auth);
        let ok = token.is_some();
        self.token = token;
        ok
    }

    pub fn get(&self, path: &str) -> Result<Response, reqwest::Error> {
        self.request(Method::GET, path).send()
    }
}

/// `describe_http_failure()`: NPM explains a rejection in the body, while the
/// status line only says "400 Client Error".
pub fn describe_failure(status: u16, body: &str) -> String {
    let detail = collapse(body);
    if detail.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {detail}")
    }
}

/// `" ".join(response.text.split())[:300]` — whitespace collapsed, 300 chars.
fn collapse(body: &str) -> String {
    body.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_description_collapses_whitespace() {
        let detail = describe_failure(400, "  data\n must  have required\tproperty identity ");
        assert_eq!(
            detail,
            "HTTP 400: data must have required property identity"
        );
    }

    #[test]
    fn failure_description_without_body_is_status_only() {
        assert_eq!(describe_failure(502, "   "), "HTTP 502");
    }

    #[test]
    fn failure_description_truncates_at_300_chars() {
        let body = "x".repeat(1000);
        let detail = describe_failure(400, &body);
        assert_eq!(detail.chars().count(), "HTTP 400: ".chars().count() + 300);
    }

    #[test]
    fn failure_description_does_not_split_multibyte_chars() {
        let body = "é".repeat(400);
        let detail = describe_failure(400, &body);
        assert!(detail.ends_with(&"é".repeat(300)));
    }

    #[test]
    fn session_keeps_the_url_verbatim() {
        let session = Session::new("http://127.0.0.1:81/api").expect("client");
        assert_eq!(session.url, "http://127.0.0.1:81/api");
        assert!(!session.has_token());
    }

    #[test]
    fn token_can_be_set_and_invalidated() {
        let mut session = Session::new("http://127.0.0.1:81/api").expect("client");
        session.set_token(Some("abc".into()));
        assert!(session.has_token());
        session.invalidate();
        assert!(!session.has_token());
    }
}
