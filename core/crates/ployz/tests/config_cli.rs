//! Authoring through the Config Store: the JSON contract of `project new`, `env new`,
//! `service add`, `get`, `set`, `unset`, `diff`, `publish`, `discard`, `schema` and
//! `explain`, and Setting path completion. Store cases run twice: on the hidden
//! in-process Store (`PLOYZ_STORE=sqlite:PATH`) and over HTTPS to a Cloud that hosts
//! the Store behind `/api/config/{read,write}`, as `PLOYZ_TOKEN`. `link`, `status`
//! and `ctx` show how a directory link and overrides pick the scope.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

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

/// A new empty Store, reached both ways.
fn targets() -> [Target; 2] {
    [
        Target::Local(tempfile::tempdir().unwrap()),
        Target::Cloud {
            url: fake_cloud(),
            token: "ployz_alice",
        },
    ]
}

/// A `ployz` process with its own home, away from any ambient Store or sign-in.
fn isolated(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .env("HOME", home)
        .env("PLOYZ_CONFIG", home.join("config.yaml"))
        .env_remove("PLOYZ_STORE")
        .env_remove("PLOYZ_TOKEN")
        .env_remove("PLOYZ_CLOUD_URL")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV");
    command
}

impl Target {
    /// A `ployz` process pointed at this Store.
    fn command(&self, home: &std::path::Path) -> Command {
        let mut command = isolated(home);
        match self {
            Self::Local(dir) => {
                let store = dir.path().join("store.db");
                command.env("PLOYZ_STORE", format!("sqlite:{}", store.display()));
            }
            Self::Cloud { url, token } => {
                command
                    .env("PLOYZ_CLOUD_URL", url)
                    .env("PLOYZ_TOKEN", token);
            }
        }
        command
    }
}

/// Run `ployz --json ARGS` against `target`; returns (exit code, stdout JSON).
fn ployz(target: Option<&Target>, args: &[&str]) -> (Option<i32>, Value) {
    let home = tempfile::tempdir().unwrap();
    let mut command = target.map_or_else(
        || isolated(home.path()),
        |target| target.command(home.path()),
    );
    let output = command.arg("--json").args(args).output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON object ({error}): {stdout:?}"));
    (output.status.code(), json)
}

fn ok(store: &Target, args: &[&str]) -> Value {
    let (code, json) = ployz(Some(store), args);
    assert_eq!(code, Some(0), "{args:?}: {json}");
    json
}

fn error(store: &Target, args: &[&str]) -> Value {
    failed(store, args, 1)
}

