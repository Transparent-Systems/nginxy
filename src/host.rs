//! The proxy-host record and the rules applied to it: what may be sent to a
//! target, what counts as a difference, and which domain sets are the same host.
//!
//! The classification is a **deny-list**, not a whitelist. Everything NPM
//! returns is treated as configuration to be propagated, except the two
//! explicit sets below. That way a field added by a future NPM version is
//! compared and forwarded automatically instead of being silently dropped —
//! `every_field_is_classified` in the tests fails if a field ever appears that
//! is neither propagated nor deliberately excluded.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Fields the NPM API returns but must not receive back on create/update.
/// Verbatim from `main.py:176`.
pub const EXCLUDED_KEYS: [&str; 5] = ["id", "owner_user_id", "created_on", "modified_on", "user"];

/// Fields that are neither sent nor compared.
///
/// * `certificate_id` and `access_list_id` are identifiers inside *one* NPM
///   database. The same number means a different certificate or access list on
///   another instance, so copying them would attach the wrong object or fail.
/// * `meta` holds runtime status (`nginx_online`, `nginx_err` on NPM 2.17), which
///   changes for reasons that have nothing to do with the configuration.
///
/// `locations` and `trust_forwarded_proto`, by contrast, are ordinary
/// configuration and are therefore propagated.
pub const NOT_SENT: [&str; 3] = ["certificate_id", "access_list_id", "meta"];

/// The subset of [`NOT_SENT`] an operator could reasonably expect to be
/// synchronised. The sync loop logs a line when one of these differs, so the
/// gap is visible instead of silent. `meta` is not listed: it is status, and
/// reporting it would just echo whether nginx is up.
pub const REPORTED_AS_NOT_SENT: [&str; 2] = ["certificate_id", "access_list_id"];

/// A proxy host as NPM returns it.
///
/// Only two fields are typed; everything else is kept as raw JSON in `rest`.
/// That is deliberate: NPM adds fields between releases and the synchroniser
/// must forward them untouched, which a closed struct would silently drop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyHost {
    /// Absent on a host that has never been created on a target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,

    #[serde(default)]
    pub domain_names: Vec<String>,

    /// Every other field, exactly as received.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl ProxyHost {
    pub fn from_json(value: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }

    /// The body for `POST`/`PUT`: everything except the read-only and the
    /// per-instance fields (`EXCLUDED_KEYS` and `NOT_SENT`).
    pub fn payload(&self) -> Value {
        let mut map = Map::new();
        for (key, value) in &self.rest {
            if is_not_sent(key) {
                continue;
            }
            map.insert(key.clone(), value.clone());
        }
        map.insert(
            "domain_names".to_string(),
            Value::Array(
                self.domain_names
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        Value::Object(map)
    }

    /// `tuple(sorted(host['domain_names']))` — order-insensitive identity.
    pub fn domain_key(&self) -> Vec<String> {
        let mut key = self.domain_names.clone();
        key.sort();
        key
    }
}

/// True when any field we would send differs.
///
/// Comparing the payload rather than a hand-picked field list means a field NPM
/// adds later is propagated without a code change, and that changes to
/// read-only or per-instance fields (`modified_on`, `certificate_id`, `meta`)
/// never trigger a write. This is the fix for the Python behaviour where a
/// touched host object re-synced every target.
pub fn should_update(source: &ProxyHost, target: &ProxyHost) -> bool {
    source.payload() != target.payload()
}

/// Per-instance fields that differ between the two hosts.
///
/// They are deliberately not synchronised, but an operator who added a
/// certificate or an access list on the source deserves to see why the targets
/// did not get it.
pub fn not_synchronised_differences(source: &ProxyHost, target: &ProxyHost) -> Vec<&'static str> {
    REPORTED_AS_NOT_SENT
        .iter()
        .copied()
        .filter(|key| source.rest.get(*key) != target.rest.get(*key))
        .collect()
}

/// `EXCLUDED_KEYS` (read-only) plus `NOT_SENT` (per-instance and status).
fn is_not_sent(key: &str) -> bool {
    EXCLUDED_KEYS.contains(&key) || NOT_SENT.contains(&key)
}

