#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Git automation through `system`, on SQLite and on Postgres (see `backend`): pushes
//! and check suites admit auto-deploys of Saved State, idempotently.

use ployz_core::config::ServiceGitAccess;
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AuthorizedRepository, Automated, BranchHead, Change, CheckSuite, Command, ConfigStore,
    CreateGitService, CreateProject, Edit, EnvironmentId, EnvironmentRef, OrganizationId,
    ProjectId, ProjectName, Publish, ServiceLineageId, SettingPath, SystemEvent, Trusted, Written,
};
use serde_json::{Value, json};

mod backend;

const H1: &str = "1111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222";
const H3: &str = "3333333333333333333333333333333333333333";

fn org() -> OrganizationId {
    OrganizationId::parse("org").unwrap()
}

/// A Project with Git Services `web` and `api` on `acme/web`'s `main`, published.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(org());
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
    let evidence = Trusted {
        repositories: vec![AuthorizedRepository {
            repository: backend::repo_name("acme/web"),
            repository_id: backend::repo_id(11),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: backend::git_branch("main"),
            branches: Vec::new(),
        }],
        ..Trusted::default()
    };
    for (n, name) in [(3, "web"), (4, "api")] {
        store
            .write_trusted(
                &who,
                &CreateGitService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    repository: backend::repo_name("acme/web"),
                    branch: None,
                },
                &evidence,
            )
            .unwrap();
    }
    (store, who)
}

fn publish(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &Command::Publish(Publish {
                environment: EnvironmentRef::default(),
                version: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
}

fn set(store: &ConfigStore, who: &Actor, path: &str, value: Value) -> Written {
    store
        .write(
            who,
            &Command::Edit(Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse(path).unwrap(),
                    value,
                }],
            }),
        )
        .unwrap()
}

fn push(base: Option<&str>, head: Option<&str>, changed: Option<&[&str]>) -> SystemEvent {
    SystemEvent::BranchHead(BranchHead {
        repository_id: backend::repo_id(11),
        branch: backend::git_branch("main"),
        base: base.map(backend::sha),
        head: head.map(backend::sha),
        changed: changed.map(|paths| paths.iter().map(|path| (*path).to_owned()).collect()),
        merged: Vec::new(),
    })
}

fn suite(id: u64, head: &str, status: &str, conclusion: Option<&str>, minute: u8) -> SystemEvent {
    SystemEvent::CheckSuite(CheckSuite {
        repository_id: backend::repo_id(11),
        suite: id,
        head: backend::sha(head),
        status: status.into(),
        conclusion: conclusion.map(Into::into),
        updated: ployz_store::GithubTimestamp::parse(format!("2026-09-29T10:{minute:02}:00Z")).unwrap(),
    })
}

fn system(store: &ConfigStore, event: &SystemEvent) -> Automated {
    let Written::Automated(automated) = store.system(&org(), event, &Trusted::default()).unwrap()
    else {
        panic!("a system event writes Automated")
    };
    automated
}

/// The Services each admitted Deployment targets, and the commit each pins.
fn deployed(store: &ConfigStore, who: &Actor, automated: &Automated) -> Vec<(Vec<String>, u64)> {
    automated
        .admitted
        .iter()
        .map(|admitted| {
            let summary = &admitted.deployment;
            let view = store
                .read(
                    who,
                    &ployz_store::DeploymentQuery {
                        id: summary.id.clone(),
                    },
                )
                .unwrap();
            let sources = store.sources(&summary.id).unwrap();
            let names = summary.services.iter().map(ToString::to_string).collect();
            for source in sources
                .iter()
                .filter(|source| summary.services.contains(&source.service))
            {
                assert!(source.commit.is_some(), "every targeted Service is pinned");
            }
            (names, view.deployment.saved.0)
        })
        .collect()
}

fn pinned(store: &ConfigStore, automated: &Automated, service: &str) -> Option<String> {
    let id = &automated.admitted.first()?.deployment.id;
    store
        .sources(id)
        .unwrap()
        .into_iter()
        .find(|source| source.service.as_str() == service)
        .and_then(|source| source.commit)
        .map(String::from)
}

