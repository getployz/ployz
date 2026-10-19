//! A Project whose Git Services follow `acme/web`'s `main`, and the pushes and check
//! suites Cloud observes of it, for the Git automation suites.
#![allow(dead_code, reason = "each suite uses what it needs")]

use ployz_core::ServiceName;
use ployz_core::config::ServiceGitAccess;
use ployz_store::{
    Actor, AuthorizedRepository, Automated, BranchHead, Change, CheckSuite, Command, ConfigStore,
    CreateGitService, CreateProject, Edit, EnvironmentId, EnvironmentRef, OrganizationId,
    ProjectId, ProjectName, Publish, ServiceLineageId, SettingPath, SystemEvent, Trusted, Written,
};
use serde_json::{Value, json};

use crate::backend;

pub const H1: &str = "1111111111111111111111111111111111111111";
pub const H2: &str = "2222222222222222222222222222222222222222";
pub const H3: &str = "3333333333333333333333333333333333333333";

pub fn org() -> OrganizationId {
    OrganizationId::parse("org").unwrap()
}

/// A Project with Git Services `web` and `api` on `acme/web`'s `main`, published.
pub fn shop() -> (ConfigStore, Actor) {
    shop_on(backend::open())
}

pub fn shop_on(store: ConfigStore) -> (ConfigStore, Actor) {
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

pub fn publish(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &Command::Publish(Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
}

pub fn set(store: &ConfigStore, who: &Actor, path: &str, value: Value) -> Written {
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

pub fn push(base: Option<&str>, head: Option<&str>, changed: Option<&[&str]>) -> SystemEvent {
    SystemEvent::BranchHead(BranchHead {
        repository_id: backend::repo_id(11),
        branch: backend::git_branch("main"),
        base: base.map(backend::sha),
        head: head.map(backend::sha),
        changed: changed.map(|paths| paths.iter().map(|path| (*path).to_owned()).collect()),
    })
}

pub fn suite(
    id: u64,
    head: &str,
    status: &str,
    conclusion: Option<&str>,
    minute: u8,
) -> SystemEvent {
    SystemEvent::CheckSuite(CheckSuite {
        repository_id: backend::repo_id(11),
        suite: id,
        head: backend::sha(head),
        status: serde_json::from_value(json!(status)).unwrap(),
        conclusion: conclusion.map(|conclusion| serde_json::from_value(json!(conclusion)).unwrap()),
        updated: ployz_store::GithubTimestamp::parse(format!("2026-09-29T10:{minute:02}:00Z"))
            .unwrap(),
    })
}

pub fn system(store: &ConfigStore, event: &SystemEvent) -> Automated {
    let Written::Automated(automated) = store.system(&org(), event, &backend::members()).unwrap()
    else {
        panic!("a system event writes Automated")
    };
    automated
}

/// The Services each admitted Deployment targets, and the commit each pins.
pub fn deployed(
    store: &ConfigStore,
    who: &Actor,
    automated: &Automated,
) -> Vec<(Vec<String>, u64)> {
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

/// The production Environment's Deployments, newest first.
pub fn deployments(store: &ConfigStore, who: &Actor) -> Vec<ployz_store::DeploymentSummary> {
    store
        .read(who, &ployz_store::DeploymentsQuery::default())
        .unwrap()
        .deployments
}

/// The production Environment's Deployments waiting for CI, newest first.
pub fn held(store: &ConfigStore, who: &Actor) -> Vec<ployz_store::DeploymentSummary> {
    deployments(store, who)
        .into_iter()
        .filter(|deployment| deployment.status == ployz_store::DeploymentStatus::WaitingForCi)
        .collect()
}

pub fn pinned(store: &ConfigStore, automated: &Automated, service: &str) -> Option<String> {
    let id = &automated.admitted.first()?.deployment.id;
    store
        .sources(id)
        .unwrap()
        .into_iter()
        .find(|source| source.service.as_str() == service)
        .and_then(|source| source.commit)
        .map(String::from)
}