/// Reproduces Python's `{tuple(sorted(h['domain_names'])): h for h in hosts}`.
///
/// The returned key order is the order of *first* appearance (Python dict
/// insertion order) while the map holds the *last* host for a key (Python dict
/// assignment), so iterating `keys` and looking up in `by_key` performs the
/// same visits in the same order as the original comprehension.
pub fn index_by_key(hosts: &[ProxyHost]) -> (Vec<Vec<String>>, HashMap<Vec<String>, &ProxyHost>) {
    let mut keys: Vec<Vec<String>> = Vec::new();
    let mut by_key: HashMap<Vec<String>, &ProxyHost> = HashMap::new();

    for host in hosts {
        let key = host.domain_key();
        if !by_key.contains_key(&key) {
            keys.push(key.clone());
        }
        by_key.insert(key, host);
    }

    (keys, by_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn host(value: Value) -> ProxyHost {
        ProxyHost::from_json(value).expect("valid proxy host")
    }

    /// Shaped like a host from NPM 2.17 (see the schema check in the transcript):
    /// every field the API returns, including the ones we filter.
    fn sample() -> ProxyHost {
        host(json!({
            "id": 7,
            "owner_user_id": 1,
            "created_on": "2026-01-01T00:00:00.000Z",
            "modified_on": "2026-01-02T00:00:00.000Z",
            "user": {"id": 1},
            "certificate_id": 0,
            "access_list_id": 0,
            "meta": {"nginx_online": true, "nginx_err": null},
            "domain_names": ["b.example.com", "a.example.com"],
            "forward_host": "192.0.2.5",
            "forward_port": 8080,
            "forward_scheme": "http",
            "advanced_config": "",
            "ssl_forced": true,
            "http2_support": false,
            "hsts_enabled": false,
            "hsts_subdomains": false,
            "trust_forwarded_proto": false,
            "block_exploits": true,
            "caching_enabled": false,
            "allow_websocket_upgrade": true,
            "enabled": true,
            "locations": [],
            "unknown_future_field": {"kept": true}
        }))
    }

    /// A value of the same kind but different content, used to prove that a
    /// field participates in the comparison.
    fn changed(value: &Value) -> Value {
        match value {
            Value::Bool(flag) => Value::Bool(!flag),
            Value::Number(number) => Value::from(number.as_i64().unwrap_or(0) + 1),
            Value::String(text) => Value::String(format!("{text}-changed")),
            Value::Array(items) => {
                let mut items = items.clone();
                items.push(json!("added"));
                Value::Array(items)
            }
            Value::Object(map) => {
                let mut map = map.clone();
                map.insert("changed".into(), json!(true));
                Value::Object(map)
            }
            Value::Null => json!("changed"),
        }
    }

    #[test]
    fn payload_drops_read_only_and_per_instance_fields() {
        let payload = sample().payload();
        let object = payload.as_object().expect("object");

        for excluded in EXCLUDED_KEYS.iter().chain(NOT_SENT.iter()) {
            assert!(
                !object.contains_key(*excluded),
                "{excluded} must not be sent"
            );
        }
        assert_eq!(
            object.get("unknown_future_field"),
            Some(&json!({"kept": true})),
            "unknown fields must survive"
        );
        assert_eq!(
            object.get("domain_names"),
            Some(&json!(["b.example.com", "a.example.com"]))
        );
        assert_eq!(object.get("forward_port"), Some(&json!(8080)));
        // These are ordinary configuration and must be propagated.
        assert_eq!(object.get("locations"), Some(&json!([])));
        assert_eq!(object.get("trust_forwarded_proto"), Some(&json!(false)));
    }

    #[test]
    fn domain_key_is_sorted_and_order_insensitive() {
        let a = sample();
        let mut b = sample();
        b.domain_names.reverse();
        assert_eq!(a.domain_key(), vec!["a.example.com", "b.example.com"]);
        assert_eq!(a.domain_key(), b.domain_key());
    }

    #[test]
    fn domain_key_keeps_duplicates() {
        let host = host(json!({"domain_names": ["a.com", "a.com"]}));
        assert_eq!(host.domain_key(), vec!["a.com", "a.com"]);
    }

    #[test]
    fn read_only_and_per_instance_changes_are_not_updates() {
        let source = sample();
        let mut target = sample();
        target.id = Some(99);
        target
            .rest
            .insert("modified_on".into(), json!("2027-01-01T00:00:00.000Z"));
        target.rest.insert("certificate_id".into(), json!(4));
        target.rest.insert("access_list_id".into(), json!(2));
        target.rest.insert(
            "meta".into(),
            json!({"nginx_online": false, "nginx_err": "down"}),
        );
        assert!(!should_update(&source, &target));
    }

    #[test]
    fn cosmetic_source_touch_does_not_resync() {
        // The Python app compared whole objects, so a bumped modified_on
        // re-synced every target even when nothing else changed.
        let source = sample();
        let mut target = sample();
        target
            .rest
            .insert("modified_on".into(), json!("2020-01-01T00:00:00.000Z"));
        assert!(!should_update(&source, &target));
    }

    #[test]
    fn every_field_is_classified() {
        // If NPM adds a field, this fails and names it: the choice between
        // "propagate" (default) and "per-instance, exclude" cannot be forgotten.
        let source = sample();
        for key in source.rest.keys() {
            if EXCLUDED_KEYS.contains(&key.as_str()) || NOT_SENT.contains(&key.as_str()) {
                continue;
            }
            let mut target = sample();
            let replacement = changed(target.rest.get(key.as_str()).expect("key present"));
            target.rest.insert(key.clone(), replacement);
            assert!(
                should_update(&source, &target),
                "{key} is neither propagated nor excluded — classify it in NOT_SENT"
            );
        }
    }

    #[test]
    fn locations_and_forwarded_proto_changes_are_updates() {
        // The two gaps the Python whitelist had: real configuration it ignored.
        let source = sample();

        let mut target = sample();
        target.rest.insert(
            "locations".into(),
            json!([{"path": "/api", "forward_host": "192.0.2.9", "forward_port": 9000}]),
        );
        assert!(should_update(&source, &target));

        let mut target = sample();
        target
            .rest
            .insert("trust_forwarded_proto".into(), json!(true));
        assert!(should_update(&source, &target));

        let mut target = sample();
        target.rest.insert("hsts_enabled".into(), json!(true));
        assert!(should_update(&source, &target));
    }

    #[test]
    fn per_instance_differences_are_reported() {
        let source = sample();
        let mut target = sample();
        target.rest.insert("certificate_id".into(), json!(4));
        target.rest.insert("access_list_id".into(), json!(2));
        target
            .rest
            .insert("meta".into(), json!({"nginx_online": false}));

        assert_eq!(
            not_synchronised_differences(&source, &target),
            vec!["certificate_id", "access_list_id"]
        );
        // An identical pair reports nothing.
        assert!(not_synchronised_differences(&source, &sample()).is_empty());
    }

    #[test]
    fn absent_and_null_are_different() {
        let source = sample();
        let mut target = sample();
        target.rest.insert("ssl_forced".into(), Value::Null);
        assert!(should_update(&source, &target));
    }

    #[test]
    fn index_by_key_keeps_first_position_last_value() {
        let first = host(json!({
            "id": 1, "domain_names": ["a.com", "b.com"], "forward_port": 1
        }));
        let superseded = host(json!({
            "id": 2, "domain_names": ["b.com", "a.com"], "forward_port": 2
        }));
        let other = host(json!({"id": 3, "domain_names": ["c.com"]}));
        let hosts = vec![first, superseded, other];

        let (keys, by_key) = index_by_key(&hosts);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], vec!["a.com", "b.com"]);
        assert_eq!(keys[1], vec!["c.com"]);
        assert_eq!(by_key[&keys[0]].id, Some(2), "last assignment wins");
        assert_eq!(by_key[&keys[1]].id, Some(3));
    }
}
