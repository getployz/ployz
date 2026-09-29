//! Authoring through the hidden in-process Config Store (`PLOYZ_STORE=sqlite:PATH`):
//! the JSON contract of `project new`, `env new`, `service add`, `get`, `set`,
//! `unset`, `schema` and `explain`, and Setting path completion.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

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
    let (code, json) = ployz(Some(store), args);
    assert_eq!(code, Some(1), "{args:?}: {json}");
    json.get("error").cloned().unwrap()
}

#[test]
fn an_agent_creates_and_edits_an_image_service() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");

    let created = ok(&store, &["project", "new", "shop"]);
    assert_eq!(created.pointer("/project/name"), Some(&json!("shop")));
    assert_eq!(
        created.pointer("/environment/namespace"),
        Some(&json!("shop-production"))
    );

    let added = ok(&store, &["service", "add", "web", "--image", "nginx:1"]);
    assert_eq!(added.pointer("/service/name"), Some(&json!("web")));
    assert_eq!(added.get("staged"), Some(&json!(["web"])));
    assert_eq!(added.get("immediate"), Some(&json!([])));

    let set = ok(
        &store,
        &[
            "set",
            "web.replicas=3",
            "web.startCommand=nginx -g 'daemon off;'",
        ],
    );
    assert_eq!(
        set.get("staged"),
        Some(&json!(["web.replicas", "web.startCommand"]))
    );
    assert_eq!(set.pointer("/environment/revision"), Some(&json!(3)));

    let got = ok(&store, &["get", "web"]);
    assert_eq!(
        got.pointer("/settings").unwrap().as_array().unwrap().len(),
        8
    );
    assert_eq!(
        got.pointer("/settings/5"),
        Some(&json!({ "path": "web.replicas", "value": 3, "default": 1, "apply": "staged" }))
    );
    assert_eq!(
        got.get("values"),
        Some(&json!({
            "image": "nginx:1",
            "maxRetries": 10,
            "replicas": 3,
            "restartPolicy": "unless-stopped",
            "startCommand": "nginx -g 'daemon off;'",
        }))
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
    assert_eq!(
        error(&store, &["get"]).get("code"),
        Some(&json!("not_found")),
        "no Project yet"
    );
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "web", "--image", "nginx:1"]);
    for (args, code) in [
        (&["set", "web.replicas"][..], "invalid_argument"),
        (&["set", "web.replicas=lots"], "invalid_argument"),
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
fn get_patch_get_round_trips_and_the_environment_shows_only_what_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "web", "--image", "nginx:1"]);

    let mut values = ok(&store, &["get", "web"])["values"].clone();
    values["cpuLimit"] = json!(0.5);
    values["memLimit"] = json!(1.5);
    values["restartPolicy"] = json!("on-failure");
    let patch = values.to_string();
    let patched = ok(&store, &["set", "web", "--patch", &patch]);
    assert_eq!(
        patched.get("staged"),
        Some(&json!([
            "web.cpuLimit",
            "web.memLimit",
            "web.restartPolicy"
        ]))
    );
    assert_eq!(ok(&store, &["get", "web"])["values"], values);

    let mut stdin = Command::new(env!("CARGO_BIN_EXE_ployz"));
    stdin
        .args(["set", "web", "--patch", "-", "--json"])
        .env("PLOYZ_STORE", format!("sqlite:{}", store.display()))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut child = stdin.spawn().unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), br#"{"replicas": 2}"#).unwrap();
    assert!(child.wait_with_output().unwrap().status.success());

    let whole = ok(&store, &["get"]);
    let paths = whole["settings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            "web.cpuLimit",
            "web.image",
            "web.memLimit",
            "web.replicas",
            "web.restartPolicy"
        ]
    );
    assert_eq!(whole.get("values"), None);
    assert_eq!(
        ok(&store, &["get", "--all"])["settings"]
            .as_array()
            .unwrap()
            .len(),
        8
    );

    for patch in [r#"{"memLimit": null}"#, "not json"] {
        let refused = error(&store, &["set", "web", "--patch", patch]);
        assert_eq!(
            refused.get("code"),
            Some(&json!("invalid_argument")),
            "{patch}"
        );
    }
}

#[test]
fn the_catalog_describes_settings_without_a_store() {
    let (code, schema) = ployz(None, &["schema"]);
    assert_eq!(code, Some(0));
    assert_eq!(
        schema.get("$schema"),
        Some(&json!("https://json-schema.org/draft/2020-12/schema"))
    );
    assert_eq!(schema.get("x-ployz-version"), Some(&json!(1)));
    assert_eq!(
        schema.pointer("/$defs/service/properties/memLimit/title"),
        Some(&json!("Memory limit"))
    );
    let (_, again) = ployz(None, &["schema"]);
    assert_eq!(schema.to_string(), again.to_string(), "deterministic");

    let (code, one) = ployz(None, &["schema", "web.replicas"]);
    assert_eq!(code, Some(0));
    assert_eq!(one.get("maximum"), Some(&json!(50)));

    let (code, explained) = ployz(None, &["explain", "web.restartPolicy"]);
    assert_eq!(code, Some(0));
    assert_eq!(explained.get("path"), Some(&json!("web.restartPolicy")));
    assert_eq!(
        explained.get("example"),
        Some(&json!("ployz set 'web.restartPolicy=on-failure'"))
    );
    assert_eq!(
        explained.pointer("/schema/x-ployz-apply"),
        Some(&json!("staged"))
    );

    let (code, json) = ployz(None, &["explain", "web.restart_policy"]);
    assert_eq!(code, Some(1));
    assert_eq!(
        json.pointer("/error/details/did_you_mean"),
        Some(&json!("restartPolicy"))
    );
    assert!(json.pointer("/error/details/valid_children").is_some());
}

#[test]
fn setting_paths_complete_from_the_store_and_the_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "web", "--image", "nginx:1"]);
    let complete = |word: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args(["--", "ployz", "set", word])
            .env("PLOYZ_COMPLETE", "bash")
            .env("PLOYZ_STORE", format!("sqlite:{}", store.display()))
            .env("_CLAP_COMPLETE_INDEX", "2")
            .env("_CLAP_IFS", "\n")
            .env_remove("PLOYZ_PROJECT")
            .env_remove("PLOYZ_ENV")
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!(complete("w").trim(), "web.");
    assert_eq!(complete("web.me").trim(), "web.memLimit");
}
