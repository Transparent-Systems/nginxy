//! NPM API calls: token, list, create, update, delete.
//!
//! Each function logs the same message the Python helper it replaces logs, and
//! follows the same failure policy: on a transport error the session's token is
//! dropped (so the next cycle logs in again), on an HTTP error the response body
//! is reported because NPM names the offending field there.

use reqwest::{Method, StatusCode};
use serde_json::Value;

use crate::host::ProxyHost;
use crate::http::{describe_failure, Session};
use crate::settings::Auth;

/// Why a host listing failed. The caller decides how to log it, because the
/// source and target paths word their messages differently in the Python app.
#[derive(Debug)]
pub enum ListError {
    /// Non-200 answer. Python: `Failed to fetch proxy hosts from source: {code}`.
    Status(u16),
    /// Could not reach the server at all (`requests.exceptions.RequestException`).
    Transport(String),
    /// Reachable, but the body was not a proxy-host list.
    Decode(String),
}

/// `get_token()`: logs in and returns the bearer token.
pub fn request_token(session: &Session, auth: &Auth) -> Option<String> {
    let payload = match auth {
        Auth::Payload(value) => value.clone(),
        Auth::InvalidJson => {
            log::error!("Error: auth_payload in config.yaml is not valid JSON.");
            return None;
        }
        Auth::Missing => {
            // The Python app posts {"identity": null, "secret": null} and reports
            // the 400 that NPM answers; the CLI refuses to start in this case.
            log::error!("No credentials configured for {}.", session.url);
            return None;
        }
    };

    let url = session.url.clone();
    let response = match session
        .request(Method::POST, "/tokens")
        .json(&payload)
        .send()
    {
        Ok(response) => response,
        Err(err) => {
            log::error!("Error obtaining token from {url}: {err}");
            return None;
        }
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        log::error!(
            "Authentication with {url} failed: {}",
            describe_failure(status.as_u16(), &body)
        );
        return None;
    }

    match response.json::<Value>() {
        Ok(value) => value
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_owned),
        Err(_) => {
            log::error!("Authentication with {url} did not return JSON.");
            None
        }
    }
}

/// `GET {url}/nginx/proxy-hosts`.
///
/// Both NPM endpoints answer `200`; the Python source path checks exactly that,
/// so the check is shared here.
pub fn list_proxy_hosts(session: &Session) -> Result<Vec<ProxyHost>, ListError> {
    let response = session
        .get("/nginx/proxy-hosts")
        .map_err(|err| ListError::Transport(err.to_string()))?;

    let status = response.status();
    if status != StatusCode::OK {
        return Err(ListError::Status(status.as_u16()));
    }

    response
        .json::<Vec<ProxyHost>>()
        .map_err(|err| ListError::Decode(err.to_string()))
}

/// `add_proxy_host()`: `POST {url}/nginx/proxy-hosts`.
pub fn add_proxy_host(session: &mut Session, host: &ProxyHost) -> Option<Value> {
    let url = format!("{}/nginx/proxy-hosts", session.url);
    log::info!(
        "Creating proxy host with domains {:?} at {url}",
        host.domain_names
    );

    let request = session
        .request(Method::POST, "/nginx/proxy-hosts")
        .json(&host.payload());

    match request.send() {
        Ok(response) => {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            if status.is_success() {
                log::info!("Successfully created proxy host: {:?}", host.domain_names);
                return serde_json::from_str(&body).ok().or(Some(Value::Null));
            }
            log::warn!(
                "Network error creating host on {}. Session invalidated.",
                session.url
            );
            session.invalidate();
            log::error!(
                "Failed to create proxy host {:?}: {}",
                host.domain_names,
                status_error(status, &url)
            );
            log::error!("API Error Detail: {body}");
            None
        }
        Err(err) => {
            log::warn!(
                "Network error creating host on {}. Session invalidated.",
                session.url
            );
            session.invalidate();
            log::error!("Failed to create proxy host {:?}: {err}", host.domain_names);
            None
        }
    }
}

/// Mirrors the text `requests` puts in its HTTPError message.
fn status_error(status: StatusCode, url: &str) -> String {
    match status.canonical_reason() {
        Some(reason) => format!("HTTP {} {reason} for url: {url}", status.as_u16()),
        None => format!("HTTP {} for url: {url}", status.as_u16()),
    }
}

/// `update_proxy_host()`: `PUT {url}/nginx/proxy-hosts/{id}`.
pub fn update_proxy_host(session: &mut Session, host_id: i64, host: &ProxyHost) -> Option<Value> {
    let url = format!("{}/nginx/proxy-hosts/{host_id}", session.url);
    log::info!(
        "Updating proxy host with domains {:?} at {url}",
        host.domain_names
    );

    let request = session
        .request(Method::PUT, &format!("/nginx/proxy-hosts/{host_id}"))
        .json(&host.payload());

    match request.send() {
        Ok(response) => {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            if status.is_success() {
                log::info!("Successfully updated proxy host: {:?}", host.domain_names);
                return serde_json::from_str(&body).ok().or(Some(Value::Null));
            }
            log::warn!(
                "Network error updating host on {}. Session invalidated.",
                session.url
            );
            session.invalidate();
            log::error!(
                "Failed to update proxy host {:?} (ID: {host_id}): {}",
                host.domain_names,
                status_error(status, &url)
            );
            log::error!("API Error Detail: {body}");
            None
        }
        Err(err) => {
            log::warn!(
                "Network error updating host on {}. Session invalidated.",
                session.url
            );
            session.invalidate();
            log::error!(
                "Failed to update proxy host {:?} (ID: {host_id}): {err}",
                host.domain_names
            );
            None
        }
    }
}

/// `delete_proxy_host()`: `DELETE {url}/nginx/proxy-hosts/{id}`.
pub fn delete_proxy_host(session: &mut Session, host_id: i64, host: Option<&ProxyHost>) -> bool {
    let url = format!("{}/nginx/proxy-hosts/{host_id}", session.url);
    let domains = domains_debug(host);
    log::info!("Deleting proxy host with domains {domains} at {url}");

    let request = session.request(Method::DELETE, &format!("/nginx/proxy-hosts/{host_id}"));

    match request.send() {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                log::info!("Successfully deleted proxy host: {domains} (ID: {host_id})");
                return true;
            }
            let body = response.text().unwrap_or_default();
            log::warn!(
                "Network error deleting host on {}. Session invalidated.",
                session.url
            );
            session.invalidate();
            log::error!(
                "Failed to delete proxy host {domains} (ID: {host_id}): {}",
                status_error(status, &url)
            );
            log::error!("API Error Detail: {body}");
            false
        }
        Err(err) => {
            log::warn!(
                "Network error deleting host on {}. Session invalidated.",
                session.url
            );
            session.invalidate();
            log::error!("Failed to delete proxy host {domains} (ID: {host_id}): {err}");
            false
        }
    }
}

/// The `domain_names` rendering used in log lines (`["a.com", "b.com"]`), or
/// Python's `Unknown` when the host was not supplied.
fn domains_debug(host: Option<&ProxyHost>) -> String {
    match host {
        Some(host) => format!("{:?}", host.domain_names),
        None => "Unknown".to_string(),
    }
}
