//! Authoring through the Config Store: the JSON contract of `project new`, `env new`,
//! `service add`, `get`, `set` and `unset`. Every case runs twice: on the hidden
//! in-process Store (`PLOYZ_STORE=sqlite:PATH`) and over HTTPS to a Cloud that hosts
//! the Store behind `/api/config/{read,write}`, as `PLOYZ_TOKEN`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

use ployz_core::RpcError;
use ployz_store::{Actor, ConfigStore, OrganizationId};
use serde_json::{Value, json};

/// Where the CLI's Config Store is.
enum Target {
    Local(tempfile::TempDir),
    Cloud { url: String, token: &'static str },
}

/// The same empty Store, reached both ways.
fn targets() -> [Target; 2] {
    [
        Target::Local(tempfile::tempdir().unwrap()),
        Target::Cloud {
            url: fake_cloud(),
            token: "ployz_alice",
        },
    ]
}

/// Run `ployz --json ARGS` against `target`; returns (exit code, stdout JSON).
fn ployz(target: Option<&Target>, args: &[&str]) -> (Option<i32>, Value) {
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .args(args)
        .arg("--json")
        .env("HOME", home.path())
        .env("PLOYZ_CONFIG", home.path().join("config.yaml"))
        .env_remove("PLOYZ_STORE")
        .env_remove("PLOYZ_TOKEN")
        .env_remove("PLOYZ_CLOUD_URL")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV");
    match target {
        Some(Target::Local(dir)) => {
            let store = dir.path().join("store.db");
            command.env("PLOYZ_STORE", format!("sqlite:{}", store.display()));
        }
        Some(Target::Cloud { url, token }) => {
            command
                .env("PLOYZ_CLOUD_URL", url)
                .env("PLOYZ_TOKEN", token);
        }
        None => {}
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON object ({error}): {stdout:?}"));
    (output.status.code(), json)
}

fn ok(target: &Target, args: &[&str]) -> Value {
    let (code, json) = ployz(Some(target), args);
    assert_eq!(code, Some(0), "{args:?}: {json}");
    json
}

fn error(target: &Target, args: &[&str]) -> Value {
    let (code, json) = ployz(Some(target), args);
    assert_eq!(code, Some(1), "{args:?}: {json}");
    json.get("error").cloned().unwrap()
}

/// Cloud's `/api/config` contract over one in-memory Store: the bearer
/// `ployz_<org>` acts in Organization `<org>`; any other caller is refused 401.
fn fake_cloud() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let store = ConfigStore::open("sqlite::memory:").unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            serve(&store, stream.unwrap()).unwrap();
        }
    });
    url
}

fn serve(store: &ConfigStore, mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let path = request.split(' ').nth(1).unwrap_or_default().to_owned();
    let (mut length, mut organization) = (0, None);
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        if header.trim().is_empty() {
            break;
        }
        let header = header.to_ascii_lowercase();
        if let Some(value) = header.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
        if let Some(value) = header.strip_prefix("authorization: bearer ployz_") {
            organization = Some(Actor {
                organization: OrganizationId::parse(value.trim()).unwrap(),
            });
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let (status, reply) = match (organization, path.as_str()) {
        (None, _) => (401, json!({ "code": "UNAUTHORIZED" })),
        (Some(who), "/api/config/read") => {
            answer(store.read(&who, &serde_json::from_slice(&body).unwrap()))
        }
        (Some(who), "/api/config/write") => {
            answer(store.write(&who, serde_json::from_slice(&body).unwrap()))
        }
        (Some(_), _) => (404, json!({ "code": "NOT_FOUND" })),
    };
    let reply = reply.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    )
}

fn answer<T: serde::Serialize>(result: Result<T, RpcError>) -> (u16, Value) {
    match result {
        Ok(value) => (200, serde_json::to_value(value).unwrap()),
        Err(error) => (409, json!({ "error": error })),
    }
}

