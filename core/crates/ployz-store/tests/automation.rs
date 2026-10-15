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
                message: None,
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
    })
}

fn suite(id: u64, head: &str, status: &str, conclusion: Option<&str>, minute: u8) -> SystemEvent {
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
fn discarded_mount_reversal_affects_automatic_deploy_only_after_save() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ConfigName::parse("sentry").unwrap(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/old".into(),
                }],
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::PutConfigFile {
                environment: EnvironmentRef::default(),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
                file: ployz_core::ConfigFileName::parse("config.yml").unwrap(),
                content: "value: old".into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
    backend::deploy(&store, &who, "production", 1);
    store
        .write(
            &who,
            &ployz_store::AttachConfig {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
                dir: "/new".into(),
            },
        )
        .unwrap();
    publish(&store, &who);
    let history = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap()
        .revisions;
    store
        .write(
            &who,
            &ployz_store::RenameConfig {
                environment: EnvironmentRef::default(),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
                name: ployz_core::ConfigName::parse("errors").unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::Discard {
                environment: EnvironmentRef::default(),
                target: ployz_store::DiscardTarget::Head,
                path: Some(SettingPath::parse("web.configs.errors").unwrap()),
                version: None,
            },
        )
        .unwrap();
    let working = store
        .read(
            &who,
            &ployz_store::ServiceQuery {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        working.values.get("configs"),
        Some(&json!({"errors": "/old"}))
    );
    let automated = system(&store, &push(None, Some(H1), None));
    let claimed = store
        .claim(
            &automated.admitted[0].deployment.id,
            &ployz_store::RunnerId::parse("runner").unwrap(),
        )
        .unwrap();
    let web = claimed
        .intent
        .target
        .iter()
        .find(|service| service.name.as_str() == "web")
        .unwrap();
    assert_eq!(
        json!(web.config_mounts()),
        json!([{
            "config_name": "sentry/config.yml", "target": "/new/config.yml",
            "mode": 0o444, "uid": 0, "gid": 0
        }])
    );
    let configs = store
        .read(&who, &ployz_store::ConfigsQuery::default())
        .unwrap();
    assert_eq!(configs.configs[0].config.name.as_str(), "errors");
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap()
            .revisions,
        history
    );
    let after_push = store
        .read(
            &who,
            &ployz_store::ServiceQuery {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(after_push.values, working.values);
    publish(&store, &who);
    let automated = system(&store, &push(Some(H1), Some(H2), None));
    let claimed = store
        .claim(
            &automated.admitted.first().unwrap().deployment.id,
            &ployz_store::RunnerId::parse("runner").unwrap(),
        )
        .unwrap();
    let web = claimed
        .intent
        .target
        .iter()
        .find(|service| service.name.as_str() == "web")
        .unwrap();
    assert_eq!(
        json!(web.config_mounts()),
        json!([{
            "config_name": "errors/config.yml", "target": "/old/config.yml",
            "mode": 0o444, "uid": 0, "gid": 0,
        }])
    );
}

const DOCKER_VOLUME: &str = "shop-production_vol-00000000-0000-4000-8000-000000000005";

/// The Servers answered, and Server `a` holds Volume `data`.
fn held_on_a() -> ployz_store::VolumeObservation {
    let name = ployz_core::DockerVolumeName::parse(DOCKER_VOLUME).unwrap();
    ployz_store::VolumeObservation {
        sought: vec![name.clone()],
        held: vec![ployz_core::DockerVolumeId {
            machine_id: ployz_core::MachineId::parse("a".repeat(32)).unwrap(),
            name,
        }],
        unanswered: Vec::new(),
    }
}

/// Deploy Volume `data` on `web`, then remove it and Save, accepting its loss
/// while Server `a` holds it.
fn saved_volume_loss() -> (ConfigStore, Actor) {
    let (store, who) = shop();
    let data = || ployz_store::VolumeName::parse("data").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateVolume {
                shared_writes: false,
                storage_locked: false,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: ployz_store::VolumeId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: data(),
                mounts: vec![ployz_store::Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
    backend::deploy(&store, &who, "production", 1);
    store
        .write(
            &who,
            &ployz_store::RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: data(),
            },
        )
        .unwrap();
    let save = |version: Option<String>| {
        store.write_trusted(
            &who,
            &Publish {
                environment: EnvironmentRef::default(),
                version,
                message: None,
                accept_volume_loss: vec![data()],
            },
            &Trusted {
                volumes: Some(held_on_a()),
                ..Trusted::default()
            },
        )
    };
    let refused = save(None).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
    save(Some(
        refused.details["version"].as_str().unwrap().to_owned(),
    ))
    .unwrap();
    (store, who)
}

/// What the push's Deployment deletes, as `machine:docker volume`.
fn pushed_deletes(
    store: &ConfigStore,
    volumes: Option<ployz_store::VolumeObservation>,
) -> Vec<String> {
    deletes_of(store, &push(None, Some(H1), None), volumes)
}

/// What the Deployment `event` admits deletes, as `machine:docker volume`.
fn deletes_of(
    store: &ConfigStore,
    event: &SystemEvent,
    volumes: Option<ployz_store::VolumeObservation>,
) -> Vec<String> {
    let Written::Automated(automated) = store
        .system(
            &org(),
            event,
            &Trusted {
                volumes,
                ..Trusted::default()
            },
        )
        .unwrap()
    else {
        panic!("a system event writes Automated")
    };
    store
        .claim(
            &automated.admitted[0].deployment.id,
            &ployz_store::RunnerId::parse("runner").unwrap(),
        )
        .unwrap()
        .deletes
        .iter()
        .map(|volume| format!("{}:{}", volume.machine_id, volume.name))
        .collect()
}

#[test]
fn a_push_after_a_save_that_accepted_a_loss_deletes_the_data() {
    let (store, who) = saved_volume_loss();
    let consented = store
        .read(&who, &ployz_store::ConsentedQuery::default())
        .unwrap();
    assert_eq!(texts(&consented.volumes), [DOCKER_VOLUME]);
    assert_eq!(
        pushed_deletes(&store, Some(held_on_a())),
        [format!("{}:{DOCKER_VOLUME}", "a".repeat(32))]
    );
}

#[test]
fn a_push_keeps_saved_loss_data_without_the_same_evidence() {
    let (store, _) = saved_volume_loss();
    assert!(pushed_deletes(&store, None).is_empty());
    let (store, _) = saved_volume_loss();
    let mut more = held_on_a();
    more.held.push(ployz_core::DockerVolumeId {
        machine_id: ployz_core::MachineId::parse("b".repeat(32)).unwrap(),
        name: ployz_core::DockerVolumeName::parse(DOCKER_VOLUME).unwrap(),
    });
    assert!(pushed_deletes(&store, Some(more)).is_empty());
}

#[test]
fn a_push_keeps_saved_loss_data_while_a_server_is_unanswered_or_unsought() {
    let (store, _) = saved_volume_loss();
    let mut unanswered = held_on_a();
    unanswered
        .unanswered
        .push(ployz_core::MachineId::parse("b".repeat(32)).unwrap());
    assert!(pushed_deletes(&store, Some(unanswered)).is_empty());
    let (store, _) = saved_volume_loss();
    let mut unsought = held_on_a();
    unsought.sought.clear();
    assert!(pushed_deletes(&store, Some(unsought)).is_empty());
}

#[test]
fn a_push_that_leaves_out_the_service_mounting_the_volume_keeps_its_data() {
    let (store, who) = saved_volume_loss();
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    system(&store, &push(None, Some(H1), None));
    // Consent stays for a later push that ships web.
    let narrow = push(Some(H1), Some(H2), Some(&["api/main.rs"]));
    assert!(deletes_of(&store, &narrow, Some(held_on_a())).is_empty());
    assert_eq!(
        deletes_of(&store, &push(Some(H2), Some(H3), None), Some(held_on_a())),
        [format!("{}:{DOCKER_VOLUME}", "a".repeat(32))]
    );
}

#[test]
fn a_merge_keeps_the_destinations_accepted_loss() {
    let (store, who) = saved_volume_loss();
    open_pull_request(&store, &who);
    store
        .write(
            &who,
            &Command::Edit(Edit {
                environment: EnvironmentRef {
                    project: None,
                    environment: Some(ployz_store::EnvironmentName::parse("pr-5").unwrap()),
                },
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("web.env.SEARCH").unwrap(),
                    value: json!("on"),
                }],
            }),
        )
        .unwrap();
    system(&store, &pull_request(false));
    assert_eq!(
        pushed_deletes(&store, Some(held_on_a())),
        [format!("{}:{DOCKER_VOLUME}", "a".repeat(32))]
    );
}

/// Turn on pull request environments from `production`, and open pull request #5.
fn open_pull_request(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &ployz_store::SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(ployz_store::EnvironmentName::parse("production").unwrap()),
                copy: None,
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    system(store, &pull_request(true));
}

/// Pull request #5 onto `main`: open, or merged.
fn pull_request(open: bool) -> SystemEvent {
    SystemEvent::PullRequest(ployz_store::PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        title: "Add search".into(),
        author: "ada".into(),
        bot: false,
        head_branch: backend::git_branch("search"),
        head: backend::sha(H2),
        target_branch: backend::git_branch("main"),
        commits: 1,
        open,
        merge_commit: (!open).then(|| backend::sha(H3)),
        target_head: None,
        updated: ployz_store::GithubTimestamp::parse(if open {
            "2026-09-29T10:00:00Z"
        } else {
            "2026-09-29T11:00:00Z"
        })
        .unwrap(),
    })
}

fn texts<T: ToString>(items: &[T]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
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

#[test]
fn a_push_deploy_refuses_a_hostname_another_namespace_publishes() {
    let (store, who) = shop();
    let trusted = Trusted::default();
    let hostname = ployz_store::Hostname::parse("shop.example.com").unwrap();
    store
        .write_trusted(
            &who,
            &ployz_store::AddDomain {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                hostname: Some(hostname.clone()),
                prefix: None,
                port: None,
            },
            &trusted,
        )
        .unwrap();
    publish(&store, &who);
    let mut published = Trusted::default();
    published
        .domains
        .published
        .push(ployz_store::PublishedHostname {
            hostname,
            namespace: ployz_core::Namespace::parse("gone-production").unwrap(),
            service: ServiceName::parse("web").unwrap(),
        });
    let Written::Automated(automated) = store
        .system(&org(), &push(None, Some(H1), None), &published)
        .unwrap()
    else {
        panic!("a system event writes Automated")
    };
    assert!(automated.admitted.is_empty());
    assert_eq!(automated.skipped.len(), 1, "{automated:?}");
    assert!(automated.skipped[0].reason.contains("shop.example.com"));
}

#[test]
fn a_status_or_conclusion_github_adds_later_reads_other() {
    let status: ployz_store::CheckStatus = serde_json::from_value(json!("requested")).unwrap();
    assert_eq!(status, ployz_store::CheckStatus::Other);
    let conclusion: ployz_store::CheckConclusion =
        serde_json::from_value(json!("brand_new")).unwrap();
    assert_eq!(conclusion, ployz_store::CheckConclusion::Other);
}
