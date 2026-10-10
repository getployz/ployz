#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Git-backed Services through the public interface, on SQLite and on Postgres (see
//! `backend`): a repository and branch are only accepted with Cloud's evidence.

use ployz_core::config::ServiceGitAccess;
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, AuthorizedRepository, BuildLogQuery, BuildReport, BuildStatus, Change, Command,
    ConfigStore, CreateGitService, CreateProject, CreateService, Deploy, DeploymentId, DiffQuery,
    Edit, EnvironmentId, EnvironmentQuery, EnvironmentRef, OrganizationId, ProjectId, ProjectName,
    Publish, Query, Retry, RunEvidence, RunnerId, ServiceLineageId, ServiceQuery, SettingPath,
    SourceKind, Trusted, View, Written,
};
use ployz_store::{
    BuildOrder, BuildOrderQuery, Builder, GithubBuildId, GithubClaims, GithubEnd, GithubGrant,
    GithubReport, GithubRun, SetBuildOrder,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod backend;

const SERVICE: &str = "00000000-0000-4000-8000-000000000003";

fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    (store, who)
}

/// Cloud's evidence: `acme/web` through installation 7 with `main` and `dev`, and the
/// public `acme/docs` with only `main`.
fn evidence() -> Trusted {
    Trusted {
        repositories: vec![
            AuthorizedRepository {
                repository: backend::repo_name("acme/web"),
                repository_id: backend::repo_id(11),
                access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
                default_branch: backend::git_branch("main"),
                branches: vec![backend::git_branch("dev")],
            },
            AuthorizedRepository {
                repository: backend::repo_name("acme/docs"),
                repository_id: backend::repo_id(12),
                access: ServiceGitAccess::Public,
                default_branch: backend::git_branch("main"),
                branches: Vec::new(),
            },
        ],
        ..Trusted::default()
    }
}

fn create(repository: &str, branch: Option<&str>) -> CreateGitService {
    CreateGitService {
        id: ServiceLineageId::parse(SERVICE).unwrap(),
        environment: EnvironmentRef::default(),
        name: ServiceName::parse("web").unwrap(),
        repository: backend::repo_name(repository),
        branch: branch.map(backend::git_branch),
    }
}

fn edit(changes: Vec<Change>) -> Command {
    Command::Edit(Edit {
        environment: EnvironmentRef::default(),
        expect: None,
        changes,
    })
}

fn set(path: &str, value: Value) -> Change {
    Change::Set {
        path: SettingPath::parse(path).unwrap(),
        value,
    }
}

fn values(store: &ConfigStore, who: &Actor) -> Value {
    let query = EnvironmentQuery {
        environment: EnvironmentRef::default(),
        path: Some(SettingPath::parse("web").unwrap()),
        all: false,
    };
    json!(store.read(who, &query).unwrap().values)
}

#[test]
fn a_repository_needs_cloud_evidence() {
    let (store, who) = shop();
    let error = store
        .write_trusted(&who, &create("acme/web", None), &Trusted::default())
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
    assert_eq!(error.details["next"], "ployz github connect");

    let error = store
        .write_trusted(&who, &create("acme/secret", None), &evidence())
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
    assert!(!error.message.contains("secret"), "{error:?}");

    let malformed = json!({
        "command": "create_git_service", "id": SERVICE, "name": "web",
        "repository": "not a repo!",
    });
    let error = serde_json::from_value::<Command>(malformed).unwrap_err();
    assert!(!error.to_string().contains("not a repo"), "{error}");

    let error = store
        .write_trusted(&who, &create("acme/web", Some("nope-branch")), &evidence())
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
    assert_eq!(error.details["next"], "ployz github ls acme/web");
    assert!(!error.message.contains("nope-branch"), "{error:?}");

    // A caller can't claim an installation: the command has no field for it.
    let forged = json!({
        "command": "create_git_service", "id": SERVICE, "name": "web",
        "repository": "acme/web", "installation_id": 7,
    });
    assert!(serde_json::from_value::<Command>(forged).is_err());
}

