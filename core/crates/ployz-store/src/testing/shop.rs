//! A Project whose Git Services follow `acme/web`'s `main`, and the pushes and check
//! suites Cloud observes of it, for the Git automation suites.

use crate::{
    Actor, AuthorizedRepository, Automated, BranchHead, Change, CheckSuite, Command, ConfigStore,
    CreateGitService, CreateProject, Edit, EnvironmentId, EnvironmentRef, OrganizationId,
    ProjectId, ProjectName, Publish, ServiceLineageId, SettingPath, SystemEvent, Trusted, Written,
};
use ployz_core::ServiceName;
use ployz_core::config::ServiceGitAccess;
use serde_json::{Value, json};

use super::backend;

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
                region: None,
            },
        )
        .unwrap();
    for (n, name) in [(3, "web"), (4, "api")] {
        add_git_service(&store, &who, n, name);
    }
    (store, who)
}

/// Save Git Service `name` on `acme/web`'s `main`, with lineage ID ending in `n`.
pub fn add_git_service(store: &ConfigStore, who: &Actor, n: u8, name: &str) {
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
            who,
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
                rule: None,
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
        updated: crate::GithubTimestamp::parse(format!("2026-09-29T10:{minute:02}:00Z")).unwrap(),
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
                    &crate::DeploymentQuery {
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
pub fn deployments(store: &ConfigStore, who: &Actor) -> Vec<crate::DeploymentSummary> {
    store
        .read(who, &crate::DeploymentsQuery::default())
        .unwrap()
        .deployments
}

/// The production Environment's Deployments waiting for CI, newest first.
pub fn held(store: &ConfigStore, who: &Actor) -> Vec<crate::DeploymentSummary> {
    deployments(store, who)
        .into_iter()
        .filter(|deployment| deployment.status == crate::DeploymentStatus::WaitingForCi)
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

/// Sign `generation` of a registry rule for `<host>/acme/worker` over Service worker's
/// settings as they are Saved.
pub fn signed_registry_rule_on(
    store: &ConfigStore,
    who: &Actor,
    generation: u64,
    host: &str,
) -> Box<ployz_core::authority::Signed<ployz_core::authority::AutoDeployRule>> {
    use ployz_core::authority::rule::{RegistrySource, RuleService, RuleSource, template};
    use ployz_core::authority::{AutoDeployRule, RuleId, Signed, Signer, SignerKey, Timestamp};
    let view: crate::RuleView = store
        .read(
            who,
            &crate::RuleQuery {
                environment: EnvironmentRef::default(),
                staged: false,
            },
        )
        .unwrap();
    let worker = view
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "worker")
        .unwrap();
    let signer = Signer::from_seed(&[7; 32]);
    let rule = AutoDeployRule {
        proposal: None,
        cluster: ployz_core::SealingPublicKey::from_bytes([3; 32]),
        rule: RuleId::from_bytes([5; 16]),
        generation,
        namespace: view.namespace.clone(),
        machines: Vec::new(),
        source: RuleSource::Registry(RegistrySource {
            host: host.into(),
            repository: "acme/worker".into(),
        }),
        services: vec![RuleService {
            name: worker.name.clone(),
            template: template(worker, &view.namespace).unwrap(),
            policy: None,
            secrets: None,
        }],
        issued_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(ployz_core::DeviceKey::from_bytes(
            *signer.public().as_bytes(),
        )),
    };
    Box::new(Signed::sign(&rule, &signer).unwrap())
}

/// Image Service `worker` on private `ghcr.io/acme/worker`, with its registry login,
/// Saved under a registry rule and deployed. Returns its image.
pub fn registry_worker(store: &ConfigStore, who: &Actor) -> String {
    let image = format!("ghcr.io/acme/worker@sha256:{}", "a".repeat(64));
    store
        .write(
            who,
            &crate::CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000006").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("worker").unwrap(),
                image: Some(image.clone()),
                template: None,
            },
        )
        .unwrap();
    set(
        store,
        who,
        "worker.registryCredential",
        super::sealed_login_for("ghcr.io", "worker"),
    );
    publish(store, who);
    let rule = signed_registry_rule_on(store, who, 1, "ghcr.io");
    let staged: crate::RuleView = store
        .read(
            who,
            &crate::RuleQuery {
                environment: EnvironmentRef::default(),
                staged: true,
            },
        )
        .unwrap();
    store
        .write_trusted(
            who,
            &Command::Publish(Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: staged.version,
                message: None,
                accept_volume_loss: Vec::new(),
                rule: Some(crate::SavedRule {
                    rule,
                    births: Default::default(),
                    prior: None,
                }),
            }),
            &Trusted::default(),
        )
        .unwrap();
    let id = crate::DeploymentId::parse("00000000-0000-4000-8000-000000000108").unwrap();
    store
        .write_trusted(
            who,
            &crate::Admit::Deploy(crate::Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                copies: Vec::new(),
                recreate: Vec::new(),
                rule: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    backend::run(store, &id);
    image
}
