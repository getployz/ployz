//! Authoring through the hidden in-process Config Store (`PLOYZ_STORE=sqlite:PATH`):
//! the JSON contract of `project new`, `env new`, `service add`, `get`, `set` and `unset`.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

/// Run `ployz --json ARGS` against the Store at `store`; returns (exit code, stdout JSON).
fn ployz(store: Option<&Path>, args: &[&str]) -> (Option<i32>, Value) {
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .args(args)
        .arg("--json")
        .env("HOME", home.path())
        .env_remove("PLOYZ_STORE")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV");
    if let Some(store) = store {
        command.env("PLOYZ_STORE", format!("sqlite:{}", store.display()));
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON object ({error}): {stdout:?}"));
    (output.status.code(), json)
}

fn ok(store: &Path, args: &[&str]) -> Value {
    let (code, json) = ployz(Some(store), args);
    assert_eq!(code, Some(0), "{args:?}: {json}");
    json
}

fn error(store: &Path, args: &[&str]) -> Value {
    failed(store, args, 1)
}

fn failed(store: &Path, args: &[&str], exit: i32) -> Value {
    let (code, json) = ployz(Some(store), args);
    assert_eq!(code, Some(exit), "{args:?}: {json}");
    json.get("error").cloned().unwrap()
}

#[test]
fn an_agent_creates_and_edits_an_image_service() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");

    let created = ok(&store, &["project", "new", "shop"]);
    assert_eq!(created.pointer("/project/name"), Some(&json!("shop")));
    assert_eq!(
        created.pointer("/environment/name"),
        Some(&json!("production"))
    );

    let added = ok(&store, &["service", "add", "web", "--image", "nginx:1"]);
    assert_eq!(added.pointer("/service/name"), Some(&json!("web")));
    assert_eq!(
        added.get("staged"),
        Some(&json!(["web.command", "web.image", "web.replicas"]))
    );
    assert_eq!(added.get("immediate"), Some(&json!([])));

    let set = ok(
        &store,
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

    let got = ok(&store, &["get", "web"]);
    assert_eq!(
        got.get("settings"),
        Some(&json!([
            { "path": "web.command", "value": "nginx -g 'daemon off;'", "default": null, "apply": "staged" },
            { "path": "web.image", "value": "nginx:1", "default": null, "apply": "staged" },
            { "path": "web.replicas", "value": 3, "default": 1, "apply": "staged" },
        ]))
    );

    ok(&store, &["unset", "web.replicas"]);
    let got = ok(&store, &["get", "web.replicas"]);
    assert_eq!(got.pointer("/settings/0/value"), Some(&json!(1)));
}

#[test]
fn a_stale_revision_conflicts_and_names_the_read_that_refreshes_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["env", "new", "staging"]);
    ok(
        &store,
        &[
            "service", "add", "web", "--image", "nginx:1", "--env", "staging",
        ],
    );
    let error = error(
        &store,
        &["set", "web.replicas=2", "--expect", "1", "--env", "staging"],
    );
    assert_eq!(error.get("code"), Some(&json!("conflict")));
    assert_eq!(
        error.get("details"),
        Some(&json!({ "revision": 2, "next": "ployz get --env staging" }))
    );
}

#[test]
fn mistakes_fail_with_their_codes() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    let no_project = error(&store, &["get"]);
    assert_eq!(no_project.get("code"), Some(&json!("not_found")));
    assert_eq!(
        no_project.pointer("/details/next"),
        Some(&json!("ployz project new NAME"))
    );
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "web", "--image", "nginx:1"]);
    let no_env = error(&store, &["get", "--env", "preview"]);
    assert_eq!(
        no_env.pointer("/details/next"),
        Some(&json!("ployz env new preview --project shop"))
    );

    let malformed = failed(&store, &["set", "web.replicas"], 2);
    assert_eq!(malformed.get("code"), Some(&json!("invalid_argument")));
    let bad_revision = failed(
        &store,
        &["set", "web.replicas=2", "--expect", "SECRET-CANARY"],
        2,
    );
    let bad_name = error(
        &store,
        &["service", "add", "SECRET-CANARY", "--image", "nginx:1"],
    );
    for error in [bad_revision, bad_name] {
        assert_eq!(error.get("code"), Some(&json!("invalid_argument")));
        assert!(!error.to_string().contains("CANARY"), "echoed: {error}");
    }

    for (args, code) in [
        (&["set", "web.replicas=lots"][..], "invalid_argument"),
        (&["set", "web.nope=1"], "invalid_argument"),
        (&["set", "api.replicas=1"], "not_found"),
        (&["project", "new", "shop"], "conflict"),
        (&["service", "add", "web", "--image", "nginx:2"], "conflict"),
    ] {
        let error = error(&store, args);
        assert_eq!(error.get("code"), Some(&json!(code)), "{args:?}: {error}");
        assert!(
            !error.to_string().contains("lots"),
            "values are never echoed"
        );
    }

    let (code, json) = ployz(None, &["get"]);
    assert_eq!(code, Some(1));
    assert_eq!(json.pointer("/error/code"), Some(&json!("unsupported")));
}

#[test]
fn an_ambiguous_project_names_the_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["project", "new", "blog"]);
    let error = error(&store, &["env", "new", "staging"]);
    assert_eq!(error.get("code"), Some(&json!("ambiguous")));
    assert_eq!(
        error.get("details"),
        Some(&json!({
            "projects": ["blog", "shop"],
            "next": "ployz env new staging --json --project PROJECT",
        }))
    );
}