#[test]
fn a_git_service_round_trips_get_edit_publish() {
    let (store, who) = shop();
    let created = store
        .write_trusted(&who, &create("ACME/Web", None), &evidence())
        .unwrap();
    let staged = created
        .staged
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    assert!(staged.contains(&"web.branch".to_owned()) && !staged.contains(&"web.image".to_owned()));
    assert_eq!(
        values(&store, &who),
        json!({
            "repository": "acme/web", "branch": "main", "buildMethod": "railpack",
            "maxRetries": 10, "privateDns": "web", "replicas": 1, "restartPolicy": "unless-stopped",
            "autoDeploy": true, "waitForCi": false, "watchPaths": [],
        })
    );
    // A replay needs no fresh evidence: it returns what the first create wrote.
    let replayed = store
        .write_trusted(
            &who,
            &Command::CreateGitService(create("ACME/Web", None)),
            &Trusted::default(),
        )
        .unwrap();
    assert_eq!(replayed, Written::Service(created));

    // Branches are checked only through evidence; other Settings need none.
    let error = store
        .write_trusted(
            &who,
            &edit(vec![set("web.branch", json!("dev"))]),
            &Trusted::default(),
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
    let patch = Change::Patch {
        path: SettingPath::parse("web").unwrap(),
        value: json!({
            "branch": "dev", "rootDir": "/apps/web/", "buildMethod": "dockerfile",
            "dockerfilePath": "web.Dockerfile", "buildCommand": "make",
        }),
    };
    store
        .write_trusted(&who, &edit(vec![patch]), &evidence())
        .unwrap();
    let after = values(&store, &who);
    assert_eq!(after["branch"], "dev");
    assert_eq!(after["rootDir"], "/apps/web");
    assert_eq!(after["dockerfilePath"], "web.Dockerfile");
    let again = Change::Patch {
        path: SettingPath::parse("web").unwrap(),
        value: after.clone(),
    };
    let Written::Edited(edited) = store
        .write_trusted(&who, &edit(vec![again]), &Trusted::default())
        .unwrap()
    else {
        panic!("an edit")
    };
    assert!(edited.staged.is_empty(), "sending get back changes nothing");

    // Diff names Settings and shows values as `get` does, never the stored shape.
    let diff = store.read(&who, &DiffQuery::default()).unwrap();
    let branch = diff.changes[0]
        .settings
        .iter()
        .find(|row| row.path == "web.branch")
        .unwrap();
    assert_eq!(
        (&branch.before, &branch.after),
        (&json!("main"), &json!("dev"))
    );

    // Moving to a repository without the branch falls back to its default branch.
    store
        .write_trusted(
            &who,
            &edit(vec![set("web.repository", json!("acme/docs"))]),
            &evidence(),
        )
        .unwrap();
    let after = values(&store, &who);
    assert_eq!(
        (&after["repository"], &after["branch"]),
        (&json!("acme/docs"), &json!("main"))
    );
    for path in [
        "web.rootDir",
        "web.buildMethod",
        "web.dockerfilePath",
        "web.buildCommand",
    ] {
        let unset = Change::Unset {
            path: SettingPath::parse(path).unwrap(),
        };
        store
            .write_trusted(&who, &edit(vec![unset]), &evidence())
            .unwrap();
    }
    for (path, code) in [
        ("web.repository", RpcErrorCode::InvalidArgument),
        ("web.image", RpcErrorCode::InvalidArgument),
    ] {
        let error = store
            .write_trusted(
                &who,
                &edit(vec![set(path, json!("acme/web:1"))]),
                &evidence(),
            )
            .unwrap_err();
        assert_eq!(error.code, code, "{path}");
    }

    let diff = store.read(&who, &DiffQuery::default()).unwrap();
    let rows = &diff.changes[0].settings;
    let source = rows.iter().find(|row| row.path == "web.source").unwrap();
    assert_eq!(source.after["repository"], json!("acme/docs"));
    let published = store
        .write_trusted(
            &who,
            &Publish {
                environment: EnvironmentRef::default(),
                version: Some(diff.version),
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &Trusted::default(),
        )
        .unwrap();
    assert!(published.saved.is_some_and(|saved| saved.0 >= 1));
    assert!(store.read(&who, &DiffQuery::default()).unwrap().published);
}

fn admit(store: &ConfigStore, who: &Actor, n: u8, services: &[&str]) -> DeploymentId {
    let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap();
    store
        .write_trusted(
            who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: services
                    .iter()
                    .map(|name| ServiceName::parse(*name).unwrap())
                    .collect(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    id
}

fn pins(commit: &ployz_store::CommitSha) -> BTreeMap<ServiceName, ployz_store::CommitSha> {
    BTreeMap::from([(ServiceName::parse("web").unwrap(), commit.clone())])
}

fn report(status: BuildStatus, message: Option<&str>, log: &str) -> RunEvidence {
    RunEvidence::Build(BuildReport {
        service: ServiceName::parse("web").unwrap(),
        status,
        message: message.map(Into::into),
        log: log.into(),
    })
}

#[test]
fn a_git_build_pins_its_commit_once_and_records_progress_log_and_receipt() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &create("acme/web", None), &evidence())
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000004").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("api").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    let first = admit(&store, &who, 1, &[]);

    // Only Git Services have a source to pin; Cloud reads it at the pinned commit.
    let sources = store.sources(&first).unwrap();
    assert_eq!(sources.len(), 1);
    let source = &sources[0];
    assert_eq!(
        (source.service.as_str(), source.repository.as_str()),
        ("web", "acme/web")
    );
    assert_eq!(source.repository_id.get(), 11);
    assert_eq!(
        source.access,
        ServiceGitAccess::GithubInstallation { installation_id: 7 }
    );
    assert_eq!(
        (source.branch.clone(), source.commit.clone()),
        (Some(backend::git_branch("main")), None)
    );

    // A pin never moves: the branch moving on changes nothing for this Deployment.
    let a = backend::sha(&"a".repeat(40));
    let b = backend::sha(&"b".repeat(40));
    assert_eq!(
        store.pin(&first, &pins(&a)).unwrap()[0].commit,
        Some(a.clone())
    );
    assert_eq!(
        store.pin(&first, &pins(&b)).unwrap()[0].commit,
        Some(a.clone())
    );
    let other = BTreeMap::from([(ServiceName::parse("api").unwrap(), a.clone())]);
    let error = store.pin(&first, &other).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);

    // Its runner gets the pin, and reports the build as it goes.
    let runner = RunnerId::parse("cloud-1").unwrap();
    let claimed = store.claim(&first, &runner).unwrap();
    assert_eq!(claimed.sources[0].commit, Some(a.clone()));
    let other = RunnerId::parse("cloud-2").unwrap();
    let error = store
        .record(&first, &other, report(BuildStatus::Building, None, "x"))
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    store
        .record(
            &first,
            &runner,
            report(BuildStatus::Building, None, "#1 FROM node\n"),
        )
        .unwrap();
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: first.clone() })
        .unwrap();
    assert_eq!(
        json!(view.builds),
        json!([{"service": "web", "commit": a, "status": "building", "message": null}])
    );
    store
        .record(
            &first,
            &runner,
            report(BuildStatus::Failed, Some("npm ci failed"), "exit 1\n"),
        )
        .unwrap();
    let View::BuildLog(log) = store
        .read(
            &who,
            &Query::BuildLog(BuildLogQuery {
                deployment: first.clone(),
                service: ServiceName::parse("web").unwrap(),
            }),
        )
        .unwrap()
    else {
        panic!("a build log")
    };
    assert_eq!(log.log, "#1 FROM node\nexit 1\n");
    assert_eq!(
        (log.build.status, log.build.message.as_deref()),
        (BuildStatus::Failed, Some("npm ci failed"))
    );
    let stranger = Actor::system(OrganizationId::parse("other").unwrap());
    let query = BuildLogQuery {
        deployment: first.clone(),
        service: ServiceName::parse("web").unwrap(),
    };
    assert_eq!(
        store.read(&stranger, &query).unwrap_err().code,
        RpcErrorCode::NotFound
    );

    // A build that succeeded leaves its receipt for the next Deployment to reuse.
    let receipt = json!({"fingerprint": "f".repeat(64)});
    store
        .record(
            &first,
            &runner,
            RunEvidence::Built(BTreeMap::from([(
                ServiceName::parse("web").unwrap(),
                receipt.clone(),
            )])),
        )
        .unwrap();
    store
        .record(
            &first,
            &runner,
            RunEvidence::NotExecuted("Build failed".into()),
        )
        .unwrap();
    assert_eq!(
        store.pin(&first, &pins(&b)).unwrap_err().code,
        RpcErrorCode::Conflict
    );

    // Retrying it builds the commit it pinned, wherever the branch is now.
    let again = DeploymentId::parse("00000000-0000-4000-8000-000000000199").unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: again.clone(),
                deployment: first.clone(),
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    assert_eq!(store.sources(&again).unwrap()[0].commit, Some(a.clone()));

    // A new Deployment pins afresh and gets the receipt as a hint.
    let retry = admit(&store, &who, 2, &[]);
    assert_eq!(store.sources(&retry).unwrap()[0].commit, None);
    store.pin(&retry, &pins(&b)).unwrap();
    let claimed = store.claim(&retry, &runner).unwrap();
    assert_eq!(
        claimed.receipts[&ServiceName::parse("web").unwrap()],
        [receipt]
    );

    // A Deployment of other Services builds nothing.
    let targeted = admit(&store, &who, 3, &["api"]);
    assert!(store.sources(&targeted).unwrap().is_empty());
}

