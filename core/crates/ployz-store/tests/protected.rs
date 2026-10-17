//! Protected Environments, on SQLite and on Postgres (see `backend`): a new Project's
//! Default starts Protected, the switch flips either way, and only a Project's own
//! Environments take it.

use ployz_core::config::ServiceGitAccess;
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AuthorizedRepository, Automated, ConfigStore, CreateBranch, CreateEnvironment,
    CreateGitService, CreateProject, EnvironmentId, EnvironmentName, EnvironmentRef,
    EnvironmentsQuery, OrganizationId, ProjectId, ProjectName, PullRequest, ServiceLineageId,
    SetEnvironmentProtected, SetPrPlan, SystemEvent, Trusted, Written,
};
use serde_json::json;

mod backend;

const HEAD: &str = "1111111111111111111111111111111111111111";

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

/// Project `shop`: Default `production` runs Git Service `web` from `acme/web`'s `main`,
/// published, beside an empty `staging`.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
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
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(3)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                repository: backend::repo_name("acme/web"),
                branch: None,
            },
            &evidence,
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::Command::Publish(ployz_store::Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateEnvironment {
                id: EnvironmentId::parse(uuid(4)).unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    (store, who)
}

/// Each Environment's name and whether it is Protected.
fn protection(store: &ConfigStore, who: &Actor) -> Vec<(String, bool)> {
    store
        .read(who, &EnvironmentsQuery::default())
        .unwrap()
        .environments
        .into_iter()
        .map(|listing| (listing.name.to_string(), listing.protected))
        .collect()
}

fn set(environment: &str, protected: bool) -> SetEnvironmentProtected {
    SetEnvironmentProtected {
        environment: at(environment),
        protected,
    }
}

#[test]
fn a_new_projects_default_environment_starts_protected() {
    let (store, who) = shop();

    assert_eq!(
        protection(&store, &who),
        [("production".into(), true), ("staging".into(), false)]
    );
}

#[test]
fn protecting_flips_either_way_and_returns_the_projects_environments() {
    let (store, who) = shop();

    let Written::Protected(view) = store
        .write(
            &who,
            &ployz_store::Command::SetEnvironmentProtected(set("staging", true)),
        )
        .unwrap()
    else {
        panic!("protecting writes Protected")
    };
    let listed: Vec<_> = view
        .environments
        .iter()
        .map(|listing| (listing.name.to_string(), listing.protected))
        .collect();
    assert_eq!(
        listed,
        [("production".into(), true), ("staging".into(), true)]
    );

    store.write(&who, &set("production", false)).unwrap();
    store.write(&who, &set("staging", false)).unwrap();
    assert_eq!(
        protection(&store, &who),
        [("production".into(), false), ("staging".into(), false)]
    );

    store.write(&who, &set("staging", false)).unwrap();
    assert_eq!(
        protection(&store, &who),
        [("production".into(), false), ("staging".into(), false)]
    );
}

#[test]
fn a_branch_cannot_be_protected() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(5)).unwrap(),
                from: at("production"),
                name: EnvironmentName::parse("fix-web").unwrap(),
                copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                live: Vec::new(),
                setup: Vec::new(),
                keep: false,
                fix: None,
            },
        )
        .unwrap();

    let error = store.write(&who, &set("fix-web", true)).unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unsupported);
    assert_eq!(
        error.message,
        "fix-web is a Branch: only a Project's own Environments can be Protected"
    );
    assert_eq!(
        error.details,
        json!({"project": "shop", "environment": "fix-web"})
    );
    assert!(
        protection(&store, &who)
            .iter()
            .all(|(name, protected)| name != "fix-web" || !protected)
    );
}

#[test]
fn a_pr_environment_cannot_be_protected() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy: None,
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    let Written::Automated(Automated { .. }) = store
        .system(
            &who.organization,
            &SystemEvent::PullRequest(PullRequest {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
                title: "Add search".into(),
                author: "ada".into(),
                author_id: Some(backend::ada()),
                bot: false,
                head_branch: backend::git_branch("search"),
                head_repository_id: Some(backend::repo_id(11)),
                head: backend::sha(HEAD),
                target_branch: backend::git_branch("main"),
                commits: 1,
                open: true,
                merge_commit: None,
                target_head: None,
                updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:00:01Z").unwrap(),
            }),
            &backend::members(),
        )
        .unwrap()
    else {
        panic!("a pull request writes Automated")
    };

    let error = store.write(&who, &set("pr-5", true)).unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unsupported);
    assert_eq!(
        error.message,
        "pr-5 is a PR Environment: only a Project's own Environments can be Protected"
    );
    assert!(protection(&store, &who).contains(&("pr-5".into(), false)));
}
