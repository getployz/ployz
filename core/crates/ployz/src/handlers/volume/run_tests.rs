use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ployz_store::{Actor, ConfigStore, OrganizationId, SealingKey};
use serde_json::{Value, json};

use super::*;
use crate::cloud_login::{Secret, tests::fake_cloud};

const VOLUME: &str = "11111111-1111-4111-8111-111111111111";
const RUN: &str = "22222222-2222-4222-8222-222222222222";
const ENVIRONMENT: &str = "33333333-3333-4333-8333-333333333333";

fn token(cloud: &str) -> Credential {
    Credential::Token {
        cloud: cloud.to_owned(),
        token: Secret::new("ployz_secret".to_owned()),
    }
}

fn volume() -> VolumeId {
    VolumeId::parse(VOLUME.to_owned()).unwrap()
}

fn environment() -> EnvironmentId {
    EnvironmentId::parse(ENVIRONMENT.to_owned()).unwrap()
}

fn named() -> RunEnvironment {
    RunEnvironment {
        project: ProjectName::parse("shop".to_owned()).unwrap(),
        environment: EnvironmentName::parse("production".to_owned()).unwrap(),
    }
}

fn run(state: &str, kind: &str, args: Value) -> Value {
    json!({
        "id": RUN, "volume_id": VOLUME, "volume_name": "data", "kind": kind, "args": args,
        "orphan": false, "state": state, "message": null,
        "created_at": "2026-10-07T18:00:00.000Z", "updated_at": "2026-10-07T18:00:00.000Z",
        "finished_at": null,
    })
}

fn parsed(args: &[&str]) -> Result<ArgMatches, clap::Error> {
    crate::cli::command().try_get_matches_from(args.iter().copied())
}

fn path(matches: &ArgMatches) -> String {
    super::super::super::command_path(matches)
}

#[test]
fn each_volume_run_command_parses_to_its_own_handler() {
    let mirror = parsed(&[
        "ployz", "volume", "mirror", "data", "--to", "web-2", "--wait",
    ])
    .unwrap();
    assert_eq!(path(&mirror), "volume mirror");
    let leaf = leaf_matches(&mirror);
    assert_eq!(
        leaf.get_one::<String>("to").map(String::as_str),
        Some("web-2")
    );
    assert!(leaf.get_flag("wait"));

    let remove = parsed(&[
        "ployz",
        "volume",
        "mirror",
        "rm",
        "data-web-2",
        "--confirm",
        "data",
    ])
    .unwrap();
    assert_eq!(path(&remove), "volume mirror rm");
    let leaf = leaf_matches(&remove);
    assert_eq!(
        leaf.get_one::<String>("mirror").map(String::as_str),
        Some("data-web-2")
    );
    assert_eq!(
        leaf.get_one::<String>("confirm").map(String::as_str),
        Some("data")
    );

    let sync = parsed(&["ployz", "volume", "sync", "data", "--full"]).unwrap();
    assert_eq!(path(&sync), "volume sync");
    assert!(leaf_matches(&sync).get_flag("full"));

    let moved = parsed(&["ployz", "volume", "move", "data", "--to", "web-2"]).unwrap();
    assert_eq!(path(&moved), "volume move");
    assert_eq!(
        leaf_matches(&moved)
            .get_one::<String>("to")
            .map(String::as_str),
        Some("web-2")
    );

    let released = parsed(&["ployz", "volume", "release", "data", "--wait"]).unwrap();
    assert_eq!(path(&released), "volume release");
    assert!(leaf_matches(&released).get_flag("wait"));

    let restored = parsed(&["ployz", "volume", "restore", "data", "--from", "web-2"]).unwrap();
    assert_eq!(path(&restored), "volume restore");
    assert_eq!(
        leaf_matches(&restored)
            .get_one::<String>("from")
            .map(String::as_str),
        Some("web-2")
    );

    let runs = parsed(&["ployz", "volume", "runs", "data", RUN]).unwrap();
    assert_eq!(path(&runs), "volume runs");
    assert_eq!(
        leaf_matches(&runs)
            .get_one::<String>("run")
            .map(String::as_str),
        Some(RUN)
    );

    for words in [
        "mirror",
        "mirror rm",
        "sync",
        "move",
        "release",
        "restore",
        "runs",
    ] {
        assert!(super::super::handler(words).is_some(), "volume {words}");
    }
}