fn build_order(store: &ConfigStore, who: &Actor, order: Option<BuildOrder>) -> Vec<Builder> {
    let written = store
        .write(
            who,
            &Command::SetBuildOrder(SetBuildOrder { build_order: order }),
        )
        .unwrap();
    let Written::BuildOrder(view) = written else {
        panic!("{written:?}")
    };
    assert_eq!(view.build_order, order);
    view.builders
}

#[test]
fn the_build_order_and_preferred_builder_apply_at_once_and_shape_each_walk() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &create("acme/web", None), &evidence())
        .unwrap();

    // Auto: GitHub first, skipped at once where the repository has no build workflow.
    let View::BuildOrder(auto) = store
        .read(&who, &Query::BuildOrder(BuildOrderQuery::default()))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(auto.build_order, None);
    assert_eq!(auto.builders, [Builder::Github, Builder::Servers]);
    assert_eq!(
        build_order(&store, &who, Some(BuildOrder::ServersThenGithub)),
        [Builder::Servers, Builder::Github]
    );

    // The Preferred Builder is immediate: no Working State change, nothing to publish.
    let Written::Edited(edited) = store
        .write(
            &who,
            &edit(vec![set("web.preferredBuilder", json!("github"))]),
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(edited.immediate.len(), 1);
    assert!(edited.staged.is_empty());
    assert_eq!(values(&store, &who)["preferredBuilder"], "github");
    for bad in [json!("gitlab"), json!(3)] {
        let error = store
            .write(&who, &edit(vec![set("web.preferredBuilder", bad)]))
            .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        assert!(!error.message.contains("gitlab"), "{error:?}");
    }

    let deployment = admit(&store, &who, 1, &[]);
    let source = &store.sources(&deployment).unwrap()[0];
    assert_eq!(source.builders, [Builder::Github, Builder::Servers]);
    assert_eq!(source.preferred_machine, None);

    // A Preferred Server goes first; the walk is read as the build starts.
    let machine = "0123456789abcdef0123456789abcdef";
    store
        .write(
            &who,
            &edit(vec![set("web.preferredBuilder", json!(machine))]),
        )
        .unwrap();
    build_order(&store, &who, Some(BuildOrder::GithubOnly));
    let source = &store.sources(&deployment).unwrap()[0];
    assert_eq!(source.builders, [Builder::Servers, Builder::Github]);
    assert_eq!(
        source.preferred_machine.as_ref().unwrap().to_string(),
        machine
    );

    store
        .write(
            &who,
            &edit(vec![Change::Unset {
                path: SettingPath::parse("web.preferredBuilder").unwrap(),
            }]),
        )
        .unwrap();
    assert!(values(&store, &who).get("preferredBuilder").is_none());
    assert_eq!(
        store.sources(&deployment).unwrap()[0].builders,
        [Builder::Github]
    );
    assert_eq!(
        build_order(&store, &who, None),
        [Builder::Github, Builder::Servers]
    );
}