fn failed(store: &Target, args: &[&str], exit: i32) -> Value {
    let (code, json) = ployz(Some(store), args);
    assert_eq!(code, Some(exit), "{args:?}: {json}");
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
            answer(store.write(&who, &serde_json::from_slice(&body).unwrap()))
        }
        (Some(who), "/api/cli/organizations") => {
            let id = who.organization.as_str();
            let organization = json!({ "id": id, "slug": id, "name": id, "current": true });
            (200, json!({ "organizations": [organization] }))
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
            created.pointer("/environment/name"),
            Some(&json!("production"))
        );

        let added = ok(store, &["service", "add", "web", "--image", "nginx:1"]);
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
            store,
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

        let got = ok(store, &["get", "web"]);
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
        let no_project = error(store, &["get"]);
        assert_eq!(no_project.get("code"), Some(&json!("not_found")));
        assert_eq!(
            no_project.pointer("/details/next"),
            Some(&json!("ployz project new NAME"))
        );
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        let no_env = error(store, &["get", "--env", "preview"]);
        assert_eq!(
            no_env.pointer("/details/next"),
            Some(&json!("ployz env new preview --project shop"))
        );

        let malformed = failed(store, &["set", "web.replicas"], 2);
        assert_eq!(malformed.get("code"), Some(&json!("invalid_argument")));
        let bad_revision = failed(
            store,
            &["set", "web.replicas=2", "--expect", "SECRET-CANARY"],
            2,
        );
        let bad_name = error(
            store,
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
fn an_agent_reviews_publishes_and_discards() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        let added = ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        assert_eq!(added.get("next"), Some(&json!("ployz diff")));
        let set = ok(store, &["set", "web.replicas=3"]);
        assert_eq!(set.get("next"), Some(&json!("ployz diff")));

        let diff = ok(store, &["diff"]);
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
            Some(&json!(format!("ployz deploy --expect-version {version}")))
        );

        // A review taken before another edit is stale.
        ok(store, &["set", "web.startCommand=nginx"]);
        let stale = error(store, &["publish", "--version", &version]);
        assert_eq!(stale.get("code"), Some(&json!("conflict")));
        assert_eq!(stale.pointer("/details/next"), Some(&json!("ployz diff")));
        let fresh = stale
            .pointer("/details/diff/version")
            .unwrap()
            .as_str()
            .unwrap();

        let published = ok(store, &["publish", "--version", fresh]);
        assert_eq!(published.get("saved"), Some(&json!(1)));
        assert_eq!(published.get("created"), Some(&json!(true)));
        assert_eq!(published.get("next"), Some(&json!("ployz deploy")));
        let diff = ok(store, &["diff"]);
        assert_eq!(diff.get("published"), Some(&json!(true)));
        let version = diff["version"].as_str().unwrap();
        assert_eq!(
            diff.get("next"),
            Some(&json!(format!("ployz deploy --expect-version {version}"))),
            "published is not yet deployed"
        );

        // The Service is published but not deployed: discarding it unpublishes it too.
        let discarded = ok(store, &["discard", "web"]);
        assert_eq!(discarded.get("saved"), Some(&json!(2)));
        assert_eq!(discarded.get("next"), Some(&json!("ployz diff")));
        assert_eq!(ok(store, &["diff"]).get("changes"), Some(&json!([])));
    }
}

#[test]
fn get_patch_get_round_trips_and_the_environment_shows_only_what_is_set() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);

        let mut values = ok(store, &["get", "web"])["values"].clone();
        values["cpuLimit"] = json!(0.5);
        values["memLimit"] = json!(1.5);
        values["restartPolicy"] = json!("on-failure");
        let patch = values.to_string();
        let patched = ok(store, &["set", "web", "--patch", &patch]);
        assert_eq!(
            patched.get("staged"),
            Some(&json!([
                "web.cpuLimit",
                "web.memLimit",
                "web.restartPolicy"
            ]))
        );
        assert_eq!(ok(store, &["get", "web"])["values"], values);

        let home = tempfile::tempdir().unwrap();
        let mut stdin = store.command(home.path());
        stdin
            .args(["set", "web", "--patch", "-", "--json"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let mut child = stdin.spawn().unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), br#"{"replicas": 2}"#).unwrap();
        assert!(child.wait_with_output().unwrap().status.success());

        let whole = ok(store, &["get"]);
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
            ok(store, &["get", "--all"])["settings"]
                .as_array()
                .unwrap()
                .len(),
            8
        );

        for patch in [r#"{"memLimit": null}"#, "not json"] {
            let refused = error(store, &["set", "web", "--patch", patch]);
            assert_eq!(
                refused.get("code"),
                Some(&json!("invalid_argument")),
                "{patch}"
            );
        }
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
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        let complete = |word: &str| {
            let home = tempfile::tempdir().unwrap();
            let output = store
                .command(home.path())
                .args(["--", "ployz", "set", word])
                .env("PLOYZ_COMPLETE", "bash")
                .env("_CLAP_COMPLETE_INDEX", "2")
                .env("_CLAP_IFS", "\n")
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap()
        };
        assert_eq!(complete("w").trim(), "web.");
        assert_eq!(complete("web.me").trim(), "web.memLimit");
    }
}