#[test]
fn a_mirror_names_its_server_and_a_removal_names_its_mirror() {
    let error = parsed(&["ployz", "volume", "mirror", "data"]).unwrap_err();
    assert!(error.to_string().contains("--to"), "{error}");
    let error = parsed(&["ployz", "volume", "mirror", "rm"]).unwrap_err();
    assert!(error.to_string().contains("VOLUME-SERVER"), "{error}");
    assert!(parsed(&["ployz", "volume", "sync", "data", "--to", "web-2"]).is_err());
    let error = parsed(&["ployz", "volume", "sync", "data", "--wait"]).unwrap_err();
    assert!(error.to_string().contains("--full"), "{error}");
    let error = parsed(&["ployz", "volume", "move", "data"]).unwrap_err();
    assert!(error.to_string().contains("--to"), "{error}");
    assert!(parsed(&["ployz", "volume", "release", "data", "--to", "web-2"]).is_err());
    let error = parsed(&["ployz", "volume", "restore", "data"]).unwrap_err();
    assert!(error.to_string().contains("--from"), "{error}");
    assert!(VolumeRunId::parse("not-a-run").is_err());
}

#[test]
fn a_mirror_name_splits_at_the_one_volume_it_starts_with() {
    let names = |names: &[&str]| -> Vec<VolumeName> {
        names
            .iter()
            .map(|name| VolumeName::parse(*name).unwrap())
            .collect()
    };
    let (volume, server) = mirror_of(&names(&["data", "logs"]), "data-web-2").unwrap();
    assert_eq!((volume.as_str(), server.as_str()), ("data", "web-2"));
    let (volume, server) = mirror_of(&names(&["my-data", "logs"]), "my-data-fsn-2").unwrap();
    assert_eq!((volume.as_str(), server.as_str()), ("my-data", "fsn-2"));

    let error = mirror_of(&names(&["data", "data-web"]), "data-web-2").unwrap_err();
    assert_eq!(error.report().code, RpcErrorCode::Ambiguous);
    let error = mirror_of(&names(&["logs"]), "data-web-2").unwrap_err();
    assert_eq!(error.report().code, RpcErrorCode::NotFound);
    assert_eq!(error.hints(), [Hint::valid(["logs"])]);
    let error = mirror_of(&names(&["data"]), "data").unwrap_err();
    assert_eq!(error.report().code, RpcErrorCode::NotFound);
}