const RUN: u64 = 555;
const WORKFLOW: &str = "acme/web/.github/workflows/ployz-build.yml@refs/heads/main";

fn github_run(run_id: u64) -> GithubRun {
    GithubRun {
        run_id,
        run_url: format!("https://github.com/acme/web/actions/runs/{run_id}"),
        workflow_ref: WORKFLOW.into(),
        repository: backend::repo_name("acme/web"),
        installation_id: 7,
    }
}

fn claims() -> GithubClaims {
    GithubClaims {
        repository_id: "11".into(),
        job_workflow_ref: WORKFLOW.into(),
        run_id: RUN.to_string(),
        event_name: "workflow_dispatch".into(),
    }
}

fn grant() -> GithubGrant {
    GithubGrant {
        id: "grant-1".into(),
        machine: ployz_core::MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        fingerprint: "f".repeat(64),
    }
}

fn lines(from: u64, lines: &[&str], platforms: Option<&[&str]>) -> GithubReport {
    GithubReport {
        from,
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
        ended: platforms.map(|platforms| match platforms {
            [] => ployz_store::RunEnd::Failed,
            platforms => ployz_store::RunEnd::Built {
                platforms: platforms.iter().map(|p| (*p).to_owned()).collect(),
            },
        }),
    }
}

/// A pinned Git build of a fresh Deployment, handed to GitHub run `RUN`.
fn dispatched(store: &ConfigStore, who: &Actor) -> GithubBuildId {
    store
        .write_trusted(who, &create("acme/web", None), &evidence())
        .unwrap();
    let deployment = admit(store, who, 1, &[]);
    store
        .pin(&deployment, &pins(&backend::sha(&"a".repeat(40))))
        .unwrap();
    let id = GithubBuildId::parse(&format!("{deployment}.web")).unwrap();
    store.github_dispatched(&id, &github_run(RUN)).unwrap();
    // A retried dispatch step replays; another run can't take it.
    store.github_dispatched(&id, &github_run(RUN)).unwrap();
    let error = store
        .github_dispatched(&id, &github_run(RUN + 1))
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    id
}