#[test]
fn an_ambiguous_project_names_the_rerun() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["project", "new", "blog"]);
        let refused = error(store, &["env", "new", "staging"]);
        assert_eq!(refused.get("code"), Some(&json!("ambiguous")));
        assert_eq!(
            refused.get("details"),
            Some(&json!({
                "projects": ["blog", "shop"],
                "next": "ployz env new staging --project PROJECT",
            }))
        );

        // The hint is built from accepted words, so rejected values and raw `--` stay out.
        let next = |args: &[&str]| error(store, args)["details"]["next"].clone();
        assert_eq!(
            next(&["set", "--env", "production", "web.replicas=SECRET-CANARY"]),
            json!("ployz set 'web.replicas=VALUE' --project PROJECT --env production")
        );
        assert_eq!(
            next(&["get", "--", "web"]),
            json!("ployz get web --project PROJECT")
        );
        // Guard flags survive the rerun, so a guarded write never turns blind.
        assert_eq!(
            next(&["set", "web.replicas=2", "--expect", "3"]),
            json!("ployz set 'web.replicas=VALUE' --expect 3 --project PROJECT")
        );
        assert_eq!(
            next(&["publish", "--version", "4:1:none"]),
            json!("ployz publish --version 4:1:none --project PROJECT")
        );
        assert_eq!(
            next(&["get", "--all"]),
            json!("ployz get --all --project PROJECT")
        );
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

#[test]
fn an_agent_plans_deploys_and_reads_the_deployment() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        ok(store, &["service", "add", "api", "--image", "nginx:1"]);

        let plan = ok(store, &["deploy", "web", "--plan"]);
        let version = plan["version"].as_str().unwrap().to_owned();
        assert_eq!(plan["namespace"], json!("shop-production"));
        assert_eq!(plan["changes"].as_array().unwrap().len(), 1);
        assert_eq!(plan["unresolved"], json!(["operations"]));
        assert_eq!(
            plan["next"],
            json!(format!("ployz deploy web --expect-version {version}"))
        );
        assert_eq!(
            error(store, &["deploy", "nope", "--plan"])["code"],
            json!("not_found")
        );

        // A review taken before another edit is stale.
        ok(store, &["set", "web.replicas=2"]);
        let stale = error(store, &["deploy", "--expect-version", &version]);
        assert_eq!(stale["code"], json!("conflict"));
        assert_eq!(stale["details"]["next"], json!("ployz diff"));

        let (code, deployed) = ployz(
            Some(store),
            &[
                "deploy",
                "--connect",
                "tcp://127.0.0.1:1",
                "--ssh-timeout",
                "1",
            ],
        );
        assert_eq!(deployed["saved"], json!(1));
        match store {
            // The CLI runs it: no Server answers, so it records that nothing ran.
            Target::Local(_) => {
                assert_eq!(code, Some(3), "{deployed}");
                assert_eq!(deployed["status"], json!("failed"));
                assert_eq!(deployed["outcome"]["type"], json!("not_executed"));
                assert_eq!(deployed["nodes"][0]["outcome"], json!("not_applied"));
            }
            // Cloud's runner runs it; the CLI only admits it.
            Target::Cloud { .. } => {
                assert_eq!(code, Some(0), "{deployed}");
                assert_eq!(deployed["status"], json!("queued"));
                assert_eq!(deployed["nodes"][0]["outcome"], json!("pending"));
            }
        }
        let id = deployed["id"].as_str().unwrap().to_owned();
        assert_eq!(
            deployed["next"],
            json!(format!("ployz deployment show {id}"))
        );

        let shown = ok(store, &["deployment", "show", &id]);
        assert_eq!(shown["id"], json!(id));
        assert_eq!(shown.get("next"), None);

        // `status` says what is deploying, or what needs attention.
        let status = ok(store, &["status"]);
        let show = json!(format!("ployz deployment show {id}"));
        assert_eq!(status["next"], show);
        match store {
            Target::Local(_) => {
                assert_eq!(status["deploying"], json!([]));
                assert_eq!(status["attention"][0]["reason"], json!("deployment_failed"));
                assert_eq!(status["attention"][0]["deployment"], json!(id));
            }
            Target::Cloud { .. } => {
                assert_eq!(status["deploying"][0]["id"], json!(id));
                assert_eq!(status["attention"], json!([]));
            }
        }
        let listed = ok(store, &["deployment", "ls", "--limit", "1"]);
        assert_eq!(listed["deployments"][0]["id"], json!(id));
        assert_eq!(listed["next_cursor"], Value::Null);
        failed(store, &["deployment", "show", "not-an-id"], 2);
    }
}