#[test]
fn a_push_deploys_saved_state_only_and_a_replay_changes_nothing() {
    let (store, who) = shop();
    // Nothing Saved yet: a push deploys nothing, yet the head counts.
    assert_eq!(
        system(&store, &push(None, Some(H1), None)),
        Automated::default()
    );
    publish(&store, &who);
    // Working State moves on; the push ships Saved revision 1.
    set(&store, &who, "web.replicas", json!(3));

    let automated = system(&store, &push(Some(H1), Some(H2), None));
    assert_eq!(
        deployed(&store, &who, &automated),
        vec![(vec!["web".to_owned(), "api".to_owned()], 1)]
    );
    assert_eq!(pinned(&store, &automated, "web").as_deref(), Some(H2));

    // A replay, or a delivery that read the same head, is a no-op.
    assert_eq!(
        system(&store, &push(Some(H1), Some(H2), None)),
        Automated::default()
    );
    // A compare from a stale head is refused, so an older head never comes back.
    let error = store
        .system(&org(), &push(Some(H1), Some(H3), None), &Trusted::default())
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(error.details["head"], H2);
    assert_eq!(
        store
            .branch_head(&org(), backend::repo_id(11), &backend::git_branch("main"))
            .unwrap(),
        Some(backend::sha(H2))
    );

    // Deleting the branch forgets its head: the next push deploys everything.
    assert_eq!(
        system(&store, &push(Some(H2), None, None)),
        Automated::default()
    );
    assert_eq!(
        store
            .branch_head(&org(), backend::repo_id(11), &backend::git_branch("main"))
            .unwrap(),
        None
    );
    let automated = system(&store, &push(None, Some(H3), Some(&["nothing.md"])));
    assert_eq!(automated.admitted.len(), 1);
    assert_eq!(automated.admitted[0].deployment.services.len(), 2);

    // Malformed observations are refused without echoing them: a commit isn't one
    // unless it parses, and a changed path must be the repository's.
    let error = serde_json::from_value::<SystemEvent>(serde_json::json!({
        "event": "branch_head", "repository_id": 11, "branch": "main", "head": "nope",
    }))
    .unwrap_err();
    assert!(!error.to_string().contains("nope"), "{error}");
    let error = store
        .system(
            &org(),
            &push(Some(H3), Some(H1), Some(&["../etc"])),
            &Trusted::default(),
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
}

#[test]
fn policy_settings_apply_at_once_and_select_by_watch_paths() {
    let (store, who) = shop();
    publish(&store, &who);
    let Written::Edited(edited) = set(
        &store,
        &who,
        "web.watchPaths",
        json!(["web/**", "!**/*.md"]),
    ) else {
        panic!("an edit")
    };
    assert_eq!(edited.immediate.len(), 1);
    assert!(edited.staged.is_empty());
    set(&store, &who, "api.autoDeploy", json!("false"));
    let error = store
        .write(
            &who,
            &Command::Edit(Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("web.watchPaths").unwrap(),
                    value: json!(["../up"]),
                }],
            }),
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);

    system(&store, &push(None, Some(H1), None));
    // Only docs changed: web doesn't watch them, and api doesn't auto-deploy.
    let automated = system(&store, &push(Some(H1), Some(H2), Some(&["web/README.md"])));
    assert!(automated.admitted.is_empty(), "{automated:?}");
    let automated = system(
        &store,
        &push(Some(H2), Some(H3), Some(&["web/src/main.rs"])),
    );
    assert_eq!(
        deployed(&store, &who, &automated),
        vec![(vec!["web".to_owned()], 1)]
    );
    // A force-push (no changed paths) deploys whatever follows the branch.
    let automated = system(&store, &push(Some(H3), Some(H1), None));
    assert_eq!(
        deployed(&store, &who, &automated),
        vec![(vec!["web".to_owned()], 1)]
    );
    assert_eq!(pinned(&store, &automated, "web").as_deref(), Some(H1));
}

#[test]
fn wait_for_ci_holds_a_deploy_until_every_suite_passes() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    system(&store, &push(None, Some(H1), None));
    let automated = system(&store, &push(Some(H1), Some(H2), None));
    assert!(automated.admitted.is_empty());
    assert_eq!(automated.waiting.len(), 1);

    assert_eq!(
        system(&store, &suite(1, H2, "in_progress", None, 10))
            .admitted
            .len(),
        0
    );
    assert_eq!(
        system(&store, &suite(2, H2, "completed", Some("success"), 11))
            .admitted
            .len(),
        0
    );
    assert_eq!(
        system(&store, &suite(1, H2, "completed", Some("failure"), 20))
            .admitted
            .len(),
        0
    );
    // A late delivery of an older result never restores it.
    assert_eq!(
        system(&store, &suite(1, H2, "in_progress", None, 15))
            .admitted
            .len(),
        0
    );
    // CI reran and passed: the deploy goes, pinned to the pushed commit.
    let automated = system(&store, &suite(1, H2, "completed", Some("success"), 30));
    assert_eq!(
        deployed(&store, &who, &automated),
        vec![(vec!["web".to_owned(), "api".to_owned()], 1)]
    );
    assert_eq!(pinned(&store, &automated, "api").as_deref(), Some(H2));
    // A replay deploys nothing more.
    assert_eq!(
        system(&store, &suite(1, H2, "completed", Some("success"), 30)),
        Automated::default()
    );

    // A newer head replaces the one still waiting, whose CI then deploys nothing.
    system(&store, &push(Some(H2), Some(H3), None));
    system(&store, &push(Some(H3), Some(H1), None));
    assert_eq!(
        system(&store, &suite(3, H3, "completed", Some("success"), 40)),
        Automated::default()
    );
    // CI that already passed before the push lets it deploy at once.
    let automated = system(&store, &suite(4, H1, "completed", Some("success"), 50));
    assert_eq!(automated.admitted.len(), 1);
    system(&store, &suite(5, H2, "completed", Some("neutral"), 60));
    let automated = system(&store, &push(Some(H1), Some(H2), None));
    assert_eq!(automated.admitted.len(), 1, "{automated:?}");
}