#[test]
fn a_github_check_in_must_come_from_the_dispatched_repository_workflow_and_run() {
    let (store, who) = shop();
    let id = dispatched(&store, &who);
    let build = store.github_authorize(&id, &claims()).unwrap();
    assert_eq!(build.status, BuildStatus::Building);
    assert_eq!(build.grant, None);

    for (claims, words) in [
        (
            GithubClaims {
                repository_id: "12".into(),
                ..claims()
            },
            "another repository",
        ),
        (
            GithubClaims {
                job_workflow_ref: WORKFLOW.replace("main", "evil").clone(),
                ..claims()
            },
            "another workflow",
        ),
        (
            GithubClaims {
                run_id: "1".into(),
                ..claims()
            },
            "another run",
        ),
        (
            GithubClaims {
                event_name: "push".into(),
                ..claims()
            },
            "not dispatched",
        ),
    ] {
        let error = store.github_authorize(&id, &claims).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::Unauthenticated);
        assert!(error.message.contains(words), "{error:?}");
    }
    let unknown = GithubBuildId::parse(&format!("{}.api", id.deployment)).unwrap();
    assert_eq!(
        store
            .github_authorize(&unknown, &claims())
            .unwrap_err()
            .code,
        RpcErrorCode::NotFound
    );
    assert_eq!(
        GithubBuildId::parse("not-a-build").unwrap_err().code,
        RpcErrorCode::NotFound
    );

    // Only the runner gets the inputs: the lowering input and the pinned commit.
    let (input, commit, receipt) = store.github_input(&id).unwrap();
    assert_eq!(commit.as_str(), "a".repeat(40));
    assert!(input.get("snapshots").is_some());
    assert_eq!(receipt, None);
}

