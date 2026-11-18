//! API contract tests against a mock NPM.
//!
//! These are the executable version of PLAN.md §6-M4: what must be created,
//! updated and deleted, in which order, and what a dry run must not send.

use std::fs;

use mockito::Matcher;
use nginxy::settings::Settings;
use nginxy::stop::Stop;
use nginxy::sync::Engine;

/// One source host as NPM returns it. `modified_on` is irrelevant to the diff
/// but present, because the real API sends it.
fn source_host(port: u16) -> serde_json::Value {
    serde_json::json!({
        "id": 1,
        "owner_user_id": 1,
        "created_on": "2026-01-01T00:00:00.000Z",
        "modified_on": "2026-01-02T00:00:00.000Z",
        "user": {"id": 1, "email": "admin@example.com"},
        "domain_names": ["app.example.com", "www.example.com"],
        "forward_host": "192.0.2.5",
        "forward_port": port,
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
        "access_list_id": 0,
        "certificate_id": 0,
        "locations": [],
        "meta": {"nginx_online": true, "nginx_err": null}
    })
}

/// A target host with the same domain set but its own id, timestamps,
/// certificate id and nginx status — exactly what a second NPM instance returns
/// for the same configuration.
fn target_host(port: u16) -> serde_json::Value {
    let mut host = source_host(port);
    let object = host.as_object_mut().expect("object");
    object.insert("id".into(), serde_json::json!(7));
    object.insert(
        "modified_on".into(),
        serde_json::json!("2026-02-02T00:00:00.000Z"),
    );
    object.insert("certificate_id".into(), serde_json::json!(12));
    object.insert(
        "meta".into(),
        serde_json::json!({"nginx_online": false, "nginx_err": "down"}),
    );
    host
}

struct Fixture {
    _dir: tempfile::TempDir,
    settings: Settings,
}

/// Writes a configuration folder that points at the two mock servers.
fn fixture(source_url: &str, target_url: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = format!(
        "source:\n  url: \"{source_url}\"\ntargets:\n  - name: \"Target-1\"\n    url: \"{target_url}\"\n"
    );
    fs::write(dir.path().join("config.yaml"), config).expect("config");
    fs::write(
        dir.path().join("log.yaml"),
        "version: 0.0.0-test\nlogging:\n  folder: logs\n  log_level: INFO\n",
    )
    .expect("log.yaml");
    fs::write(
        dir.path().join(".env"),
        "NPM_SOURCE_IDENTITY=tester@example.com\nNPM_SOURCE_SECRET=secret\n",
    )
    .expect(".env");

    let settings = Settings::load(dir.path()).expect("settings");
    Fixture {
        _dir: dir,
        settings,
    }
}

/// Mocks the login for one server and returns the mock (kept alive by caller).
fn token_mock(server: &mut mockito::Server) -> mockito::Mock {
    server
        .mock("POST", "/api/tokens")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"token":"token-1","expires":1767225600000}"#)
        .expect(1)
        .create()
}

fn list_mock(server: &mut mockito::Server, hosts: &serde_json::Value) -> mockito::Mock {
    server
        .mock("GET", "/api/nginx/proxy-hosts")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(hosts.to_string())
        .expect(1)
        .create()
}

fn run_once(settings: &Settings, dry_run: bool) -> u8 {
    let mut engine = Engine::new(settings, 0, dry_run, Stop::new()).expect("engine");
    engine.run()
}

#[test]
fn creates_hosts_missing_on_the_target() {
    let mut source = mockito::Server::new();
    let mut target = mockito::Server::new();
    let _source_token = token_mock(&mut source);
    let _target_token = token_mock(&mut target);
    let _source_list = list_mock(&mut source, &serde_json::json!([source_host(8080)]));
    let _target_list = list_mock(&mut target, &serde_json::json!([]));

    let create = target
        .mock("POST", "/api/nginx/proxy-hosts")
        .match_header("authorization", "Bearer token-1")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "domain_names": ["app.example.com", "www.example.com"],
            "forward_host": "192.0.2.5",
            "forward_port": 8080,
            // Ordinary configuration, propagated even though the Python
            // whitelist ignored it.
            "locations": [],
            "trust_forwarded_proto": false,
            "hsts_enabled": false
        })))
        .with_status(201)
        .with_header("content-type", "application/json")
        .with_body(source_host(8080).to_string())
        .expect(1)
        .create();

    let fixture = fixture(
        &format!("{}/api", source.url()),
        &format!("{}/api", target.url()),
    );
    assert_eq!(run_once(&fixture.settings, false), 0);

    create.assert();
}

#[test]
fn updates_hosts_whose_relevant_fields_changed() {
    let mut source = mockito::Server::new();
    let mut target = mockito::Server::new();
    let _source_token = token_mock(&mut source);
    let _target_token = token_mock(&mut target);
    let _source_list = list_mock(&mut source, &serde_json::json!([source_host(8080)]));
    let _target_list = list_mock(&mut target, &serde_json::json!([target_host(9999)]));

    let update = target
        .mock("PUT", "/api/nginx/proxy-hosts/7")
        .match_header("authorization", "Bearer token-1")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "forward_port": 8080
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(target_host(8080).to_string())
        .expect(1)
        .create();

    let fixture = fixture(
        &format!("{}/api", source.url()),
        &format!("{}/api", target.url()),
    );
    assert_eq!(run_once(&fixture.settings, false), 0);

    update.assert();
}

