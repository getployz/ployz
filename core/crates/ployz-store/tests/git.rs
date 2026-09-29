#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Git-backed Services through the public interface, on SQLite and on Postgres (see
//! `backend`): a repository and branch are only accepted with Cloud's evidence.

use ployz_core::config::ServiceGitAccess;
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AuthorizedRepository, Change, Command, ConfigStore, CreateGitService, CreateProject,
    DiffQuery, Edit, EnvironmentId, EnvironmentQuery, EnvironmentRef, OrganizationId, ProjectId,
    ProjectName, Publish, ServiceId, SettingPath, Trusted, Written,
};
use serde_json::{Value, json};

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