#[test]
fn a_github_build_checks_in_once_takes_each_log_line_once_and_refuses_late_reports() {
    let (store, who) = shop();
    let id = dispatched(&store, &who);

    // No report before check-in.
    let error = store
        .github_report(&id, RUN, &lines(0, &["early\n"], None))
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);

    store.github_check_in(&id, RUN, &grant()).unwrap();
    let error = store.github_check_in(&id, RUN, &grant()).unwrap_err();
    assert_eq!(
        error.code,
        RpcErrorCode::Conflict,
        "a second check-in is refused"
    );
    let build = store.github_build(&id).unwrap();
    assert_eq!(build.grant, Some(grant()));
    assert!(build.checked_in_at.is_some());

    assert_eq!(
        store
            .github_report(&id, RUN, &lines(0, &["one\n", "two\n"], None))
            .unwrap(),
        2
    );
    // A retried batch repeating lines files only the new ones; a gap is refused.
    assert_eq!(
        store
            .github_report(&id, RUN, &lines(1, &["two\n", "three\n"], None))
            .unwrap(),
        3
    );
    let error = store
        .github_report(&id, RUN, &lines(5, &["six\n"], None))
        .unwrap_err();
    assert_eq!(error.details["received"], 3);
    assert_eq!(
        store
            .github_report(&id, RUN, &lines(3, &[], Some(&["linux/amd64"])))
            .unwrap(),
        3
    );
    // The final report came: a duplicate or late one is refused.
    let error = store
        .github_report(&id, RUN, &lines(3, &["late\n"], None))
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(
        store.github_build(&id).unwrap().ended,
        Some(ployz_store::RunEnd::Built {
            platforms: vec!["linux/amd64".to_owned()]
        })
    );

    let receipt = json!({ "fingerprint": "f".repeat(64), "machine_id": grant().machine });
    let ended = store
        .github_end(
            &id,
            Some(RUN),
            &GithubEnd::Built {
                receipt: receipt.clone(),
            },
        )
        .unwrap();
    assert_eq!(ended, BuildStatus::Built);
    // Ending again, or the run failing afterwards, changes nothing.
    let failed = GithubEnd::Failed {
        message: "late".into(),
    };
    assert_eq!(
        store.github_end(&id, Some(RUN), &failed).unwrap_err().code,
        RpcErrorCode::Conflict
    );
    let log = store
        .read(
            &who,
            &BuildLogQuery {
                deployment: id.deployment.clone(),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(log.build.status, BuildStatus::Built);
    assert!(log.log.ends_with("one\ntwo\nthree\n"), "{}", log.log);
    // The receipt is the Service's latest: the runner delivers it without building.
    let source = &store.sources(&id.deployment).unwrap()[0];
    assert_eq!(source.status, Some(BuildStatus::Built));
}

#[test]
fn a_skipped_github_build_goes_back_to_the_next_builder_and_cancellation_lists_open_grants() {
    let (store, who) = shop();
    let id = dispatched(&store, &who);
    store.github_check_in(&id, RUN, &grant()).unwrap();
    assert_eq!(store.github_outstanding(&id.deployment).unwrap().len(), 1);
    // The start-within limit lost to the check-in: the run that started keeps it.
    let unstarted = GithubEnd::Unstarted {
        message: "no runner started the build in time".into(),
    };
    assert_eq!(
        store
            .github_end(&id, Some(RUN), &unstarted)
            .unwrap_err()
            .code,
        RpcErrorCode::Conflict
    );

    // GitHub failed it for its own reasons: pending again, with why, for the servers.
    let skip = GithubEnd::Skipped {
        message: "the run ran out of time".into(),
    };
    assert_eq!(
        store.github_end(&id, Some(RUN), &skip).unwrap(),
        BuildStatus::Pending
    );
    let source = &store.sources(&id.deployment).unwrap()[0];
    assert_eq!(source.status, Some(BuildStatus::Pending));
    assert_eq!(source.message.as_deref(), Some("the run ran out of time"));
    assert!(store.github_outstanding(&id.deployment).unwrap().is_empty());
    assert_eq!(
        store.github_check_in(&id, RUN, &grant()).unwrap_err().code,
        RpcErrorCode::Conflict,
        "a late check-in from the skipped run is refused"
    );

    // A cancelled Deployment wants no build: a new dispatch is refused.
    store
        .write(
            &who,
            &ployz_store::Cancel {
                deployment: id.deployment.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        store
            .github_dispatched(&id, &github_run(RUN + 1))
            .unwrap_err()
            .code,
        RpcErrorCode::Conflict
    );
}

fn source(store: &ConfigStore, who: &Actor) -> SourceKind {
    let query = ServiceQuery {
        environment: EnvironmentRef::default(),
        service: ServiceName::parse("web").unwrap(),
    };
    store.read(who, &query).unwrap().service.source
}

#[test]
fn a_source_disconnects_to_empty_and_an_empty_service_connects_a_repository() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(SERVICE).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    let unset = |path: &str| {
        edit(vec![Change::Unset {
            path: SettingPath::parse(path).unwrap(),
        }])
    };
    store.write(&who, &unset("web.image")).unwrap();
    assert_eq!(source(&store, &who), SourceKind::Empty);

    // Connecting a repository needs Cloud's evidence, then builds its default branch.
    let connect = edit(vec![set("web.repository", json!("acme/web"))]);
    let refused = store
        .write_trusted(&who, &connect, &Trusted::default())
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::NotFound);
    store.write_trusted(&who, &connect, &evidence()).unwrap();
    assert_eq!(source(&store, &who), SourceKind::Git);
    assert_eq!(values(&store, &who)["repository"], json!("acme/web"));
    assert_eq!(values(&store, &who)["branch"], json!("main"));

    store.write(&who, &unset("web.repository")).unwrap();
    assert_eq!(source(&store, &who), SourceKind::Empty);
    store
        .write(&who, &edit(vec![set("web.image", json!("nginx:2"))]))
        .unwrap();
    assert_eq!(source(&store, &who), SourceKind::Image);
}
