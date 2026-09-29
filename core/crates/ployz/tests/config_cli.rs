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
        Some(&json!([
            "web.cpuLimit",
            "web.image",
            "web.maxRetries",
            "web.memLimit",
            "web.preDeployCommand",
            "web.replicas",
            "web.restartPolicy",
            "web.startCommand"
        ]))
    );
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
fn an_agent_reviews_publishes_and_discards() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    ok(&store, &["project", "new", "shop"]);
    let added = ok(&store, &["service", "add", "web", "--image", "nginx:1"]);
    assert_eq!(added.get("next"), Some(&json!("ployz diff")));
    let set = ok(&store, &["set", "web.replicas=3"]);
    assert_eq!(set.get("next"), Some(&json!("ployz diff")));

    let diff = ok(&store, &["diff"]);
    let version = diff
        .get("version")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    assert_eq!(diff.pointer("/changes/0/name"), Some(&json!("web")));
    assert_eq!(diff.pointer("/changes/0/lifecycle"), Some(&json!("create")));
    assert_eq!(
        diff.pointer("/changes/0/settings/0/path"),
        Some(&json!("web.replicas"))
    );
    assert_eq!(
        diff.get("next"),
        Some(&json!(format!("ployz publish --version {version}")))
    );

    // A review taken before another edit is stale.
    ok(&store, &["set", "web.startCommand=nginx"]);
    let stale = error(&store, &["publish", "--version", &version]);
    assert_eq!(stale.get("code"), Some(&json!("conflict")));
    assert_eq!(stale.pointer("/details/next"), Some(&json!("ployz diff")));
    let fresh = stale
        .pointer("/details/diff/version")
        .unwrap()
        .as_str()
        .unwrap();

    let published = ok(&store, &["publish", "--version", fresh]);
    assert_eq!(published.get("saved"), Some(&json!(1)));
    assert_eq!(published.get("created"), Some(&json!(true)));
    let diff = ok(&store, &["diff"]);
    assert_eq!(diff.get("published"), Some(&json!(true)));
    assert_eq!(diff.get("next"), None, "nothing left to publish");

    // The Service is published but not deployed: discarding it unpublishes it too.
    let discarded = ok(&store, &["discard", "web"]);
    assert_eq!(discarded.get("saved"), Some(&json!(2)));
    assert_eq!(discarded.get("next"), Some(&json!("ployz diff")));
    assert_eq!(ok(&store, &["diff"]).get("changes"), Some(&json!([])));
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
            "next": "ployz env new staging --project PROJECT",
        }))
    );

    // The hint is built from accepted words, so rejected values and raw `--` stay out.
    let next = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .arg("--json")
            .args(args)
            .env("HOME", home.path())
            .env("PLOYZ_STORE", format!("sqlite:{}", store.display()))
            .env_remove("PLOYZ_PROJECT")
            .env_remove("PLOYZ_ENV")
            .output()
            .unwrap();
        let json: Value = serde_json::from_slice(&output.stdout).unwrap();
        json.pointer("/error/details/next").cloned().unwrap()
    };
    assert_eq!(
        next(&["set", "--env", "production", "web.replicas=SECRET-CANARY"]),
        json!("ployz set 'web.replicas=VALUE' --project PROJECT --env production")
    );
    assert_eq!(
        next(&["get", "--", "web"]),
        json!("ployz get web --project PROJECT")
    );
}
