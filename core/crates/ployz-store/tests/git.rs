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
    ConfigStore, CreateGitService, CreateProject, CreateService, DeploymentId, DiffQuery, Edit,
    EnvironmentId, EnvironmentQuery, EnvironmentRef, OrganizationId, ProjectId, ProjectName,
    Publish, Query, RunEvidence, RunnerId, ServiceId, SettingPath, Trusted, View, Written,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod backend;

const SERVICE: &str = "00000000-0000-4000-8000-000000000003";

fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor {
        organization: OrganizationId::parse("org").unwrap(),
    };
    store
        .create_project(
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
                repository: "acme/web".into(),
                repository_id: 11,
                access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
                default_branch: "main".into(),
                branches: vec!["dev".into()],
            },
            AuthorizedRepository {
                repository: "acme/docs".into(),
                repository_id: 12,
                access: ServiceGitAccess::Public,
                default_branch: "main".into(),
                branches: Vec::new(),
            },
        ],
        ..Trusted::default()
    }
}

fn create(repository: &str, branch: Option<&str>) -> CreateGitService {
    CreateGitService {
        id: ServiceId::parse(SERVICE).unwrap(),
        environment: EnvironmentRef::default(),
        name: ServiceName::parse("web").unwrap(),
        repository: repository.into(),
        branch: branch.map(Into::into),
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
    json!(store.environment(who, &query).unwrap().values)
}

#[test]
fn a_repository_needs_cloud_evidence() {
    let (store, who) = shop();
    let error = store
        .create_git_service(&who, &create("acme/web", None), &Trusted::default())
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
    assert_eq!(error.details["next"], "ployz github connect");

    let error = store
        .create_git_service(&who, &create("acme/secret", None), &evidence())
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
    assert!(!error.message.contains("secret"), "{error:?}");

    let error = store
        .create_git_service(&who, &create("not a repo!", None), &evidence())
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(!error.message.contains("not a repo"), "{error:?}");

    let error = store
        .create_git_service(&who, &create("acme/web", Some("nope-branch")), &evidence())
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
        .create_git_service(&who, &create("ACME/Web", None), &evidence())
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
            "maxRetries": 10, "replicas": 1, "restartPolicy": "unless-stopped",
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
    let diff = store.diff(&who, &DiffQuery::default()).unwrap();
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

    let diff = store.diff(&who, &DiffQuery::default()).unwrap();
    let rows = &diff.changes[0].settings;
    let repository = rows
        .iter()
        .find(|row| row.path == "web.repository")
        .unwrap();
    assert_eq!(repository.after, json!("acme/docs"));
    let published = store
        .publish(
            &who,
            &Publish {
                environment: EnvironmentRef::default(),
                version: Some(diff.version),
            },
        )
        .unwrap();
    assert!(published.saved.0 >= 1);
    assert!(store.diff(&who, &DiffQuery::default()).unwrap().published);
}

fn admit(store: &ConfigStore, who: &Actor, n: u8, services: &[&str]) -> DeploymentId {
    let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap();
    store
        .admit(
            who,
            &Admit {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: services
                    .iter()
                    .map(|name| ServiceName::parse(*name).unwrap())
                    .collect(),
                version: None,
                upload: None,
                retry: None,
            },
        )
        .unwrap();
    id
}

fn pins(commit: &str) -> BTreeMap<ServiceName, String> {
    BTreeMap::from([(ServiceName::parse("web").unwrap(), commit.to_owned())])
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
        .create_git_service(&who, &create("acme/web", None), &evidence())
        .unwrap();
    store
        .create_service(
            &who,
            &CreateService {
                id: ServiceId::parse("00000000-0000-4000-8000-000000000004").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("api").unwrap(),
                image: Some("nginx:1".into()),
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
    assert_eq!(source.repository_id, 11);
    assert_eq!(
        source.access,
        ServiceGitAccess::GithubInstallation { installation_id: 7 }
    );
    assert_eq!(
        (source.branch.as_deref(), source.commit.as_deref()),
        (Some("main"), None)
    );

    // A pin never moves: the branch moving on changes nothing for this Deployment.
    let a = "a".repeat(40);
    let b = "b".repeat(40);
    assert_eq!(
        store.pin(&first, &pins(&a)).unwrap()[0].commit,
        Some(a.clone())
    );
    assert_eq!(
        store.pin(&first, &pins(&b)).unwrap()[0].commit,
        Some(a.clone())
    );
    for bad in [
        pins("HEAD"),
        BTreeMap::from([(ServiceName::parse("api").unwrap(), a.clone())]),
    ] {
        let error = store.pin(&first, &bad).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    }

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
    let view = store.deployment(&who, &first).unwrap();
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
    let stranger = Actor {
        organization: OrganizationId::parse("other").unwrap(),
    };
    let query = BuildLogQuery {
        deployment: first.clone(),
        service: ServiceName::parse("web").unwrap(),
    };
    assert_eq!(
        store.build_log(&stranger, &query).unwrap_err().code,
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
        .admit(
            &who,
            &Admit {
                id: again.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                retry: Some(first.clone()),
            },
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
        receipt
    );

    // A Deployment of other Services builds nothing.
    let targeted = admit(&store, &who, 3, &["api"]);
    assert!(store.sources(&targeted).unwrap().is_empty());
}
