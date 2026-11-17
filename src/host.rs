//! The proxy-host record and the three rules the Python app applies to it:
//! payload filtering (`EXCLUDED_KEYS`), domain-set identity for diffing, and
//! the ten-field comparison in `should_update()`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Fields the NPM API returns but must not receive back on create/update.
/// Verbatim from `main.py:176`.
pub const EXCLUDED_KEYS: [&str; 5] = ["id", "owner_user_id", "created_on", "modified_on", "user"];

/// Fields `should_update()` compares. Verbatim from `main.py:503-510` — adding
/// or removing a name here changes how often target servers are rewritten.
pub const RELEVANT_KEYS: [&str; 10] = [
    "forward_host",
    "forward_port",
    "forward_scheme",
    "advanced_config",
    "ssl_forced",
    "http2_support",
    "block_exploits",
    "caching_enabled",
    "allow_websocket_upgrade",
    "enabled",
];

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

    /// The body for `POST`/`PUT`: everything except the read-only fields.
    pub fn payload(&self) -> Value {
        let mut map = Map::new();
        for (key, value) in &self.rest {
            if EXCLUDED_KEYS.contains(&key.as_str()) {
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

/// True when any of the [`RELEVANT_KEYS`] differs.
///
/// A key that is absent and a key holding JSON `null` are *not* equal here,
/// matching Python's `dict.get()` returning a distinct value for each.
pub fn should_update(source: &ProxyHost, target: &ProxyHost) -> bool {
    RELEVANT_KEYS
        .iter()
        .any(|key| source.rest.get(*key) != target.rest.get(*key))
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

    fn sample() -> ProxyHost {
        host(json!({
            "id": 7,
            "owner_user_id": 1,
            "created_on": "2026-01-01T00:00:00.000Z",
            "modified_on": "2026-01-02T00:00:00.000Z",
            "user": {"id": 1},
            "domain_names": ["b.example.com", "a.example.com"],
            "forward_host": "192.0.2.5",
            "forward_port": 8080,
            "forward_scheme": "http",
            "advanced_config": "",
            "ssl_forced": true,
            "http2_support": false,
            "block_exploits": true,
            "caching_enabled": false,
            "allow_websocket_upgrade": true,
            "enabled": true,
            "unknown_future_field": {"kept": true}
        }))
    }

    #[test]
    fn payload_drops_read_only_fields_and_keeps_the_rest() {
        let payload = sample().payload();
        let object = payload.as_object().expect("object");

        for excluded in EXCLUDED_KEYS {
            assert!(
                !object.contains_key(excluded),
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
        // Forwarded verbatim, still an integer.
        assert_eq!(object.get("forward_port"), Some(&json!(8080)));
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
    fn should_update_ignores_irrelevant_fields() {
        let source = sample();
        let mut target = sample();
        target.id = Some(99);
        target
            .rest
            .insert("modified_on".into(), json!("2027-01-01T00:00:00.000Z"));
        target.rest.insert("certificate_id".into(), json!(4));
        assert!(!should_update(&source, &target));
    }

    #[test]
    fn should_update_detects_each_relevant_field() {
        let source = sample();
        for key in RELEVANT_KEYS {
            let mut target = sample();
            target.rest.insert(key.to_string(), json!("changed"));
            assert!(
                should_update(&source, &target),
                "a change to {key} must trigger an update"
            );
        }
    }

    #[test]
    fn absent_and_null_are_different() {
        let source = host(json!({"domain_names": ["a.com"]}));
        let target = host(json!({"domain_names": ["a.com"], "ssl_forced": null}));
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