#[test]
fn the_in_process_store_has_no_volume_runs() {
    let key = SealingKey::new(&[7; 32]).unwrap();
    let local = Backend::Local(
        Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
        Actor::system(OrganizationId::parse("local").unwrap()),
    );
    let Err(error) = cloud(&local) else {
        panic!("the in-process Store has no Volume runs");
    };
    let error = error.report();
    assert_eq!(error.code, RpcErrorCode::Unsupported);
    assert!(
        error.message.contains("need Ployz Cloud"),
        "{}",
        error.message
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn each_kind_posts_only_its_own_fields() {
    let cases = [
        (
            RunRequest {
                environment: named(),
                ask: RunAsk::Mirror {
                    to: MachineName::parse("web-2").unwrap(),
                },
            },
            json!({ "environment": { "project": "shop", "environment": "production" }, "kind": "mirror", "to": "web-2" }),
            json!({ "to": "web-2" }),
        ),
        (
            RunRequest {
                environment: named(),
                ask: RunAsk::Sync { full: Full },
            },
            json!({ "environment": { "project": "shop", "environment": "production" }, "kind": "sync", "full": true }),
            json!({ "full": true }),
        ),
        (
            RunRequest {
                environment: named(),
                ask: RunAsk::DeleteMirror {
                    slot: MachineName::parse("web-2").unwrap(),
                    confirm: Some("data".to_owned()),
                },
            },
            json!({ "environment": { "project": "shop", "environment": "production" }, "kind": "delete_mirror", "slot": "web-2", "confirm": "data" }),
            json!({ "slot": "web-2", "confirmed_name": "data" }),
        ),
        (
            RunRequest {
                environment: named(),
                ask: RunAsk::Move {
                    to: MachineName::parse("web-2").unwrap(),
                },
            },
            json!({ "environment": { "project": "shop", "environment": "production" }, "kind": "move", "to": "web-2" }),
            json!({ "to": "web-2" }),
        ),
        (
            RunRequest {
                environment: named(),
                ask: RunAsk::Release,
            },
            json!({ "environment": { "project": "shop", "environment": "production" }, "kind": "release" }),
            json!({}),
        ),
        (
            RunRequest {
                environment: named(),
                ask: RunAsk::Restore {
                    from: MachineName::parse("web-2").unwrap(),
                },
            },
            json!({ "environment": { "project": "shop", "environment": "production" }, "kind": "restore", "from": "web-2" }),
            json!({ "from": "web-2" }),
        ),
    ];
    for (request, body, args) in cases {
        let kind = body["kind"].as_str().unwrap().to_owned();
        let reply = run("requested", &kind, args);
        let expected = body.clone();
        let cloud = fake_cloud(move |route, sent| {
            assert_eq!(route, format!("POST /api/cli/volumes/{VOLUME}/runs"));
            let sent: Value = serde_json::from_str(sent).unwrap();
            assert_eq!(sent, expected);
            (200, json!({ "run": reply.clone() }))
        });
        let started = start(&token(&cloud), &volume(), &request, None)
            .await
            .unwrap();
        assert_eq!(started.id.as_str(), RUN);
        assert_eq!(started.state, MaybeKnown::Known(VolumeRunState::Requested));
        assert_eq!(serde_json::to_value(started.kind).unwrap(), json!(kind));
    }
}

#[tokio::test]
async fn a_refused_run_keeps_cloud_s_refusal() {
    let cloud = fake_cloud(|_, _| {
        (
            409,
            json!({ "error": { "code": "conflict", "message": "Volume data has run 2222 in progress", "details": null } }),
        )
    });
    let request = RunRequest {
        environment: named(),
        ask: RunAsk::Sync { full: Full },
    };
    let error = start(&token(&cloud), &volume(), &request, None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, StoreCallError::Refused(refusal) if refusal.code == RpcErrorCode::Conflict && refusal.message.contains("in progress")),
        "{error:?}"
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn runs_are_read_by_volume_and_environment_and_one_by_its_id() {
    let cloud = fake_cloud(|route, _| match route {
        r if r == format!("GET /api/cli/volumes/{VOLUME}/runs?environment={ENVIRONMENT}") => (
            200,
            json!({ "runs": [run("done", "mirror", json!({ "to": "web-2" }))] }),
        ),
        r if r == format!("GET /api/cli/volume-runs/{RUN}") => (
            200,
            json!({ "run": run("failed", "sync", json!({ "full": true })) }),
        ),
        other => panic!("unexpected {other}"),
    });
    let credential = token(&cloud);
    let runs = list(&credential, &volume(), &environment()).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(asked(&runs[0].args), "to web-2");
    let one = get(&credential, &VolumeRunId::parse(RUN).unwrap())
        .await
        .unwrap();
    assert_eq!(one.state, MaybeKnown::Known(VolumeRunState::Failed));
    assert_eq!(asked(&one.args), "full");
}

#[tokio::test]
async fn wait_polls_the_run_until_it_ends() {
    let polls = Arc::new(AtomicUsize::new(0));
    let seen = polls.clone();
    let cloud = fake_cloud(move |route, _| {
        assert_eq!(route, format!("GET /api/cli/volume-runs/{RUN}"));
        let state = match seen.fetch_add(1, Ordering::SeqCst) {
            0 => "running",
            _ => "done",
        };
        (
            200,
            json!({ "run": run(state, "sync", json!({ "full": false })) }),
        )
    });
    let requested: VolumeRun =
        serde_json::from_value(run("requested", "sync", json!({ "full": false }))).unwrap();
    let ended = settle(&token(&cloud), requested).await.unwrap();
    assert_eq!(ended.state, MaybeKnown::Known(VolumeRunState::Done));
    assert_eq!(polls.load(Ordering::SeqCst), 2);
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[test]
fn a_run_that_did_not_finish_is_the_command_s_failure() {
    let mut failed: VolumeRun =
        serde_json::from_value(run("failed", "mirror", json!({ "to": "web-2" }))).unwrap();
    failed.message = Some("data-web-2 diverged".to_owned());
    let follow = format!("ployz volume runs data {RUN}");
    let error = ended(&failed, "Mirror of Volume data onto web-2", follow.clone()).unwrap_err();
    assert_eq!(error.hints(), [Hint::Inspect(follow.clone())]);
    let error = error.report();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert!(
        error.message.ends_with("failed: data-web-2 diverged"),
        "{}",
        error.message
    );
    assert_eq!(error.details["run"]["id"], json!(RUN));

    failed.state = MaybeKnown::Known(VolumeRunState::Lost);
    let error = ended(&failed, "Mirror", follow).unwrap_err().report();
    assert_eq!(error.code, RpcErrorCode::Unavailable);
}

#[test]
fn a_yielded_run_says_a_deploy_took_its_volume() {
    let mut yielded: VolumeRun =
        serde_json::from_value(run("yielded", "move", json!({ "to": "web-2" }))).unwrap();
    assert_eq!(yielded.state, MaybeKnown::Known(VolumeRunState::Yielded));
    yielded.message = Some("a Deploy took the Volume".to_owned());
    let error = ended(&yielded, "Move", format!("ployz volume runs data {RUN}"))
        .unwrap_err()
        .report();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(
        error.message,
        "The Move gave way to a Deploy: a Deploy took the Volume"
    );
}

#[tokio::test]
async fn a_plain_sync_posts_its_environment_and_reads_back_the_lines() {
    let cloud = fake_cloud(|route, sent| {
        assert_eq!(route, format!("POST /api/cli/volumes/{VOLUME}/sync"));
        let sent: Value = serde_json::from_str(sent).unwrap();
        assert_eq!(
            sent,
            json!({ "environment": { "project": "shop", "environment": "production" } })
        );
        (
            200,
            json!({ "lines": ["data-web-2 is pulling w-3-1700000000"] }),
        )
    });
    let synced = sync_mirror(
        &token(&cloud),
        &volume(),
        &SyncRequest {
            environment: named(),
        },
    )
    .await
    .unwrap();
    assert_eq!(synced.lines, ["data-web-2 is pulling w-3-1700000000"]);
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_run_of_a_kind_or_state_this_cli_predates_still_lists_by_its_word() {
    let cloud = fake_cloud(|route, _| match route {
        r if r == format!("GET /api/cli/volumes/{VOLUME}/runs?environment={ENVIRONMENT}") => (
            200,
            json!({ "runs": [
                run("done", "seed", json!({})),
                run("paused_for_review", "move", json!({ "to": "web-2" })),
            ] }),
        ),
        other => panic!("unexpected {other}"),
    });
    let runs = list(&token(&cloud), &volume(), &environment())
        .await
        .unwrap();
    assert_eq!(runs[0].kind, MaybeKnown::Unknown("seed".to_owned()));
    assert_eq!(runs[0].state, MaybeKnown::Known(VolumeRunState::Done));
    assert_eq!(runs[1].kind, MaybeKnown::Known(VolumeRunKind::Move));
    assert_eq!(store::word(&runs[0].kind), "seed");
    assert_eq!(store::word(&runs[1].state), "paused for review");
    assert_eq!(runs[1].state.tone(), Tone::Muted);
    assert_eq!(
        serde_json::to_value(&runs[1]).unwrap()["state"],
        json!("paused_for_review")
    );

    let mut shown = Vec::new();
    record(&runs[0]).write(&mut shown, true).unwrap();
    let shown = String::from_utf8(shown).unwrap();
    assert!(shown.contains("seed"), "{shown}");
}

#[test]
fn a_wait_stops_on_a_state_this_cli_predates_and_names_it() {
    let paused: VolumeRun =
        serde_json::from_value(run("paused_for_review", "move", json!({ "to": "web-2" }))).unwrap();
    assert!(paused.state.ended());
    let error = ended(&paused, "Move", format!("ployz volume runs data {RUN}"))
        .unwrap_err()
        .report();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(
        error.message,
        "The Move is paused for review: Cloud gave no reason"
    );
}

#[tokio::test]
async fn a_release_cloud_plans_for_20_seconds_still_starts() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cloud = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0; 8192];
        let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
            .await
            .unwrap();
        // The request is in, so the client's timeout is armed; only Cloud's planning
        // takes paused time, and the reply goes out on real time again.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::time::resume();
        let body = json!({ "run": run("requested", "release", json!({})) }).to_string();
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, reply.as_bytes()).await;
    });
    let release = RunRequest {
        environment: named(),
        ask: RunAsk::Release,
    };
    let started = start(&token(&cloud), &volume(), &release, None).await;
    server.await.unwrap();
    assert_eq!(started.unwrap().id.as_str(), RUN);
}
