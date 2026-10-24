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
    Publish, Query, Retry, RunEvidence, RunnerId, SealingKey, ServiceLineageId, ServiceQuery,
    SettingPath, SourceKind, Trusted, View, Written,
};
use ployz_store::{
    BuildOrder, BuildOrderQuery, Builder, GithubBuild, GithubBuildId, GithubClaims, GithubEnd,
    GithubGrant, GithubHandOff, GithubReport, GithubRun, SetBuildOrder,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod backend;

const SERVICE: &str = "00000000-0000-4000-8000-000000000003";
const ENVIRONMENT: &str = "00000000-0000-4000-8000-000000000002";

fn shop() -> (ConfigStore, Actor) {
    project(backend::open())
}

/// Project `shop` in `store`, its default Environment [`ENVIRONMENT`].
fn project(store: ConfigStore) -> (ConfigStore, Actor) {
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(ENVIRONMENT).unwrap(),
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
    let error = store
        .write_trusted(
            &who,
            &edit(vec![set("web.repository", json!("acme/web:1"))]),
            &evidence(),
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);

    let diff = store.read(&who, &DiffQuery::default()).unwrap();
    let rows = &diff.changes[0].settings;
    let source = rows.iter().find(|row| row.path == "web.source").unwrap();
    assert_eq!(source.after["repository"], json!("acme/docs"));
    let published = store
        .write_trusted(
            &who,
            &Publish {
                id: None,
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

#[test]
fn a_new_git_service_diffs_with_the_repository_and_branch_it_builds() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &create("acme/web", Some("dev")), &evidence())
        .unwrap();
    let rows = |store: &ConfigStore| {
        let diff = store.read(&who, &DiffQuery::default()).unwrap();
        let rows = diff.changes[0]
            .settings
            .iter()
            .map(|row| {
                (
                    row.path.clone(),
                    row.before.clone(),
                    row.after.clone(),
                    row.can_restore,
                )
            })
            .collect::<Vec<_>>();
        (rows, diff.total_count)
    };
    assert_eq!(
        rows(&store),
        (
            vec![
                (
                    "web.repository".into(),
                    Value::Null,
                    json!("acme/web"),
                    false
                ),
                ("web.branch".into(), Value::Null, json!("dev"), false),
            ],
            3
        )
    );
    // A branch changed since it was added is that change, which discard takes.
    store
        .write_trusted(
            &who,
            &edit(vec![set("web.branch", json!("main"))]),
            &evidence(),
        )
        .unwrap();
    assert_eq!(
        rows(&store),
        (
            vec![
                (
                    "web.repository".into(),
                    Value::Null,
                    json!("acme/web"),
                    false
                ),
                ("web.branch".into(), json!("dev"), json!("main"), true),
            ],
            3
        )
    );
}

#[test]
fn push_policy_on_a_public_repository_applies_and_says_pushes_need_the_app() {
    let unheard = "Pushes reach Ployz only once the GitHub App is installed on acme/docs.";
    let notes = |store: &ConfigStore, who: &Actor| {
        let query = EnvironmentQuery {
            environment: EnvironmentRef::default(),
            path: Some(SettingPath::parse("web").unwrap()),
            all: false,
        };
        store
            .read(who, &query)
            .unwrap()
            .settings
            .into_iter()
            .filter_map(|row| Some((row.path.to_string(), row.note?)))
            .collect::<Vec<_>>()
    };
    let set_policy = |store: &ConfigStore, who: &Actor| {
        store
            .write_trusted(
                who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![
                        set("web.waitForCi", json!(true)),
                        set("web.watchPaths", json!(["src/**"])),
                    ],
                },
                &evidence(),
            )
            .unwrap()
    };

    let (store, who) = shop();
    store
        .write_trusted(&who, &create("acme/docs", None), &evidence())
        .unwrap();
    let edited = set_policy(&store, &who);
    assert_eq!(
        edited
            .immediate
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["web.waitForCi", "web.watchPaths"]
    );
    assert_eq!(
        serde_json::to_value(&edited.notes).unwrap(),
        json!([{ "path": "web.waitForCi", "note": unheard }])
    );
    assert_eq!(values(&store, &who)["waitForCi"], json!(true));
    assert_eq!(
        notes(&store, &who),
        [
            ("web.autoDeploy".to_owned(), unheard.to_owned()),
            ("web.waitForCi".to_owned(), unheard.to_owned()),
        ]
    );

    // Through the GitHub App, pushes arrive: nothing to say.
    let (store, who) = shop();
    store
        .write_trusted(&who, &create("acme/web", None), &evidence())
        .unwrap();
    let edited = set_policy(&store, &who);
    assert!(edited.notes.is_empty(), "{edited:?}");
    assert!(notes(&store, &who).is_empty());
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
                prune: false,
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
        server: None,
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
    let started_at = view.builds[0].started_at;
    assert!(started_at.is_some());
    assert_eq!(
        json!(view.builds),
        json!([{"service": "web", "commit": a, "status": "building", "message": null,
                "started_at": started_at, "finished_at": null, "server": null,
                "github_skipped": null}])
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
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
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

/// Deployment `id`, claimed by `runner`, applied `web`.
fn applied(store: &ConfigStore, id: &DeploymentId, runner: &RunnerId) {
    let operation = json!({"type": "remove_container", "machine_id": "a".repeat(32), "container_id": "a".repeat(64)});
    let preview = serde_json::from_value(json!({
        "namespace": "shop-production", "warnings": [], "would_remove": [], "preserved_volumes": [],
        "operations": [{"index": 0, "machine_id": "a".repeat(32), "service_name": "web",
                        "operation": operation, "status": {"type": "pending"}}],
    }))
    .unwrap();
    store.claim(id, runner).unwrap();
    store
        .record(id, runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome =
        serde_json::from_value(json!({"type": "success", "completed": [operation]})).unwrap();
    store
        .record(
            id,
            runner,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

#[test]
fn redeploying_an_older_applied_deployment_builds_the_commit_it_pinned() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &create("acme/web", None), &evidence())
        .unwrap();
    let runner = RunnerId::parse("cloud-1").unwrap();
    let a = backend::sha(&"a".repeat(40));
    let b = backend::sha(&"b".repeat(40));
    let first = admit(&store, &who, 1, &[]);
    store.pin(&first, &pins(&a)).unwrap();
    applied(&store, &first, &runner);
    let second = admit(&store, &who, 2, &[]);
    store.pin(&second, &pins(&b)).unwrap();
    applied(&store, &second, &runner);

    // The branch is at b now; redeploying #1 builds a, which #1 pinned.
    let back = DeploymentId::parse("00000000-0000-4000-8000-000000000199").unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: back.clone(),
                deployment: first.clone(),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    assert_eq!(store.sources(&back).unwrap()[0].commit, Some(a.clone()));
    assert_eq!(
        store.claim(&back, &runner).unwrap().sources[0].commit,
        Some(a)
    );
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
    assert_eq!(source.sealed, None);
    assert!(json!(source).get("sealed").is_none(), "{source:?}");

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

fn held(store: &ConfigStore, id: &GithubBuildId) -> GithubBuild {
    match store.github_build(id).unwrap() {
        GithubHandOff::Held(build) => *build,
        GithubHandOff::PassedOn { why } => panic!("GitHub passed it on: {why}"),
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
    let built = store.github_input(&id).unwrap();
    assert_eq!(built.commit.as_str(), "a".repeat(40));
    assert!(built.input.get("snapshots").is_some());
    assert_eq!(built.receipt, None);
    assert_eq!(built.sealed, None);
}

#[test]
fn a_github_input_without_sealed_values_does_not_look_up_the_built_service() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = project(ConfigStore::open(&url, backend::key()).unwrap());
    let id = dispatched(&store, &who);
    let expected = store.github_input(&id).unwrap();
    backend::execute(
        &url,
        "UPDATE config_build SET service = 'missing' WHERE service = 'web'",
    );
    let id = GithubBuildId::parse(&format!("{}.missing", id.deployment)).unwrap();
    assert_eq!(store.github_input(&id).unwrap(), expected);
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
    let build = held(&store, &id);
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
        held(&store, &id).ended,
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

#[test]
fn a_build_github_passed_on_at_check_in_reads_as_no_longer_on_github() {
    let (store, who) = shop();
    let id = dispatched(&store, &who);
    let skip = GithubEnd::Unstarted {
        message: "GitHub builds a sealed value only with an up-to-date Ployz".into(),
    };
    assert_eq!(
        store.github_end(&id, Some(RUN), &skip).unwrap(),
        BuildStatus::Pending
    );
    assert_eq!(
        store.github_build(&id).unwrap(),
        GithubHandOff::PassedOn {
            why: "GitHub builds a sealed value only with an up-to-date Ployz".into()
        }
    );
    assert_eq!(
        store.github_end(&id, Some(RUN), &skip).unwrap_err().code,
        RpcErrorCode::Conflict,
        "the walk's later look finds it already ended"
    );
    let source = &store.sources(&id.deployment).unwrap()[0];
    assert_eq!(
        (source.status, source.message.as_deref()),
        (
            Some(BuildStatus::Pending),
            Some("GitHub builds a sealed value only with an up-to-date Ployz")
        ),
        "the next Builder takes it, with why"
    );
}

#[test]
fn a_run_github_passed_on_holds_nothing_once_the_next_builder_starts() {
    for checked_in in [false, true] {
        let (store, who) = shop();
        let id = dispatched(&store, &who);
        let skip = if checked_in {
            store.github_check_in(&id, RUN, &grant()).unwrap();
            GithubEnd::Skipped {
                message: "the run ran out of time".into(),
            }
        } else {
            GithubEnd::Unstarted {
                message: "GitHub builds a sealed value only with an up-to-date Ployz".into(),
            }
        };
        store.github_end(&id, Some(RUN), &skip).unwrap();

        let runner = RunnerId::parse("cloud-1").unwrap();
        store.claim(&id.deployment, &runner).unwrap();
        store
            .record(
                &id.deployment,
                &runner,
                report(BuildStatus::Building, None, ""),
            )
            .unwrap();

        let refused = |result: Result<(), ployz_core::RpcError>, what: &str| {
            assert_eq!(
                result.unwrap_err().code,
                RpcErrorCode::Conflict,
                "{what} (checked in: {checked_in})"
            );
        };
        refused(store.github_check_in(&id, RUN, &grant()), "a late check-in");
        refused(
            store
                .github_report(&id, RUN, &lines(0, &["late\n"], None))
                .map(drop),
            "a late report",
        );
        refused(
            store
                .github_end(
                    &id,
                    Some(RUN),
                    &GithubEnd::Built {
                        receipt: json!({ "fingerprint": "f".repeat(64) }),
                    },
                )
                .map(drop),
            "a late Built",
        );
        refused(
            store
                .github_end(
                    &id,
                    Some(RUN),
                    &GithubEnd::Unstarted {
                        message: "no runner started the build in time".into(),
                    },
                )
                .map(drop),
            "a late Unstarted",
        );
        refused(
            store
                .github_end(
                    &id,
                    Some(RUN),
                    &GithubEnd::Failed {
                        message: "cancelled".into(),
                    },
                )
                .map(drop),
            "a late cancel",
        );
        assert!(store.github_outstanding(&id.deployment).unwrap().is_empty());
        assert!(
            matches!(
                store.github_build(&id).unwrap(),
                GithubHandOff::PassedOn { .. }
            ),
            "GitHub no longer reads as building it"
        );
        let source = &store.sources(&id.deployment).unwrap()[0];
        assert_eq!(
            source.status,
            Some(BuildStatus::Building),
            "the Server builds it"
        );
    }
}

#[test]
fn a_late_old_mint_skip_leaves_a_run_that_checked_in_building() {
    let (store, who) = shop();
    let id = dispatched(&store, &who);
    store.github_check_in(&id, RUN, &grant()).unwrap();
    let late = GithubEnd::Unstarted {
        message: "GitHub builds a sealed value only with an up-to-date Ployz".into(),
    };
    assert_eq!(
        store.github_end(&id, Some(RUN), &late).unwrap_err().code,
        RpcErrorCode::Conflict
    );
    let build = store.github_authorize(&id, &claims()).unwrap();
    assert_eq!(build.status, BuildStatus::Building);
    assert_eq!(build.grant, Some(grant()));
    assert_eq!(store.github_outstanding(&id.deployment).unwrap().len(), 1);
}

#[test]
fn a_build_clock_starts_at_building_clears_when_skipped_and_reopens_for_a_later_builder() {
    let (store, who) = shop();
    let id = dispatched(&store, &who);
    let clock = || {
        let build = store
            .read(
                &who,
                &BuildLogQuery {
                    deployment: id.deployment.clone(),
                    service: ServiceName::parse("web").unwrap(),
                },
            )
            .unwrap()
            .build;
        (build.status, build.started_at, build.finished_at)
    };
    let (status, started, finished) = clock();
    assert_eq!((status, finished), (BuildStatus::Building, None));
    assert!(started.is_some(), "dispatching to GitHub starts the clock");

    let skip = GithubEnd::Skipped {
        message: "the run ran out of time".into(),
    };
    store.github_end(&id, Some(RUN), &skip).unwrap();
    assert_eq!(
        clock(),
        (BuildStatus::Pending, None, None),
        "a build waiting for the next Builder has no clock"
    );

    let runner = RunnerId::parse("cloud-1").unwrap();
    store.claim(&id.deployment, &runner).unwrap();
    for status in [BuildStatus::Building, BuildStatus::Failed] {
        store
            .record(&id.deployment, &runner, report(status, None, ""))
            .unwrap();
    }
    let (status, again, finished) = clock();
    assert_eq!(status, BuildStatus::Failed);
    assert!(
        again.is_some() && again >= started,
        "the runner restarts the clock"
    );
    assert!(finished.is_some() && finished >= again);

    store
        .record(
            &id.deployment,
            &runner,
            report(BuildStatus::Building, None, ""),
        )
        .unwrap();
    assert_eq!(
        clock(),
        (BuildStatus::Building, again, None),
        "a later Builder reopens the clock and keeps its start"
    );
}

#[test]
fn a_build_names_the_server_that_built_it_and_why_github_passed_it_on() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &create("acme/web", None), &evidence())
        .unwrap();
    let deployment = admit(&store, &who, 1, &[]);
    store
        .pin(&deployment, &pins(&backend::sha(&"a".repeat(40))))
        .unwrap();
    let id = GithubBuildId::parse(&format!("{deployment}.web")).unwrap();
    let why = "acme/web has no .github/workflows/ployz-build.yml on its default branch";
    store
        .github_end(
            &id,
            None,
            &GithubEnd::Skipped {
                message: why.into(),
            },
        )
        .unwrap();

    let runner = RunnerId::parse("cloud-1").unwrap();
    store.claim(&deployment, &runner).unwrap();
    for (status, server) in [
        (BuildStatus::Building, None),
        (BuildStatus::Building, Some("machine-1")),
        (BuildStatus::Built, None),
    ] {
        store
            .record(
                &deployment,
                &runner,
                RunEvidence::Build(BuildReport {
                    service: ServiceName::parse("web").unwrap(),
                    status,
                    message: None,
                    log: String::new(),
                    server: server.map(Into::into),
                }),
            )
            .unwrap();
    }
    let build = store
        .read(
            &who,
            &BuildLogQuery {
                deployment,
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap()
        .build;
    assert_eq!(build.status, BuildStatus::Built);
    assert_eq!(
        build.server.as_deref(),
        Some("machine-1"),
        "a report that names no Server keeps the one that builds it"
    );
    assert_eq!(
        build.github_skipped.as_deref(),
        Some(why),
        "the reason GitHub passed it on outlives the Server's reports"
    );
}

#[test]
fn a_build_github_passed_on_stays_passed_on_for_its_deployment() {
    for (checked_in, why) in [
        (
            false,
            "GitHub builds a sealed value only with an up-to-date Ployz",
        ),
        (false, "no runner started the build in time"),
        (true, "the run ran out of time"),
    ] {
        let (store, who) = shop();
        let id = dispatched(&store, &who);
        let message = why.to_owned();
        let skip = if checked_in {
            store.github_check_in(&id, RUN, &grant()).unwrap();
            GithubEnd::Skipped { message }
        } else {
            GithubEnd::Unstarted { message }
        };
        store.github_end(&id, Some(RUN), &skip).unwrap();

        let refused = |result: Result<(), ployz_core::RpcError>, what: &str| {
            assert_eq!(
                result.unwrap_err().code,
                RpcErrorCode::Conflict,
                "{what} ({why})"
            );
        };
        refused(
            store.github_dispatched(&id, &github_run(RUN + 1)),
            "a retried start's new run",
        );
        refused(
            store.github_dispatched(&id, &github_run(RUN)),
            "the run it passed on",
        );
        refused(
            store
                .github_end(
                    &id,
                    None,
                    &GithubEnd::Skipped {
                        message: "GitHub couldn't take the build: rate limited".into(),
                    },
                )
                .map(drop),
            "a retried start's skip",
        );
        refused(
            store
                .github_end(
                    &id,
                    None,
                    &GithubEnd::Built {
                        receipt: json!({ "fingerprint": "f".repeat(64) }),
                    },
                )
                .map(drop),
            "a retried start's reuse",
        );

        assert!(store.github_outstanding(&id.deployment).unwrap().is_empty());
        assert_eq!(
            store.github_build(&id).unwrap(),
            GithubHandOff::PassedOn { why: why.into() }
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
        assert_eq!(
            (log.build.status, log.build.github_skipped.as_deref()),
            (BuildStatus::Pending, Some(why)),
            "the next Builder takes it, with why"
        );
        assert_eq!(log.log.matches("GitHub skipped").count(), 1, "{}", log.log);
    }
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

#[test]
fn setting_a_repository_or_an_image_switches_the_source_in_one_write() {
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

    // An image Service builds a repository's default branch from its root.
    store
        .write_trusted(
            &who,
            &edit(vec![set("web.repository", json!("acme/web"))]),
            &evidence(),
        )
        .unwrap();
    assert_eq!(source(&store, &who), SourceKind::Git);
    let switched = values(&store, &who);
    assert_eq!(
        (
            &switched["repository"],
            &switched["branch"],
            &switched["rootDir"]
        ),
        (&json!("acme/web"), &json!("main"), &Value::Null)
    );

    // A repository Service takes a root directory and build settings.
    store
        .write_trusted(
            &who,
            &edit(vec![
                set("web.rootDir", json!("/apps/web")),
                set("web.buildMethod", json!("dockerfile")),
                set("web.dockerfilePath", json!("web.Dockerfile")),
            ]),
            &evidence(),
        )
        .unwrap();

    // A repository Service runs an image, and its build settings return to their defaults.
    store
        .write(&who, &edit(vec![set("web.image", json!("nginx:2"))]))
        .unwrap();
    assert_eq!(source(&store, &who), SourceKind::Image);
    assert_eq!(values(&store, &who)["image"], json!("nginx:2"));

    store
        .write_trusted(
            &who,
            &edit(vec![set("web.repository", json!("acme/web"))]),
            &evidence(),
        )
        .unwrap();
    let back = values(&store, &who);
    assert_eq!(
        (
            &back["rootDir"],
            &back["buildMethod"],
            &back["dockerfilePath"]
        ),
        (&Value::Null, &json!("railpack"), &Value::Null),
        "{back}"
    );
}

#[test]
fn every_setting_set_must_apply_to_the_source_the_whole_edit_leaves() {
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
    let refused = |changes: Vec<Change>, message: &str| {
        let error = store
            .write_trusted(&who, &edit(changes), &evidence())
            .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
        assert_eq!(error.message, message, "{error:?}");
    };
    let image_build = "buildMethod: needs a repository source, and web runs an image";

    // An image Service takes no build setting, alone or beside a new image, in either order.
    refused(
        vec![set("web.buildMethod", json!("dockerfile"))],
        image_build,
    );
    refused(
        vec![
            set("web.image", json!("nginx:2")),
            set("web.buildMethod", json!("dockerfile")),
        ],
        image_build,
    );
    refused(
        vec![
            set("web.buildCommand", json!("npm run build")),
            set("web.image", json!("nginx:2")),
        ],
        "buildCommand: needs a repository source, and web runs an image",
    );
    // An image and a repository in one edit is two sources, in either order.
    refused(
        vec![
            set("web.image", json!("nginx:2")),
            set("web.repository", json!("acme/web")),
        ],
        "Set web.image or web.repository, not both",
    );
    refused(
        vec![
            set("web.repository", json!("acme/web")),
            set("web.image", json!("nginx:2")),
        ],
        "Set web.image or web.repository, not both",
    );
    // Unsetting is never refused.
    store
        .write(&who, &edit(vec![unset("web.buildMethod")]))
        .unwrap();
    assert_eq!(source(&store, &who), SourceKind::Image);
    assert_eq!(values(&store, &who)["image"], json!("nginx:1"));

    // Switching to a repository in the same edit is fine, in either order, even for a
    // branch that only a repository Service has.
    store
        .write_trusted(
            &who,
            &edit(vec![
                set("web.branch", json!("dev")),
                set("web.buildMethod", json!("dockerfile")),
                set("web.repository", json!("acme/web")),
                set("web.dockerfilePath", json!("web.Dockerfile")),
            ]),
            &evidence(),
        )
        .unwrap();
    assert_eq!(source(&store, &who), SourceKind::Git);
    let built = values(&store, &who);
    assert_eq!(
        (
            &built["branch"],
            &built["buildMethod"],
            &built["dockerfilePath"]
        ),
        (
            &json!("dev"),
            &json!("dockerfile"),
            &json!("web.Dockerfile")
        ),
        "{built}"
    );

    // A repository Service switching to an image takes no build or repository setting in
    // the same edit, in either order. The refusal names the Setting.
    for (path, value) in [
        ("web.buildMethod", json!("railpack")),
        ("web.branch", json!("main")),
        ("web.rootDir", json!("/apps/web")),
        ("web.autoDeploy", json!(false)),
    ] {
        let named = format!("{}: ", path.trim_start_matches("web."));
        for changes in [
            vec![set(path, value.clone()), set("web.image", json!("nginx:4"))],
            vec![set("web.image", json!("nginx:4")), set(path, value.clone())],
        ] {
            let error = store
                .write_trusted(&who, &edit(changes), &evidence())
                .unwrap_err();
            assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
            assert!(error.message.starts_with(&named), "{error:?}");
        }
    }
    let patch = Change::Patch {
        path: SettingPath::parse("web").unwrap(),
        value: json!({ "image": "nginx:4", "buildCommand": "make" }),
    };
    refused(
        vec![patch],
        "buildCommand: needs a repository source, and web runs an image",
    );
    assert_eq!(source(&store, &who), SourceKind::Git);
    assert_eq!(values(&store, &who), built);

    // Nothing hidden survives the switch: a later repository builds with the defaults.
    store
        .write(&who, &edit(vec![set("web.image", json!("nginx:4"))]))
        .unwrap();
    store
        .write_trusted(
            &who,
            &edit(vec![set("web.repository", json!("acme/web"))]),
            &evidence(),
        )
        .unwrap();
    let back = values(&store, &who);
    assert_eq!(
        (
            &back["buildMethod"],
            &back["dockerfilePath"],
            &back["buildCommand"]
        ),
        (&json!("railpack"), &Value::Null, &Value::Null),
        "{back}"
    );

    // An empty Service builds uploads, so it takes build settings.
    store
        .write(&who, &edit(vec![unset("web.repository")]))
        .unwrap();
    assert_eq!(source(&store, &who), SourceKind::Empty);
    store
        .write(
            &who,
            &edit(vec![set("web.buildMethod", json!("dockerfile"))]),
        )
        .unwrap();
    assert_eq!(values(&store, &who)["buildMethod"], json!("dockerfile"));
}

fn image_service() -> (ConfigStore, Actor) {
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
    (store, who)
}

fn unset(path: &str) -> Change {
    Change::Unset {
        path: SettingPath::parse(path).unwrap(),
    }
}

#[test]
fn an_edit_that_changes_one_path_twice_is_refused_and_changes_nothing() {
    let (store, who) = image_service();
    let before = values(&store, &who);
    let patch = |value: Value| Change::Patch {
        path: SettingPath::parse("web").unwrap(),
        value,
    };
    let refused = [
        (
            vec![set("web.image", json!("nginx:2")), unset("web.image")],
            "web.image",
        ),
        (
            vec![unset("web.image"), set("web.image", json!("nginx:2"))],
            "web.image",
        ),
        (
            vec![set("web.replicas", json!(2)), set("WEB.replicas", json!(3))],
            "web.replicas",
        ),
        (
            vec![
                patch(json!({ "replicas": 2 })),
                set("web.replicas", json!(3)),
            ],
            "web.replicas",
        ),
    ];
    for (changes, path) in refused {
        let error = store
            .write_trusted(&who, &edit(changes.clone()), &evidence())
            .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{changes:?}");
        assert_eq!(
            error.message,
            format!("{path} is changed twice in one edit; change it once"),
            "{changes:?}"
        );
        assert_eq!(values(&store, &who), before, "{changes:?}");
    }
}

#[test]
fn unsetting_a_setting_the_source_lacks_is_accepted_and_changes_nothing() {
    let (store, who) = image_service();
    let before = values(&store, &who);
    for path in [
        "web.repository",
        "web.branch",
        "web.rootDir",
        "web.buildMethod",
        "web.dockerfilePath",
        "web.buildCommand",
        "web.autoDeploy",
        "web.waitForCi",
        "web.watchPaths",
        "web.preferredBuilder",
    ] {
        store
            .write_trusted(&who, &edit(vec![unset(path)]), &evidence())
            .unwrap_or_else(|error| panic!("{path}: {error:?}"));
        assert_eq!(source(&store, &who), SourceKind::Image, "{path}");
        assert_eq!(values(&store, &who), before, "{path}");
    }

    let (store, who) = shop();
    store
        .write_trusted(&who, &create("acme/web", Some("dev")), &evidence())
        .unwrap();
    let before = values(&store, &who);
    for path in ["web.image", "web.registryCredential"] {
        store
            .write_trusted(&who, &edit(vec![unset(path)]), &evidence())
            .unwrap_or_else(|error| panic!("{path}: {error:?}"));
        assert_eq!(source(&store, &who), SourceKind::Git, "{path}");
        assert_eq!(values(&store, &who), before, "{path}");
    }
}

#[test]
fn a_source_switch_lands_the_same_in_every_key_order() {
    let changes = [
        unset("web.image"),
        set("web.repository", json!("acme/web")),
        set("web.branch", json!("dev")),
    ];
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    for order in orders {
        let (store, who) = image_service();
        let changes = order.map(|index| changes[index].clone()).to_vec();
        store
            .write_trusted(&who, &edit(changes), &evidence())
            .unwrap_or_else(|error| panic!("{order:?}: {error:?}"));
        assert_eq!(source(&store, &who), SourceKind::Git, "{order:?}");
        assert_eq!(
            values(&store, &who),
            json!({
                "repository": "acme/web", "branch": "dev", "buildMethod": "railpack",
                "maxRetries": 10, "privateDns": "web", "replicas": 1,
                "restartPolicy": "unless-stopped", "autoDeploy": true, "waitForCi": false,
                "watchPaths": [],
            }),
            "{order:?}"
        );
    }
}

/// `shop` with `sealedVariables` on and Git Service `web`, on a database in `dir`.
fn sealed_shop(dir: &tempfile::TempDir) -> (ConfigStore, Actor) {
    let url = backend::fresh_url(dir);
    let (store, who) = project(ConfigStore::open(&url, backend::key()).unwrap());
    backend::sealed_variables_on(&url, "org");
    store
        .write_trusted(&who, &create("acme/web", None), &evidence())
        .unwrap();
    (store, who)
}

/// Variable `key` of Service `service` as the browser sealed it.
fn sealed(service: &str, key: &str) -> Value {
    json!({ "secret": backend::client_sealed(ENVIRONMENT, service, key, 1) })
}

#[test]
fn a_build_that_reads_a_sealed_value_skips_github_whatever_it_prefers() {
    let dir = tempfile::tempdir().unwrap();
    let (store, who) = sealed_shop(&dir);
    store
        .write(
            &who,
            &edit(vec![
                set("web.preferredBuilder", json!("github")),
                set("web.env.PLAIN", json!("text")),
                set("web.env.NPM_TOKEN", sealed(SERVICE, "NPM_TOKEN")),
            ]),
        )
        .unwrap();
    let deployment = admit(&store, &who, 1, &[]);
    let source = &store.sources(&deployment).unwrap()[0];
    assert_eq!(source.builders, [Builder::Servers]);
    assert_eq!(source.preferred_machine, None);
    assert_eq!(source.sealed.as_deref(), Some("NPM_TOKEN"));
    assert_eq!(json!(source)["sealed"], "NPM_TOKEN");

    build_order(&store, &who, Some(BuildOrder::GithubOnly));
    let source = &store.sources(&deployment).unwrap()[0];
    assert!(source.builders.is_empty(), "{source:?}");

    // A preferred Server still leads.
    let machine = "0123456789abcdef0123456789abcdef";
    store
        .write(
            &who,
            &edit(vec![set("web.preferredBuilder", json!(machine))]),
        )
        .unwrap();
    let source = &store.sources(&deployment).unwrap()[0];
    assert_eq!(source.builders, [Builder::Servers]);
    assert_eq!(
        source.preferred_machine.as_ref().unwrap().to_string(),
        machine
    );
}

#[test]
fn github_gets_no_sealed_value_and_learns_which_one_its_build_reads() {
    let dir = tempfile::tempdir().unwrap();
    let (store, who) = sealed_shop(&dir);
    let db = "00000000-0000-4000-8000-000000000004";
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(db).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("db").unwrap(),
                image: Some("postgres:16".into()),
                template: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &edit(vec![
                set("web.env.PLAIN", json!("text")),
                set("web.env.TOKEN", json!({ "secret": "legacy" })),
                set("db.env.PASSWORD", sealed(db, "PASSWORD")),
                set("db.env.URL", json!("pg://${{ PASSWORD }}@db")),
            ]),
        )
        .unwrap();
    let id = {
        let deployment = admit(&store, &who, 1, &[]);
        store
            .pin(&deployment, &pins(&backend::sha(&"a".repeat(40))))
            .unwrap();
        GithubBuildId::parse(&format!("{deployment}.web")).unwrap()
    };
    let built = store.github_input(&id).unwrap();
    assert_eq!(built.sealed, None);
    let input = built.input;
    let snapshots = input["snapshots"].as_array().unwrap();
    assert_eq!(snapshots.len(), 2);
    for snapshot in snapshots {
        for (name, value) in snapshot["resolvedEnv"].as_object().unwrap() {
            assert!(value.is_string(), "{name} is {value}");
        }
    }
    let of = |input: &Value, service: &str| {
        input["snapshots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|snapshot| snapshot["serviceId"] == service)
            .unwrap()
            .clone()
    };
    assert_eq!(of(&input, SERVICE)["resolvedEnv"]["TOKEN"], "legacy");
    assert!(of(&input, db)["resolvedEnv"].get("PASSWORD").is_none());
    // What a Server builds from: the built Service reads the same there.
    let claimed = store
        .claim(&id.deployment, &RunnerId::parse("runner").unwrap())
        .unwrap();
    assert!(of(&claimed.input, db)["resolvedEnv"]["PASSWORD"].is_object());
    assert_eq!(of(&input, SERVICE), of(&claimed.input, SERVICE));

    // A reference to another Service's sealed value reads it too.
    store
        .write(
            &who,
            &edit(vec![set("web.env.DB_URL", json!("${{ db.URL }}"))]),
        )
        .unwrap();
    store
        .record(
            &id.deployment,
            &RunnerId::parse("runner").unwrap(),
            RunEvidence::NotExecuted("stopped".into()),
        )
        .unwrap();
    let deployment = admit(&store, &who, 2, &[]);
    let sources = store
        .pin(&deployment, &pins(&backend::sha(&"b".repeat(40))))
        .unwrap();
    assert_eq!(sources[0].sealed.as_deref(), Some("DB_URL"));
    assert_eq!(sources[0].builders, [Builder::Servers]);
    let id = GithubBuildId::parse(&format!("{deployment}.web")).unwrap();
    let built = store.github_input(&id).unwrap();
    assert_eq!(built.sealed.as_deref(), Some("DB_URL"));
    for snapshot in built.input["snapshots"].as_array().unwrap() {
        for (name, value) in snapshot["resolvedEnv"].as_object().unwrap() {
            assert!(value.is_string(), "{name} is {value}");
        }
    }
}

#[test]
fn a_revision_claim_will_refuse_still_lists_its_builds() {
    let dir = tempfile::tempdir().unwrap();
    let (store, who) = sealed_shop(&dir);
    // PORT reads PASSWORD while it is plain text, and still once it is sealed.
    for change in [
        set("web.preferredBuilder", json!("github")),
        set("web.env.PASSWORD", json!("8080")),
        set("web.env.PORT", json!("${{ PASSWORD }}")),
        set("web.env.PASSWORD", sealed(SERVICE, "PASSWORD")),
    ] {
        store.write(&who, &edit(vec![change])).unwrap();
    }
    let deployment = admit(&store, &who, 1, &[]);
    let source = &store.sources(&deployment).unwrap()[0];
    assert_eq!(source.sealed.as_deref(), Some("PASSWORD"));
    assert_eq!(source.builders, [Builder::Servers]);
}

#[test]
fn an_environment_with_no_sealed_value_routes_without_opening_a_secret() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = project(ConfigStore::open(&url, backend::key()).unwrap());
    store
        .write_trusted(&who, &create("acme/web", None), &evidence())
        .unwrap();
    store
        .write(
            &who,
            &edit(vec![
                set("web.env.TOKEN", json!({ "secret": "legacy" })),
                set("web.env.URL", json!("https://${{ TOKEN }}@example.com")),
            ]),
        )
        .unwrap();
    let deployment = admit(&store, &who, 1, &[]);

    // Another key can't open the legacy secret, which a build's input needs.
    let store = ConfigStore::open(&url, SealingKey::new(b"another-key").unwrap()).unwrap();
    let id = GithubBuildId::parse(&format!("{deployment}.web")).unwrap();
    assert!(store.github_input(&id).is_err());
    // Routing it never did, so it still doesn't.
    let source = &store.sources(&deployment).unwrap()[0];
    assert_eq!(source.sealed, None);
    assert_eq!(source.builders, [Builder::Github, Builder::Servers]);
    let pinned = store
        .pin(&deployment, &pins(&backend::sha(&"a".repeat(40))))
        .unwrap();
    assert_eq!(pinned[0].commit, Some(backend::sha(&"a".repeat(40))));
    assert_eq!(pinned[0].sealed, None);
}