/// The 1.2 regression test: the target here differs in `id`, `modified_on`,
/// `certificate_id` and `meta` — everything a second NPM instance returns
/// differently — yet nothing is written.
#[test]
fn leaves_unchanged_hosts_alone() {
    let mut source = mockito::Server::new();
    let mut target = mockito::Server::new();
    let _source_token = token_mock(&mut source);
    let _target_token = token_mock(&mut target);
    let _source_list = list_mock(&mut source, &serde_json::json!([source_host(8080)]));
    let _target_list = list_mock(&mut target, &serde_json::json!([target_host(8080)]));

    let create = target
        .mock("POST", "/api/nginx/proxy-hosts")
        .expect(0)
        .create();
    let update = target.mock("PUT", Matcher::Any).expect(0).create();
    let delete = target.mock("DELETE", Matcher::Any).expect(0).create();

    let fixture = fixture(
        &format!("{}/api", source.url()),
        &format!("{}/api", target.url()),
    );
    assert_eq!(run_once(&fixture.settings, false), 0);

    create.assert();
    update.assert();
    delete.assert();
}

#[test]
fn deletes_hosts_the_source_no_longer_has() {
    let mut source = mockito::Server::new();
    let mut target = mockito::Server::new();
    let _source_token = token_mock(&mut source);
    let _target_token = token_mock(&mut target);
    let _source_list = list_mock(&mut source, &serde_json::json!([source_host(8080)]));

    let orphan = serde_json::json!([{
        "id": 9,
        "domain_names": ["old.example.com"],
        "forward_host": "192.0.2.9",
        "forward_port": 80
    }]);
    let _target_list = list_mock(&mut target, &orphan);

    let delete = target
        .mock("DELETE", "/api/nginx/proxy-hosts/9")
        .match_header("authorization", "Bearer token-1")
        .with_status(200)
        .expect(1)
        .create();
    let create = target
        .mock("POST", "/api/nginx/proxy-hosts")
        .expect(1)
        .create();

    let fixture = fixture(
        &format!("{}/api", source.url()),
        &format!("{}/api", target.url()),
    );
    assert_eq!(run_once(&fixture.settings, false), 0);

    delete.assert();
    create.assert();
}

/// Custom locations are ordinary configuration: a difference must produce a PUT
/// that carries them. The Python whitelist ignored this field entirely.
#[test]
fn custom_locations_are_propagated() {
    let mut source = mockito::Server::new();
    let mut target = mockito::Server::new();
    let _source_token = token_mock(&mut source);
    let _target_token = token_mock(&mut target);

    let location = serde_json::json!({
        "path": "/api",
        "forward_scheme": "http",
        "forward_host": "192.0.2.9",
        "forward_port": 9000,
        "advanced_config": ""
    });

    let mut source_value = source_host(8080);
    source_value["locations"] = serde_json::json!([location.clone()]);
    let _source_list = list_mock(&mut source, &serde_json::json!([source_value]));

    let mut target_value = target_host(8080);
    target_value["locations"] = serde_json::json!([]);
    let _target_list = list_mock(&mut target, &serde_json::json!([target_value]));

    let update = target
        .mock("PUT", "/api/nginx/proxy-hosts/7")
        .match_body(Matcher::PartialJson(
            serde_json::json!({ "locations": [location] }),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(target_host(8080).to_string())
        .expect(1)
        .create();

    let fixture = fixture(
        &format!("{}/api", source.url()),
        &format!("{}/api", target.url()),
    );
    assert_eq!(run_once(&fixture.settings, false), 0);

    update.assert();
}

#[test]
fn dry_run_sends_no_writes() {
    let mut source = mockito::Server::new();
    let mut target = mockito::Server::new();
    let _source_token = token_mock(&mut source);
    let _target_token = token_mock(&mut target);
    let _source_list = list_mock(&mut source, &serde_json::json!([source_host(8080)]));

    let orphan = serde_json::json!([{
        "id": 9,
        "domain_names": ["old.example.com"],
        "forward_host": "192.0.2.9",
        "forward_port": 80
    }]);
    let _target_list = list_mock(&mut target, &orphan);

    let create = target
        .mock("POST", "/api/nginx/proxy-hosts")
        .expect(0)
        .create();
    let update = target.mock("PUT", Matcher::Any).expect(0).create();
    let delete = target.mock("DELETE", Matcher::Any).expect(0).create();

    let fixture = fixture(
        &format!("{}/api", source.url()),
        &format!("{}/api", target.url()),
    );
    assert_eq!(run_once(&fixture.settings, true), 0);

    create.assert();
    update.assert();
    delete.assert();
}

#[test]
fn unreachable_source_ends_the_run_without_touching_targets() {
    let mut target = mockito::Server::new();
    let tokens = target.mock("POST", "/api/tokens").expect(0).create();
    let lists = target
        .mock("GET", "/api/nginx/proxy-hosts")
        .expect(0)
        .create();

    // Port 9 (discard) refuses the connection immediately.
    let fixture = fixture("http://127.0.0.1:9/api", &format!("{}/api", target.url()));
    assert_eq!(run_once(&fixture.settings, false), 0);

    tokens.assert();
    lists.assert();
}

#[test]
fn failed_login_skips_the_cycle() {
    let mut source = mockito::Server::new();
    let mut target = mockito::Server::new();

    // NPM answers a missing identity with 400 and names the field.
    let login = source
        .mock("POST", "/api/tokens")
        .with_status(400)
        .with_body(r#"{"error":{"code":"INVALID_PAYLOAD","message":"identity is required"}}"#)
        .expect(1)
        .create();
    let target_tokens = target.mock("POST", "/api/tokens").expect(0).create();

    let fixture = fixture(
        &format!("{}/api", source.url()),
        &format!("{}/api", target.url()),
    );
    assert_eq!(run_once(&fixture.settings, false), 0);

    login.assert();
    target_tokens.assert();
}