#[test]
fn an_agent_creates_and_edits_an_image_service() {
    for store in &targets() {
        let created = ok(store, &["project", "new", "shop"]);
        assert_eq!(created.pointer("/project/name"), Some(&json!("shop")));
        assert_eq!(
            created.pointer("/environment/namespace"),
            Some(&json!("shop-production"))
        );

        let added = ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        assert_eq!(added.pointer("/service/name"), Some(&json!("web")));
        assert_eq!(added.get("staged"), Some(&json!(["web"])));
        assert_eq!(added.get("immediate"), Some(&json!([])));

        let set = ok(
            store,
            &[
                "set",
                "web.replicas=3",
                "web.command=nginx -g 'daemon off;'",
            ],
        );
        assert_eq!(
            set.get("staged"),
            Some(&json!(["web.replicas", "web.command"]))
        );
        assert_eq!(set.pointer("/environment/revision"), Some(&json!(3)));

        let got = ok(store, &["get", "web"]);
        assert_eq!(
            got.get("settings"),
            Some(&json!([
                { "path": "web.command", "value": "nginx -g 'daemon off;'", "default": null, "apply": "staged" },
                { "path": "web.image", "value": "nginx:1", "default": null, "apply": "staged" },
                { "path": "web.replicas", "value": 3, "default": 1, "apply": "staged" },
            ]))
        );

        ok(store, &["unset", "web.replicas"]);
        let got = ok(store, &["get", "web.replicas"]);
        assert_eq!(got.pointer("/settings/0/value"), Some(&json!(1)));
    }
}

#[test]
fn a_stale_revision_conflicts_and_names_the_read_that_refreshes_it() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["env", "new", "staging"]);
        ok(
            store,
            &[
                "service", "add", "web", "--image", "nginx:1", "--env", "staging",
            ],
        );
        let error = error(
            store,
            &["set", "web.replicas=2", "--expect", "1", "--env", "staging"],
        );
        assert_eq!(error.get("code"), Some(&json!("conflict")));
        assert_eq!(
            error.get("details"),
            Some(&json!({ "revision": 2, "next": "ployz get --env staging" }))
        );
    }
}

#[test]
fn mistakes_fail_with_their_codes() {
    for store in &targets() {
        assert_eq!(
            error(store, &["get"]).get("code"),
            Some(&json!("not_found")),
            "no Project yet"
        );
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        for (args, code) in [
            (&["set", "web.replicas"][..], "invalid_argument"),
            (&["set", "web.replicas=lots"], "invalid_argument"),
            (&["set", "web.nope=1"], "invalid_argument"),
            (&["set", "api.replicas=1"], "not_found"),
            (&["project", "new", "shop"], "conflict"),
            (&["service", "add", "web", "--image", "nginx:2"], "conflict"),
        ] {
            let error = error(store, args);
            assert_eq!(error.get("code"), Some(&json!(code)), "{args:?}: {error}");
            assert!(
                !error.to_string().contains("lots"),
                "values are never echoed"
            );
        }
    }
}

#[test]
fn cloud_answers_only_a_credential_in_its_own_organization() {
    let url = fake_cloud();
    let alice = Target::Cloud {
        url: url.clone(),
        token: "ployz_alice",
    };
    ok(&alice, &["project", "new", "shop"]);
    let bob = Target::Cloud {
        url: url.clone(),
        token: "ployz_bob",
    };
    assert_eq!(
        error(&bob, &["get"]).get("code"),
        Some(&json!("not_found")),
        "another Organization sees nothing"
    );
    let refused = error(
        &Target::Cloud {
            url,
            token: "revoked",
        },
        &["get"],
    );
    assert_eq!(refused.get("code"), Some(&json!("unauthenticated")));
    assert_eq!(
        refused.pointer("/details/next"),
        Some(&json!("ployz token new"))
    );

    let (code, json) = ployz(None, &["get"]);
    assert_eq!(code, Some(1), "signed out: {json}");
    assert_eq!(json.pointer("/error/code"), Some(&json!("unauthenticated")));
    assert_eq!(
        json.pointer("/error/details/next"),
        Some(&json!("ployz login"))
    );
}