/// Run `ployz --json ARGS` in `dir` with a lasting `home`, where directory links live.
fn in_dir(
    store: &Target,
    home: &std::path::Path,
    dir: &std::path::Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> (Option<i32>, Value) {
    let output = store
        .command(home)
        .current_dir(dir)
        .envs(env.iter().copied())
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON object ({error}): {stdout:?}"));
    (output.status.code(), json)
}

#[test]
fn two_linked_directories_act_on_their_own_environments() {
    for store in &targets() {
        let home = tempfile::tempdir().unwrap();
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a_dir = a.path().canonicalize().unwrap();
        let nested = a_dir.join("src");
        std::fs::create_dir(&nested).unwrap();
        let run = |dir: &std::path::Path, args: &[&str], env: &[(&str, &str)]| {
            let (code, json) = in_dir(store, home.path(), dir, args, env);
            assert_eq!(code, Some(0), "{args:?}: {json}");
            json
        };
        ok(store, &["project", "new", "shop"]);
        ok(store, &["env", "new", "staging"]);

        let linked = run(&a_dir, &["link", "--env", "staging"], &[]);
        assert_eq!(
            linked.get("directory"),
            Some(&json!(a_dir.to_str().unwrap()))
        );
        assert_eq!(linked.get("project"), Some(&json!("shop")));
        assert_eq!(linked.get("environment"), Some(&json!("staging")));
        assert_eq!(linked.get("next"), Some(&json!("ployz status")));
        let linked = run(b.path(), &["link"], &[]);
        assert_eq!(linked.get("environment"), Some(&json!("production")));

        // A subdirectory acts where its linked ancestor does.
        let added = run(&nested, &["service", "add", "api", "--image", "api:1"], &[]);
        assert_eq!(added.pointer("/environment/name"), Some(&json!("staging")));
        let got = run(b.path(), &["get"], &[]);
        assert_eq!(got.pointer("/environment/name"), Some(&json!("production")));
        assert_eq!(got.get("settings"), Some(&json!([])));

        let status = run(&nested, &["status"], &[]);
        assert_eq!(status.pointer("/environment/name"), Some(&json!("staging")));
        assert_eq!(
            status.pointer("/scope/environment"),
            Some(&json!({ "name": "staging", "source": "link" }))
        );
        assert_eq!(
            status.pointer("/scope/link"),
            Some(&json!(a_dir.to_str().unwrap()))
        );
        assert_eq!(status.pointer("/staged/published"), Some(&json!(false)));
        assert_eq!(status.get("deploying"), Some(&json!([])));
        assert_eq!(status.get("attention"), Some(&json!([])));
        assert_eq!(status.get("next"), Some(&json!("ployz diff")));
        let organization = match store {
            Target::Local(_) => ("local", "local"),
            Target::Cloud { .. } => ("token", "alice"),
        };
        assert_eq!(
            status.pointer("/identity/credential"),
            Some(&json!(organization.0))
        );
        assert_eq!(
            status.pointer("/identity/organization/slug"),
            Some(&json!(organization.1))
        );

        // Flags beat environment variables, which beat the link.
        let by_env = run(&a_dir, &["status"], &[("PLOYZ_ENV", "production")]);
        assert_eq!(
            by_env.pointer("/scope/environment"),
            Some(&json!({ "name": "production", "source": "env" }))
        );
        assert_eq!(
            by_env.pointer("/scope/project/source"),
            Some(&json!("link"))
        );
        let by_flag = run(
            &a_dir,
            &["status", "--env", "staging"],
            &[("PLOYZ_ENV", "production")],
        );
        assert_eq!(
            by_flag.pointer("/scope/environment"),
            Some(&json!({ "name": "staging", "source": "flag" }))
        );

        let context = run(&nested, &["ctx"], &[]);
        assert_eq!(
            context.pointer("/environment/name"),
            Some(&json!("staging"))
        );
        assert_eq!(context.pointer("/project/source"), Some(&json!("link")));
        let via = match store {
            Target::Local(_) => "local",
            Target::Cloud { .. } => "cloud",
        };
        assert_eq!(context.pointer("/servers/via"), Some(&json!(via)));
    }
}

#[test]
fn missing_ambiguous_and_foreign_scope_name_the_fix() {
    for store in &targets() {
        let home = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| in_dir(store, home.path(), dir.path(), args, &[]);

        // Nothing yet: status still says who, and what to do first.
        let (code, status) = run(&["status"]);
        assert_eq!(code, Some(0), "{status}");
        assert_eq!(status.get("environment"), Some(&Value::Null));
        assert_eq!(
            status.pointer("/attention/0/reason"),
            Some(&json!("not_found"))
        );
        assert_eq!(status.get("next"), Some(&json!("ployz project new NAME")));
        let (code, refused) = run(&["link"]);
        assert_eq!(code, Some(1));
        assert_eq!(refused.pointer("/error/code"), Some(&json!("not_found")));

        ok(store, &["project", "new", "shop"]);
        ok(store, &["project", "new", "blog"]);
        let (_, status) = run(&["status"]);
        assert_eq!(
            status.pointer("/attention/0/reason"),
            Some(&json!("ambiguous"))
        );
        assert_eq!(
            status.get("next"),
            Some(&json!("ployz link --project PROJECT"))
        );
        let (code, refused) = run(&["link"]);
        assert_eq!(code, Some(1));
        assert_eq!(
            refused.pointer("/error/details/next"),
            Some(&json!("ployz link --project PROJECT"))
        );
        let (_, refused) = run(&["link", "--project", "blog", "--env", "preview"]);
        assert_eq!(
            refused.pointer("/error/details/next"),
            Some(&json!("ployz env new preview --project blog"))
        );

        ok(store, &["env", "new", "staging", "--project", "blog"]);
        let (code, _) = run(&["link", "--project", "blog", "--env", "staging"]);
        assert_eq!(code, Some(0));
        // Another Project on the command line drops the link's Environment.
        let (_, status) = run(&["status", "--project", "shop"]);
        assert_eq!(status.pointer("/environment/project"), Some(&json!("shop")));
        assert_eq!(
            status.pointer("/environment/name"),
            Some(&json!("production"))
        );
        assert_eq!(status.pointer("/scope/environment"), Some(&Value::Null));
        assert_eq!(status.pointer("/scope/link"), Some(&Value::Null));
    }

    // A link made in another Organization is refused, never followed.
    let store = &targets()[0];
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    ok(store, &["project", "new", "shop"]);
    let (code, _) = in_dir(store, home.path(), dir.path(), &["link"], &[]);
    assert_eq!(code, Some(0));
    let links = home.path().join("links.json");
    let moved = std::fs::read_to_string(&links)
        .unwrap()
        .replace("\"local\"", "\"acme\"");
    std::fs::write(&links, moved).unwrap();
    let (code, refused) = in_dir(store, home.path(), dir.path(), &["get"], &[]);
    assert_eq!(code, Some(1));
    assert_eq!(refused.pointer("/error/code"), Some(&json!("conflict")));
    assert_eq!(
        refused.pointer("/error/details/next"),
        Some(&json!("ployz link"))
    );
}
