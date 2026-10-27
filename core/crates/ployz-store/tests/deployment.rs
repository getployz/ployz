#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Admission, runner ownership, replay and partial Node Outcomes, through the Store's
//! interface only, on SQLite and on Postgres (see `backend`).

use ployz_core::config::ReviewLifecycleKind;
use ployz_core::{
    ConfigFileName, ConfigName, DeployOutcome, DeployPreview, ExecutionError, MachineId,
    MachineName, OperationRow, QualifiedService, RpcError, RpcErrorCode, ServiceName,
};
use ployz_store::tools::{LineCap, LogExcerpt, LogText, SealedValues, Withheld};
use ployz_store::{
    Actor, Admit, Approval, ApprovalDigest, Asks, AttachConfig, Cancel, Change, Command, ConfigId,
    ConfigMountAt, ConfigStore, ConfigsQuery, CreateConfig, CreateProject, CreateService,
    DeleteConfig, Deploy, DeploymentId, DeploymentStatus, DeploymentSummary, DeploymentWarning,
    DeploymentsQuery, DetachConfig, DiffQuery, DiffView, Discard, Edit, EnvironmentId,
    EnvironmentRef, FailedAt, IngressAction, IngressFailure, IngressRepair, NamespaceQuery,
    NodeStatus, OrganizationId, Permission, PlanQuery, Principal, ProjectId, ProjectName, Publish,
    PutConfigFile, Query, RemoveService, RenameConfig, RenameService, Retry, Revision, RowPhase,
    RowState, RowTracker, RunEvidence, RunnerId, RunningObservation, ServerRow, ServiceLineageId,
    ServiceQuery, ServicesQuery, SettingPath, Start, StoppedBy, Trusted, UploadBase,
    UploadedSource, View, Written,
};
use serde_json::{Value, json};

mod backend;

const PROJECT: &str = "00000000-0000-4000-8000-000000000001";
const ENVIRONMENT: &str = "00000000-0000-4000-8000-000000000002";

/// A store with Project `shop` and new Services `web` and `api`.
fn shop() -> (ConfigStore, Actor) {
    shop_in(backend::open())
}

fn shop_in(store: ConfigStore) -> (ConfigStore, Actor) {
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(PROJECT).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(ENVIRONMENT).unwrap(),
            },
        )
        .unwrap();
    for (n, name) in [(3, "web"), (4, "api")] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some("nginx:1".into()),
                    template: None,
                },
            )
            .unwrap();
    }
    (store, who)
}

fn id(n: u8) -> DeploymentId {
    DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap()
}

fn runner(name: &str) -> RunnerId {
    RunnerId::parse(name).unwrap()
}

fn admit(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    services: &[&str],
    version: Option<String>,
) -> Result<DeploymentSummary, RpcError> {
    store.write_trusted(
        who,
        &Admit::Deploy(Deploy {
            id: id(n),
            environment: EnvironmentRef::default(),
            services: services
                .iter()
                .map(|name| ServiceName::parse(*name).unwrap())
                .collect(),
            version,
            upload: None,
            accept_volume_loss: Vec::new(),
            message: None,
            prune: false,
            no_prune: false,
        }),
        &Trusted::default(),
    )
}

fn diff(store: &ConfigStore, who: &Actor) -> DiffView {
    store.read(who, &DiffQuery::default()).unwrap()
}

fn changed(store: &ConfigStore, who: &Actor) -> Vec<String> {
    diff(store, who)
        .changes
        .into_iter()
        .map(|change| change.name)
        .collect()
}

fn configs(store: &ConfigStore, who: &Actor) -> Vec<(String, bool, Option<ReviewLifecycleKind>)> {
    store
        .read(who, &ConfigsQuery::default())
        .unwrap()
        .configs
        .into_iter()
        .map(|listing| {
            (
                listing.config.name.to_string(),
                listing.deployed,
                listing.change,
            )
        })
        .collect()
}

fn set_replicas(store: &ConfigStore, who: &Actor, service: &str, replicas: u8) {
    store
        .write(
            who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse(&format!("{service}.replicas")).unwrap(),
                    value: json!(replicas),
                }],
            },
        )
        .unwrap();
}

/// A distinct operation per Service, attributed to it.
fn operation(service: &str) -> Value {
    let container = match service {
        "web" => "a",
        "worker" => "c",
        _ => "b",
    };
    json!({"type": "remove_container", "machine_id": "a".repeat(32), "container_id": container.repeat(64)})
}

fn preview(services: &[&str]) -> DeployPreview {
    serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": services.iter().enumerate().map(|(index, service)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": service,
            "operation": operation(service), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap()
}

fn succeeded(services: &[&str]) -> RunEvidence {
    RunEvidence::Executed {
        progress: Vec::new(),
        outcome: Box::new(outcome(json!({
            "type": "success",
            "completed": services.iter().map(|service| operation(service)).collect::<Vec<_>>()
        }))),
        removed: Vec::new(),
    }
}

fn outcome(value: Value) -> DeployOutcome<ExecutionError> {
    serde_json::from_value(value).unwrap()
}

fn code(result: Result<impl std::fmt::Debug, RpcError>) -> RpcErrorCode {
    result.unwrap_err().code
}

/// The nodes Deployment `n` marks cancelled: Failed, read Cancelled.
fn cancelled_nodes(store: &ConfigStore, who: &Actor, n: u8) -> Vec<String> {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id(n) })
        .unwrap()
        .nodes
        .into_iter()
        .filter(|node| node.cancelled)
        .map(|node| node.node.name().to_owned())
        .collect()
}

/// Whether Deployment `n`'s outcome says its cancel stopped the operation it names.
fn outcome_cancelled(store: &ConfigStore, who: &Actor, n: u8) -> bool {
    let outcome = store
        .read(who, &ployz_store::DeploymentQuery { id: id(n) })
        .unwrap()
        .deployment
        .outcome;
    let Some(ployz_store::Outcome::Executed { cancelled, .. }) = outcome else {
        panic!("expected an executed outcome, got {outcome:?}");
    };
    cancelled
}

fn nodes(store: &ConfigStore, who: &Actor, n: u8) -> Vec<(String, NodeStatus)> {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id(n) })
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| (node.node.name().to_owned(), node.outcome))
        .collect()
}

#[test]
fn diff_and_a_deployment_list_each_kind_of_node_by_name_not_by_id() {
    let (store, who) = shop();
    sentry(&store, &who);
    // web's id sorts before api's.
    assert_eq!(changed(&store, &who), ["api", "web", "sentry"]);
    admit(&store, &who, 1, &[], None).unwrap();
    assert_eq!(
        nodes(&store, &who, 1)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        ["api", "web", "sentry"]
    );
}

#[test]
fn a_recreated_service_lists_after_the_one_it_replaces_whichever_id_sorts_first() {
    // The replacement's id sorts below the original's (…03) in one run, above it in the other.
    for replacement in [
        "00000000-0000-4000-8000-000000000000",
        "00000000-0000-4000-8000-0000000000ff",
    ] {
        let (store, who) = shop();
        backend::deploy(&store, &who, "production", 1);
        store
            .write(
                &who,
                &RemoveService {
                    environment: EnvironmentRef::default(),
                    service: ServiceName::parse("web").unwrap(),
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(replacement).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("web").unwrap(),
                    image: Some("nginx:2".into()),
                    template: None,
                },
            )
            .unwrap();
        let view = diff(&store, &who);
        for changes in [view.changes.clone(), view.review_changes()] {
            let listed: Vec<_> = changes
                .iter()
                .map(|change| (change.name.as_str(), change.lifecycle))
                .collect();
            assert_eq!(
                listed,
                [
                    ("web", ReviewLifecycleKind::Delete),
                    ("web", ReviewLifecycleKind::Create)
                ],
                "replacement {replacement}"
            );
        }
        admit(&store, &who, 2, &[], None).unwrap();
        let ids: Vec<_> = store
            .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
            .unwrap()
            .nodes
            .into_iter()
            .map(|node| node.node.id().to_owned())
            .collect();
        assert_eq!(
            ids,
            [
                "00000000-0000-4000-8000-000000000004",
                "00000000-0000-4000-8000-000000000003",
                replacement
            ],
            "replacement {replacement}"
        );
    }
}

#[test]
fn a_deploy_publishes_then_its_runner_records_it_into_applied_state() {
    let (store, who) = shop();
    let admitted = admit(&store, &who, 1, &[], None).unwrap();
    assert_eq!(
        (admitted.status, admitted.saved, admitted.number),
        (DeploymentStatus::Queued, Revision(1), Some(1))
    );
    // The submitted Deployment is Head now, so nothing is staged.
    assert!(changed(&store, &who).is_empty());

    let a = runner("runner-a");
    let claimed = store.claim(&id(1), &a).unwrap();
    assert_eq!(claimed.deployment.status, DeploymentStatus::Running);
    assert_eq!(claimed.intent.namespace.as_str(), "shop-production");
    assert_eq!(claimed.intent.target.len(), 2);
    assert!(claimed.intent.options.selected.is_empty());
    assert_eq!(store.claim(&id(1), &a).unwrap(), claimed);
    assert_eq!(
        code(store.claim(&id(1), &runner("runner-b"))),
        RpcErrorCode::Conflict
    );

    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    // Recording the same evidence twice changes nothing; another runner never may.
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    assert_eq!(
        code(store.record(&id(1), &runner("runner-b"), succeeded(&["web", "api"]))),
        RpcErrorCode::Conflict
    );
    let recorded = store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(
        recorded,
        store
            .record(&id(1), &a, succeeded(&["web", "api"]))
            .unwrap()
    );
    assert_eq!(recorded.status, DeploymentStatus::Applied);
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::Deployed),
            ("web".to_owned(), NodeStatus::Deployed)
        ]
    );
    assert!(
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
            .unwrap()
            .preview
            .is_some()
    );
    assert!(changed(&store, &who).is_empty());
    assert_eq!(code(store.claim(&id(1), &a)), RpcErrorCode::Conflict);

    // Applied State is the new Head: a later edit shows against it.
    set_replicas(&store, &who, "web", 3);
    assert_eq!(changed(&store, &who), ["web"]);

    // A Deploy of every Service settles the ones its preview plans nothing for
    // as soon as it records that preview, not once it ends.
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    assert_eq!(
        nodes(&store, &who, 2),
        [
            ("api".to_owned(), NodeStatus::Unchanged),
            ("web".to_owned(), NodeStatus::Pending)
        ]
    );
    // Ending without executing leaves what the preview settled: nothing ran on api either way.
    store
        .record(&id(2), &a, RunEvidence::NotExecuted("stopped".into()))
        .unwrap();
    assert_eq!(
        nodes(&store, &who, 2),
        [
            ("api".to_owned(), NodeStatus::Unchanged),
            ("web".to_owned(), NodeStatus::NotAttempted)
        ]
    );
}

#[test]
fn a_failed_ingress_repair_leaves_the_deploy_applied_with_a_warning() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let taken = IngressFailure::PortTaken {
        cause: "failed to bind host port 0.0.0.0:80/tcp: address already in use".into(),
    };
    let repairs = vec![
        IngressRepair {
            machine: MachineId::parse("a".repeat(32)).unwrap(),
            server: "web-1".into(),
            action: IngressAction::Create,
            failure: Some(taken.clone()),
        },
        IngressRepair {
            machine: MachineId::parse("b".repeat(32)).unwrap(),
            server: "web-2".into(),
            action: IngressAction::Start,
            failure: None,
        },
    ];
    store
        .record(&id(1), &a, RunEvidence::Ingress(repairs.clone()))
        .unwrap();
    let recorded = store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();

    let warning = DeploymentWarning::IngressNotServing {
        server: "web-1".into(),
        ingress: taken,
    };
    assert_eq!(recorded.status, DeploymentStatus::Applied);
    assert_eq!(recorded.warnings, std::slice::from_ref(&warning));
    assert_eq!(recorded.ingress, repairs);
    let read = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap()
        .deployment;
    assert_eq!((read.warnings, read.ingress), (vec![warning], repairs));
}

#[test]
fn an_unchecked_ingress_leaves_the_deploy_applied_with_one_warning() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let cause = "Couldn't list the Cluster's Servers.".to_owned();
    for _ in 0..2 {
        store
            .record(&id(1), &a, RunEvidence::IngressUnchecked(cause.clone()))
            .unwrap();
    }
    let recorded = store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();

    let warning = DeploymentWarning::IngressUnchecked { cause };
    assert_eq!(recorded.status, DeploymentStatus::Applied);
    assert_eq!(recorded.warnings, std::slice::from_ref(&warning));
    let read = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap()
        .deployment;
    assert_eq!(read.warnings, [warning]);
}

#[test]
fn a_deploy_warning_shows_once_on_the_applied_deploy() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let mismatch = ployz_core::DeployWarning::ImagePortMismatch {
        service: "web".parse().unwrap(),
        service_id: ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
        image: "nginx".into(),
        machines: vec![MachineId::parse("a".repeat(32)).unwrap()],
        exposed: vec![80],
        routed: vec![8080],
    };
    for _ in 0..2 {
        store
            .record(&id(1), &a, RunEvidence::Warned(mismatch.clone()))
            .unwrap();
    }
    let recorded = store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();

    let warning = DeploymentWarning::Deploy { warning: mismatch };
    assert_eq!(recorded.status, DeploymentStatus::Applied);
    assert_eq!(recorded.warnings, std::slice::from_ref(&warning));
    assert_eq!(
        serde_json::to_value(&warning).unwrap(),
        serde_json::json!({"warning": {
            "type": "image_port_mismatch",
            "service": "web",
            "service_id": "c".repeat(32),
            "image": "nginx",
            "machines": ["a".repeat(32)],
            "exposed": [80],
            "routed": [8080],
        }})
    );
    let read = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap()
        .deployment;
    assert_eq!(read.warnings, [warning]);
}

fn sentry(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &CreateConfig {
                id: ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("sentry").unwrap(),
                mounts: vec![ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    put_sentry(store, who, "url: http://api:${{ api.PORT }}\n");
}

fn put_sentry(store: &ConfigStore, who: &Actor, content: &str) {
    store
        .write(
            who,
            &PutConfigFile {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("sentry").unwrap(),
                file: ConfigFileName::parse("config.yml").unwrap(),
                content: content.into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
}

/// A replayed edit must not rotate a secret that referrers and an initialized
/// database already use, and generate seals over a plain value.
#[test]
fn a_replayed_generate_keeps_its_secret_and_seals_over_plain_text() {
    let (store, who) = shop();
    let keypair = ployz_core::Keypair::generate();
    let trusted = Trusted {
        sealing: Some(ployz_store::SealingRecipient {
            public_key: keypair.public(),
        }),
        ..Trusted::default()
    };
    let edit = |changes: Vec<Change>| {
        store
            .write_trusted(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes,
                },
                &trusted,
            )
            .unwrap();
    };
    let set = |path: &str, value: Value| Change::Set {
        path: SettingPath::parse(path).unwrap(),
        value,
    };
    edit(vec![set("web.env.TOKEN", json!("plain-token"))]);
    let generate = || {
        ["web.env.PASSWORD", "web.env.TOKEN"]
            .map(|path| set(path, json!({ "secret": { "generate": true } })))
            .to_vec()
    };
    let a = runner("a");
    let mut claimed_env = Vec::new();
    for n in 1..=2 {
        edit(generate());
        admit(&store, &who, n, &[], None).unwrap();
        let claimed = store.claim(&id(n), &a).unwrap();
        let web = claimed
            .intent
            .target
            .iter()
            .find(|spec| spec.name.as_str() == "web")
            .unwrap();
        let identity = web.container.sealed_identity.clone().unwrap();
        let opened = |key: &str| {
            let ployz_core::EnvValue::Sealed(value) = &web.container.environment[key] else {
                panic!("{:?}", web.container.environment);
            };
            let sealed = ployz_core::config::ClientSealedValue {
                sealed_for: identity.environment,
                value: value.clone(),
            };
            (value.clone(), keypair.open(&identity, &sealed).unwrap())
        };
        claimed_env.push((opened("PASSWORD"), opened("TOKEN")));
        store
            .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
            .unwrap();
        store
            .record(&id(n), &a, succeeded(&["web", "api"]))
            .unwrap();
    }
    let ((_, password), (_, token)) = &claimed_env[0];
    for secret in [password, token] {
        assert_eq!(secret.len(), 32, "{secret}");
        assert!(secret.chars().all(|c| c.is_ascii_alphabetic()), "{secret}");
    }
    assert_ne!(password, token);
    assert_eq!(
        claimed_env[0], claimed_env[1],
        "a replayed generate rotated the secret"
    );
}

#[test]
fn a_config_deploys_with_the_services_that_mount_it() {
    let (store, who) = shop();
    sentry(&store, &who);
    admit(&store, &who, 1, &[], None).unwrap();
    assert!(changed(&store, &who).is_empty());
    let a = runner("runner-a");
    let claimed = store.claim(&id(1), &a).unwrap();
    let web = claimed
        .intent
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "web")
        .unwrap();
    assert_eq!(
        json!(web.configs()),
        json!([{"name": "sentry/config.yml", "content": b"url: http://api:8080\n".to_vec()}])
    );
    assert_eq!(
        json!(web.config_mounts()),
        json!([{"config_name": "sentry/config.yml", "target": "/etc/sentry/config.yml",
            "uid": 0, "gid": 0, "mode": 0o444}])
    );
    let web_name = ServiceName::parse("web").unwrap();
    assert_eq!(
        claimed.intent.dependencies()[&web_name][0].service.as_str(),
        "api"
    );
    assert!(claimed.deployment.warnings.is_empty());
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let sentry_is = |n: u8| {
        nodes(&store, &who, n)
            .into_iter()
            .find(|(name, _)| name == "sentry")
            .map(|(_, outcome)| outcome)
    };
    assert_eq!(sentry_is(1), Some(NodeStatus::Pending));
    assert_eq!(configs(&store, &who), [("sentry".to_owned(), false, None)]);
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(sentry_is(1), Some(NodeStatus::Deployed));
    assert_eq!(configs(&store, &who), [("sentry".to_owned(), true, None)]);
    assert!(
        changed(&store, &who).is_empty(),
        "Applied State holds Config and mount"
    );

    admit(&store, &who, 2, &["api"], None).unwrap();
    assert_eq!(sentry_is(2), None);
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&[])))
        .unwrap();
    store.record(&id(2), &a, succeeded(&[])).unwrap();

    put_sentry(&store, &who, "url: changed\n");
    admit(&store, &who, 3, &[], None).unwrap();
    store.claim(&id(3), &a).unwrap();
    store
        .record(&id(3), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(
            &id(3),
            &a,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome(json!({
                    "type": "failed", "completed": [operation("api")],
                    "failed": {"type": "operation", "operation": operation("web"), "error": {
                        "type": "machine", "action": "RemoveContainer",
                        "error": {"code": "internal", "message": "busy", "details": {}}
                    }},
                    "unexecuted": []
                }))),
                removed: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(sentry_is(3), Some(NodeStatus::Failed));
    assert_eq!(changed(&store, &who), ["sentry"]);

    store
        .write(
            &who,
            &DetachConfig {
                environment: EnvironmentRef::default(),
                service: web_name.clone(),
                config: ConfigName::parse("sentry").unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &DeleteConfig {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("sentry").unwrap(),
            },
        )
        .unwrap();
    admit(&store, &who, 4, &[], None).unwrap();
    assert!(changed(&store, &who).is_empty());
    store.claim(&id(4), &a).unwrap();
    store
        .record(&id(4), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    assert_eq!(sentry_is(4), Some(NodeStatus::Pending));
    store.record(&id(4), &a, succeeded(&["web"])).unwrap();
    assert_eq!(sentry_is(4), Some(NodeStatus::Removed));
    assert!(changed(&store, &who).is_empty());
    assert!(configs(&store, &who).is_empty());
}

#[test]
fn a_deploy_refuses_config_that_expands_past_what_a_service_mounts() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("api.env.BIG").unwrap(),
                    value: json!("x".repeat(64 * 1024)),
                }],
            },
        )
        .unwrap();
    sentry(&store, &who);
    put_sentry(&store, &who, &"${{ api.BIG }}\n".repeat(13));

    let refused = admit(&store, &who, 1, &[], None).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert!(refused.message.contains("web"), "{}", refused.message);
    assert!(
        refused.message.contains("Config sentry"),
        "{}",
        refused.message
    );
    assert!(refused.message.contains("768 KB"), "{}", refused.message);
}

#[test]
fn a_deployed_configs_rename_and_file_edits_discard_by_their_rows() {
    let (store, who) = shop();
    sentry(&store, &who);
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    store
        .write(
            &who,
            &RenameConfig {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("sentry").unwrap(),
                name: ConfigName::parse("errors").unwrap(),
            },
        )
        .unwrap();
    for (file, content) in [
        ("config.yml", "url: changed\n"),
        ("extra.yml", "on: true\n"),
    ] {
        store
            .write(
                &who,
                &PutConfigFile {
                    environment: EnvironmentRef::default(),
                    config: ConfigName::parse("errors").unwrap(),
                    file: ConfigFileName::parse(file).unwrap(),
                    content: content.into(),
                    mode: None,
                    uid: None,
                    gid: None,
                },
            )
            .unwrap();
    }
    let rows: Vec<(String, bool)> = diff(&store, &who)
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .map(|row| (row.path.clone(), row.can_restore))
        .collect();
    assert_eq!(
        rows,
        [
            (
                "configs.@00000000-0000-4000-8000-000000000009.name".to_owned(),
                true
            ),
            (
                "configs.@00000000-0000-4000-8000-000000000009.files.config.yml".to_owned(),
                true
            ),
            (
                "configs.@00000000-0000-4000-8000-000000000009.files.extra.yml".to_owned(),
                true
            ),
        ]
    );
    for (path, _) in &rows {
        assert_eq!(&SettingPath::parse(path).unwrap().to_string(), path);
    }
    let discard = |path: &str| {
        store.write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse(path).unwrap()),
                version: None,
            },
        )
    };
    discard(&rows[2].0).unwrap();
    discard(&rows[1].0).unwrap();
    assert_eq!(changed(&store, &who), ["errors"], "the rename stays staged");
    discard(&rows[0].0).unwrap();
    assert!(diff(&store, &who).changes.is_empty());
    assert_eq!(configs(&store, &who), [("sentry".to_owned(), true, None)]);
    assert_eq!(
        SettingPath::parse("configs.sentry.mode").unwrap_err().code,
        RpcErrorCode::InvalidArgument
    );
}

#[test]
fn a_config_deleted_while_its_deploy_is_in_flight_lists_as_a_delete() {
    let (store, who) = shop();
    sentry(&store, &who);
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .write(
            &who,
            &DetachConfig {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                config: ConfigName::parse("sentry").unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &DeleteConfig {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("sentry").unwrap(),
            },
        )
        .unwrap();
    let delete = Some(ReviewLifecycleKind::Delete);
    assert_eq!(
        configs(&store, &who),
        [("sentry".to_owned(), false, delete)]
    );
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(configs(&store, &who), [("sentry".to_owned(), true, delete)]);
}

fn shared_sentry(store: &ConfigStore, who: &Actor) {
    sentry(store, who);
    put_sentry(store, who, "value: old\n");
    store
        .write(
            who,
            &AttachConfig {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("api").unwrap(),
                config: ConfigName::parse("sentry").unwrap(),
                dir: "/etc/sentry".into(),
            },
        )
        .unwrap();
}

#[test]
fn an_existing_shared_config_keeps_its_edit_after_a_mounter_fails() {
    let (store, who) = shop();
    shared_sentry(&store, &who);
    backend::deploy(&store, &who, "production", 1);
    put_sentry(&store, &who, "value: new\n");
    admit(&store, &who, 2, &[], None).unwrap();
    fail_api(&store, 2);

    assert_eq!(changed(&store, &who), ["sentry"]);
    assert_eq!(
        nodes(&store, &who, 2),
        [
            ("api".to_owned(), NodeStatus::Failed),
            ("web".to_owned(), NodeStatus::Deployed),
            ("sentry".to_owned(), NodeStatus::Failed),
        ]
    );
}

#[test]
fn an_existing_shared_config_waits_for_pending_mounters() {
    let (store, who) = shop();
    shared_sentry(&store, &who);
    backend::deploy(&store, &who, "production", 1);
    put_sentry(&store, &who, "value: new\n");
    admit(&store, &who, 2, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(
            &id(2),
            &a,
            RunEvidence::Confirmed(vec![ServiceName::parse("web").unwrap()]),
        )
        .unwrap();
    assert_eq!(
        nodes(&store, &who, 2),
        [
            ("api".to_owned(), NodeStatus::Pending),
            ("web".to_owned(), NodeStatus::Deployed),
            ("sentry".to_owned(), NodeStatus::Pending),
        ]
    );
    store.record(&id(2), &a, RunEvidence::Abandoned).unwrap();
    assert_eq!(changed(&store, &who), ["sentry"]);
}

#[test]
fn a_shared_configs_outcome_follows_its_mounters_when_an_unrelated_service_fails() {
    for confirmed_early in [false, true] {
        for (completed, unexecuted, expected) in [
            (vec!["web", "api"], vec![], NodeStatus::Deployed),
            (vec!["web"], vec!["api"], NodeStatus::NotAttempted),
        ] {
            let (store, who) = shop();
            shared_sentry(&store, &who);
            store
                .write(
                    &who,
                    &CreateService {
                        id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005")
                            .unwrap(),
                        environment: EnvironmentRef::default(),
                        name: ServiceName::parse("worker").unwrap(),
                        image: Some("nginx:1".into()),
                        template: None,
                    },
                )
                .unwrap();
            backend::deploy(&store, &who, "production", 1);
            put_sentry(&store, &who, "value: new\n");
            admit(&store, &who, 2, &[], None).unwrap();
            let a = runner("runner-a");
            store.claim(&id(2), &a).unwrap();
            store
                .record(
                    &id(2),
                    &a,
                    RunEvidence::Prepared(preview(&["web", "api", "worker"])),
                )
                .unwrap();
            if confirmed_early {
                store
                    .record(
                        &id(2),
                        &a,
                        RunEvidence::Confirmed(
                            completed
                                .iter()
                                .map(|name| ServiceName::parse(*name).unwrap())
                                .collect(),
                        ),
                    )
                    .unwrap();
            }
            store
                .record(
                    &id(2),
                    &a,
                    RunEvidence::Executed {
                        progress: Vec::new(),
                        outcome: Box::new(outcome(json!({
                            "type": "failed",
                            "completed": completed.iter().map(|name| operation(name)).collect::<Vec<_>>(),
                            "failed": {"type": "operation", "operation": operation("worker"),
                                "error": {"type": "cancelled"}},
                            "unexecuted": unexecuted.iter().map(|name| operation(name)).collect::<Vec<_>>()
                        }))),
                        removed: Vec::new(),
                    },
                )
                .unwrap();
            assert_eq!(status(&store, &who, 2), DeploymentStatus::Failed);
            assert_eq!(
                nodes(&store, &who, 2)
                    .into_iter()
                    .find(|(name, _)| name == "sentry")
                    .unwrap()
                    .1,
                expected,
            );
            let expected_changes: &[&str] = if expected == NodeStatus::Deployed {
                &[]
            } else {
                &["sentry"]
            };
            assert_eq!(changed(&store, &who), expected_changes);
        }
    }
}

#[test]
fn a_fresh_shared_config_keeps_a_confirmed_mount_valid_when_its_sibling_fails() {
    let (store, who) = shop();
    backend::deploy(&store, &who, "production", 1);
    shared_sentry(&store, &who);
    admit(&store, &who, 2, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(
            &id(2),
            &a,
            RunEvidence::Confirmed(vec![ServiceName::parse("web").unwrap()]),
        )
        .unwrap();
    assert_eq!(configs(&store, &who), [("sentry".to_owned(), true, None)]);
    store
        .record(
            &id(2),
            &a,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome(json!({
                    "type": "failed", "completed": [operation("web")],
                    "failed": {"type": "operation", "operation": operation("api"),
                        "error": {"type": "cancelled"}},
                    "unexecuted": []
                }))),
                removed: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(configs(&store, &who), [("sentry".to_owned(), true, None)]);
    assert_eq!(changed(&store, &who), ["api"]);
    let plan = store.read(&who, &PlanQuery::default()).unwrap();
    assert_eq!(plan.changes[0].name, "api");
}

#[test]
fn a_narrowed_deploy_leaves_a_shared_config_staged_until_every_mounter_redeploys() {
    let (store, who) = shop();
    sentry(&store, &who);
    store
        .write(
            &who,
            &AttachConfig {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("api").unwrap(),
                config: ConfigName::parse("sentry").unwrap(),
                dir: "/etc/sentry".into(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateConfig {
                id: ConfigId::parse("00000000-0000-4000-8000-000000000010").unwrap(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("unmounted").unwrap(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
    let a = runner("runner-a");
    let deploy = |n: u8, services: &[&str], ran: &[&str]| {
        admit(&store, &who, n, services, None).unwrap();
        let claimed = store.claim(&id(n), &a).unwrap();
        store
            .record(&id(n), &a, RunEvidence::Prepared(preview(ran)))
            .unwrap();
        store.record(&id(n), &a, succeeded(ran)).unwrap();
        claimed
    };
    let claimed = deploy(1, &[], &["web", "api"]);
    assert!(!claimed.input.to_string().contains("unmounted"));
    assert!(changed(&store, &who).is_empty());

    put_sentry(&store, &who, "url: changed\n");
    let restarts = |changes: Vec<ployz_store::NodeChange>| -> Vec<(String, Vec<String>)> {
        changes
            .into_iter()
            .map(|change| {
                let services = change.restarts.iter().map(ToString::to_string).collect();
                (change.name, services)
            })
            .collect()
    };
    assert_eq!(
        restarts(diff(&store, &who).changes),
        [(
            "sentry".to_owned(),
            vec!["web".to_owned(), "api".to_owned()]
        )]
    );
    let plan = |services: &[&str]| {
        let plan: ployz_store::PlanView = store
            .read(
                &who,
                &PlanQuery {
                    services: services
                        .iter()
                        .map(|name| ServiceName::parse(*name).unwrap())
                        .collect(),
                    ..PlanQuery::default()
                },
            )
            .unwrap();
        restarts(plan.changes)
    };
    assert_eq!(
        plan(&["web"]),
        [("sentry".to_owned(), vec!["web".to_owned()])],
        "api keeps the old file, so only web restarts"
    );
    deploy(2, &["web"], &["web"]);
    assert!(
        !nodes(&store, &who, 2)
            .iter()
            .any(|(name, _)| name == "sentry"),
        "api still runs the old file"
    );
    assert_eq!(changed(&store, &who), ["sentry"]);

    deploy(3, &[], &["web", "api"]);
    assert!(changed(&store, &who).is_empty());

    store
        .write(
            &who,
            &CreateConfig {
                id: ConfigId::parse("00000000-0000-4000-8000-000000000011").unwrap(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("fresh").unwrap(),
                mounts: vec![ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/fresh".into(),
                }],
            },
        )
        .unwrap();
    let fresh = diff(&store, &who)
        .changes
        .into_iter()
        .find(|change| change.name == "fresh")
        .unwrap();
    assert_eq!(fresh.lifecycle, ReviewLifecycleKind::Create);
    assert!(fresh.restarts.is_empty(), "a new Config restarts nothing");
}

#[test]
fn a_partial_outcome_applies_only_confirmed_nodes() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    // Recording an outcome before its preview is refused.
    assert_eq!(
        code(store.record(&id(1), &a, succeeded(&["web"]))),
        RpcErrorCode::Conflict
    );
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    // Evidence that doesn't match the preview is refused.
    assert_eq!(
        code(store.record(&id(1), &a, succeeded(&["web"]))),
        RpcErrorCode::InvalidArgument
    );
    let partial = |done: &str, failed: &str| RunEvidence::Executed {
        progress: Vec::new(),
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [operation(done)],
            "failed": {"type": "operation", "operation": operation(failed), "error": {
                "type": "machine", "action": "RemoveContainer",
                "error": {"code": "internal", "message": "the daemon is busy", "details": {}}
            }},
            "unexecuted": []
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, partial("web", "api")).unwrap();
    // The mirror image counts the same, yet says something else.
    assert_eq!(
        code(store.record(&id(1), &a, partial("api", "web"))),
        RpcErrorCode::Conflict
    );
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Failed);
    // The Deployment says why, in words users read.
    let Some(ployz_store::Outcome::Executed { reason, cause, .. }) = view.deployment.outcome else {
        panic!("an executed outcome");
    };
    assert_eq!(reason.as_deref(), Some("remove Container failed"));
    assert_eq!(cause, ["the daemon is busy"]);
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::Failed),
            ("web".to_owned(), NodeStatus::Deployed)
        ]
    );
    // Only `api` is still to deploy; a different outcome can't overwrite this one.
    assert_eq!(changed(&store, &who), ["api"]);
    assert_eq!(
        code(store.record(&id(1), &a, succeeded(&["web", "api"]))),
        RpcErrorCode::Conflict
    );
}

#[test]
fn an_executed_deployment_reads_where_each_service_ended() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let ended = json!([
        {"service": "web", "result": "new"},
        {"service": "api", "result": "old"},
    ]);
    store
        .record(
            &id(1),
            &a,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome(json!({
                    "type": "failed", "completed": [operation("web")],
                    "failed": {"type": "operation", "operation": operation("api"),
                        "error": {"type": "cancelled"}},
                    "unexecuted": [], "services": ended
                }))),
                removed: Vec::new(),
            },
        )
        .unwrap();
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    assert_eq!(
        serde_json::to_value(&view).unwrap()["outcome"]["services"],
        ended
    );
}

fn progress(rows: &[(&str, &str, Value)]) -> Vec<OperationRow> {
    serde_json::from_value(Value::Array(
        rows.iter()
            .enumerate()
            .map(|(index, (service, server, status))| {
                json!({
                    "index": index, "machine_id": server.chars().next().unwrap().to_string().repeat(32),
                    "machine_name": server, "service_name": service,
                    "operation": operation(service), "status": status
                })
            })
            .collect(),
    ))
    .unwrap()
}

fn waiting(elapsed_ms: u64) -> Value {
    json!({"type": "running", "phase": {
        "type": "waiting_for_health", "container_id": "c".repeat(64),
        "elapsed_ms": elapsed_ms, "deadline_ms": 60_000
    }})
}

fn rows(store: &ConfigStore, who: &Actor, n: u8) -> Vec<(String, Vec<ServerRow>)> {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id(n) })
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| (node.node.name().to_owned(), node.rows))
        .collect()
}

#[test]
fn final_progress_and_known_outcome_are_recorded_together() {
    let (store, who) = shop();
    admit(&store, &who, 1, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    let RunEvidence::Executed {
        outcome, removed, ..
    } = succeeded(&["web"])
    else {
        unreachable!()
    };
    let evidence = RunEvidence::Executed {
        outcome,
        removed,
        progress: RowTracker::default().changes(&progress(&[(
            "web",
            "alpha",
            json!({"type": "completed"}),
        )])),
    };
    store.record(&id(1), &a, evidence.clone()).unwrap();
    store.record(&id(1), &a, evidence).unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Applied);
    assert_eq!(
        nodes(&store, &who, 1),
        [("web".to_owned(), NodeStatus::Deployed)]
    );
    let original = rows(&store, &who, 1);
    assert_eq!(original[0].1[0].state, RowState::Completed);
    assert!(original[0].1[0].finished_at.is_some());
    admit(&store, &who, 2, &["web"], None).unwrap();
    store.claim(&id(2), &runner("runner-b")).unwrap();
    assert_eq!(rows(&store, &who, 1), original);
}

#[test]
fn terminal_progress_refusal_rolls_back_and_the_known_outcome_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let (store, who) =
        shop_in(ConfigStore::open(&format!("sqlite:{}", path.display()), backend::key()).unwrap());
    admit(&store, &who, 1, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    let mut evidence = succeeded(&["web"]);
    let RunEvidence::Executed {
        progress: final_rows,
        ..
    } = &mut evidence
    else {
        unreachable!()
    };
    *final_rows =
        RowTracker::default().changes(&progress(&[("web", "alpha", json!({"type":"completed"}))]));
    let sql = rusqlite::Connection::open(path).unwrap();
    sql.execute_batch("CREATE TRIGGER refuse_progress BEFORE INSERT ON config_deployment_row BEGIN SELECT RAISE(ABORT, 'injected Store failure'); END;").unwrap();
    assert!(store.record(&id(1), &a, evidence.clone()).is_err());
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Running);
    assert!(rows(&store, &who, 1)[0].1.is_empty());
    assert_ne!(nodes(&store, &who, 1)[0].1, NodeStatus::Deployed);
    sql.execute_batch("DROP TRIGGER refuse_progress").unwrap();
    store.record(&id(1), &a, evidence).unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Applied);
    assert_eq!(nodes(&store, &who, 1)[0].1, NodeStatus::Deployed);
    assert_eq!(rows(&store, &who, 1)[0].1[0].state, RowState::Completed);
}

#[test]
fn an_unattempted_row_has_no_start_time() {
    let (store, who) = shop();
    admit(&store, &who, 1, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    let mut tracker = RowTracker::default();
    for state in ["pending", "unexecuted"] {
        store
            .record(
                &id(1),
                &a,
                RunEvidence::Progress(tracker.changes(&progress(&[(
                    "web",
                    "alpha",
                    json!({"type": state}),
                )]))),
            )
            .unwrap();
    }
    let row = &rows(&store, &who, 1)[0].1[0];
    assert_eq!(row.state, RowState::NotAttempted);
    assert_eq!(row.started_at, None);
    assert!(row.finished_at.is_some());
}

#[test]
fn failed_rows_use_the_failed_operations_container() {
    let mut tracker = RowTracker::default();
    tracker.changes(&progress(&[
        ("web", "alpha", json!({"type": "running", "phase": {"type":"waiting_for_hook", "container_id":"a".repeat(64), "elapsed_ms":0,"deadline_ms":1000}})),
        ("web", "alpha", json!({"type":"pending"})),
    ]));
    let failed = json!({"type":"failed","error":{"type":"machine","action":"CreateContainer","error":{"code":"internal","message":"create failed","details":{}}}});
    let changed = tracker.changes(&progress(&[
        ("web", "alpha", json!({"type":"completed"})),
        ("web", "alpha", failed.clone()),
    ]));
    assert_eq!(
        tracker.container(&changed[0]),
        None,
        "the successful hook's Container is unrelated"
    );
    let mut tracker = RowTracker::default();
    tracker.changes(&progress(&[("web", "alpha", waiting(0))]));
    let mut inspect_failed = failed;
    inspect_failed["error"]["action"] = json!("InspectContainer");
    let changed = tracker.changes(&progress(&[("web", "alpha", inspect_failed)]));
    assert_eq!(
        tracker.container(&changed[0]),
        Some("c".repeat(64).parse().unwrap())
    );
}

#[test]
fn replacement_failure_logs_follow_the_old_or_new_container_that_failed() {
    let replacement: ployz_core::DeployOperation = serde_json::from_value(json!({
        "type":"replace_container", "machine_id":"a".repeat(32), "old_container_id":"b".repeat(64),
        "spec":{"service_id":"a".repeat(32),"name":"web","mode":{"mode":"replicated","replicas":1},"container":{"image":"nginx:1","pull_policy":"missing"}},
        "skip_health_monitor": false
    })).unwrap();
    for (phase, action, expected) in [
        ("stopping_container", "StopContainer", Some("b")),
        ("removing_container", "RemoveContainer", Some("b")),
        ("removing_container", "InspectContainer", Some("b")),
        ("creating_container", "CreateContainer", None),
        ("starting_container", "StartContainer", None),
    ] {
        let mut tracker = RowTracker::default();
        let mut snapshot = progress(&[("web", "alpha", waiting(0))]);
        snapshot[0].operation = replacement.clone();
        tracker.changes(&snapshot);
        snapshot[0].status =
            serde_json::from_value(json!({"type":"running","phase":{"type":phase}})).unwrap();
        tracker.changes(&snapshot);
        snapshot[0].status = serde_json::from_value(json!({"type":"failed","error":{"type":"machine","action":action,"error":{"code":"internal","message":"failed","details":{}}}})).unwrap();
        let changed = tracker.changes(&snapshot);
        assert_eq!(
            tracker.container(&changed[0]),
            expected.map(|id| id.repeat(64).parse().unwrap()),
            "{phase} {action}"
        );
    }
}

#[test]
fn dependency_and_hook_failures_keep_nested_rpc_causes() {
    let rpc = json!({"code":"unavailable","message":"Observation failed","details":{},"cause":["transport unavailable","connection refused"]});
    for (error, expected) in [
        (
            json!({"type":"dependency_health","dependency":"shop-production/api","failure":{"type":"observation","error":rpc}}),
            vec![
                "Container observation failed",
                "Observation failed",
                "transport unavailable",
                "connection refused",
            ],
        ),
        (
            json!({"type":"hook","container_id":"a".repeat(64),"failure":{"type":"timed_out","stop_error":rpc}}),
            vec![
                "timed out",
                "Stopping the hook failed",
                "Observation failed",
                "transport unavailable",
                "connection refused",
            ],
        ),
    ] {
        let error: ExecutionError = serde_json::from_value(error).unwrap();
        assert_eq!(ployz_store::Failure::of(&error, &[]).cause, expected);
    }
}

#[test]
fn deployment_rows_record_state_changes_once() {
    let (store, who) = shop();
    admit(&store, &who, 1, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    let mut tracker = RowTracker::default();
    let mut writes = 0;
    let mut observe = |status: Value| {
        let changed = tracker.changes(&progress(&[("web", "alpha", status)]));
        if !changed.is_empty() {
            writes += 1;
            store
                .record(&id(1), &a, RunEvidence::Progress(changed))
                .unwrap();
        }
    };
    observe(json!({"type": "pending"}));
    for elapsed in (0..=30_000).step_by(500) {
        observe(waiting(elapsed));
    }
    observe(json!({"type": "completed"}));
    assert_eq!(writes, 3, "pending, waiting for health, completed");
    let [(web, row)] = &rows(&store, &who, 1)[..] else {
        panic!("one node");
    };
    assert_eq!(web, "web");
    let [row] = &row[..] else {
        panic!("one row per Server");
    };
    assert_eq!(
        (row.server.as_str(), &row.state),
        ("alpha", &RowState::Completed)
    );
    assert!(row.started_at.is_some() && row.finished_at.is_some());
}

#[test]
fn a_row_history_lists_every_state_change_and_keeps_recurring_phases() {
    let (store, who) = shop();
    admit(&store, &who, 1, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    let compensating = json!({"type": "running", "phase": {"type": "compensating"}});
    let mut tracker = RowTracker::default();
    for status in [
        json!({"type": "pending"}),
        waiting(0),
        waiting(500),
        compensating,
        waiting(0),
    ] {
        let changes = tracker.changes(&progress(&[("web", "alpha", status)]));
        store
            .record(&id(1), &a, RunEvidence::Progress(changes))
            .unwrap();
    }
    let health = RowState::Running {
        phase: RowPhase::WaitingForHealth,
        serving: false,
    };
    let history = || {
        let view = rows(&store, &who, 1);
        let history = view[0].1[0].history.clone().unwrap();
        assert!(history.windows(2).all(|pair| pair[0].at <= pair[1].at));
        history
            .into_iter()
            .map(|entry| entry.state)
            .collect::<Vec<_>>()
    };
    let expected = vec![
        RowState::Pending,
        health.clone(),
        RowState::Running {
            phase: RowPhase::Compensating,
            serving: false,
        },
        health.clone(),
    ];
    assert_eq!(history(), expected);

    let again = RowTracker::default().changes(&progress(&[("web", "alpha", waiting(900))]));
    store
        .record(&id(1), &a, RunEvidence::Progress(again))
        .unwrap();
    assert_eq!(
        history(),
        expected,
        "re-recording the current state adds nothing"
    );
}

#[test]
fn settling_a_run_ends_each_unfinished_history_at_its_finish_time() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let error = json!({
        "type": "machine", "action": "RemoveContainer",
        "error": {"code": "internal", "message": "disk failure", "details": {}}
    });
    let changes = RowTracker::default().changes(&progress(&[
        ("web", "alpha", json!({"type": "failed", "error": error})),
        ("api", "beta", waiting(0)),
    ]));
    store
        .record(&id(1), &a, RunEvidence::Progress(changes))
        .unwrap();
    store
        .record(
            &id(1),
            &a,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome(json!({
                    "type": "failed", "completed": [],
                    "failed": {"type": "operation", "operation": operation("web"), "error": error},
                    "unexecuted": [operation("api")]
                }))),
                removed: Vec::new(),
            },
        )
        .unwrap();
    let view = rows(&store, &who, 1);
    let row = |service: &str| {
        view.iter()
            .find(|(name, _)| name == service)
            .map(|(_, rows)| rows[0].clone())
            .unwrap()
    };
    let web = row("web");
    let [failed] = &web.history.unwrap()[..] else {
        panic!("one state");
    };
    assert!(
        matches!(&failed.state, RowState::Failed { cause, log: None, .. } if cause == &["disk failure"]),
        "the log stays on the row, out of history"
    );
    assert!(web.state != failed.state, "the row itself keeps its log");
    let api = row("api");
    let states: Vec<_> = api
        .history
        .as_ref()
        .unwrap()
        .iter()
        .map(|entry| (entry.state.clone(), Some(entry.at)))
        .collect();
    assert_eq!(
        states,
        [
            (
                RowState::Running {
                    phase: RowPhase::WaitingForHealth,
                    serving: false
                },
                api.started_at
            ),
            (RowState::NotAttempted, api.finished_at),
        ]
    );
}

#[test]
fn terminal_row_retries_preserve_the_recorded_clocks() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    admit(&store, &who, 1, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    let error = json!({
        "type": "machine", "action": "RemoveContainer",
        "error": {"code": "internal", "message": "disk failure", "details": {}}
    });
    let changes = RowTracker::default().changes(&progress(&[
        ("web", "alpha", json!({"type": "completed"})),
        ("web", "beta", json!({"type": "failed", "error": error})),
        ("web", "charlie", json!({"type": "unexecuted"})),
    ]));
    store
        .record(&id(1), &a, RunEvidence::Progress(changes.clone()))
        .unwrap();
    let age_clocks = "UPDATE config_deployment_row SET \
        started = CASE WHEN started IS NULL THEN NULL ELSE 123 END, finished = 124";
    if let Some(path) = url.strip_prefix("sqlite:") {
        rusqlite::Connection::open(path)
            .unwrap()
            .execute_batch(age_clocks)
            .unwrap();
    } else {
        postgres::Client::connect(&url, postgres::NoTls)
            .unwrap()
            .batch_execute(age_clocks)
            .unwrap();
    }
    let original = rows(&store, &who, 1);
    assert_eq!(original[0].1.len(), 3);
    assert_eq!(original[0].1[0].state, RowState::Completed);
    assert!(matches!(original[0].1[1].state, RowState::Failed { .. }));
    assert_eq!(original[0].1[2].state, RowState::NotAttempted);
    assert!(original[0].1.iter().all(|row| row.finished_at == Some(124)));
    assert_eq!(original[0].1[2].started_at, None);
    store
        .record(&id(1), &a, RunEvidence::Progress(changes.clone()))
        .unwrap();
    assert_eq!(rows(&store, &who, 1), original);
    let finished = RunEvidence::Executed {
        progress: changes,
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [],
            "failed": {"type": "operation", "operation": operation("web"), "error": error},
            "unexecuted": []
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, finished.clone()).unwrap();
    assert_eq!(rows(&store, &who, 1), original);
    store.record(&id(1), &a, finished).unwrap();
    assert_eq!(rows(&store, &who, 1), original);
}

#[test]
fn a_failed_row_keeps_its_cause_chain_and_unfinished_rows_end_not_attempted() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let error = json!({
        "type": "machine", "action": "RemoveContainer",
        "error": {"code": "internal", "message": "the daemon is busy", "details": {},
                  "cause": ["connection refused"]}
    });
    let mut tracker = RowTracker::default();
    for snapshot in [
        progress(&[
            ("web", "alpha", json!({"type": "pending"})),
            ("api", "beta", json!({"type": "pending"})),
        ]),
        progress(&[
            ("web", "alpha", json!({"type": "failed", "error": error})),
            ("api", "beta", json!({"type": "pending"})),
        ]),
    ] {
        store
            .record(
                &id(1),
                &a,
                RunEvidence::Progress(tracker.changes(&snapshot)),
            )
            .unwrap();
    }
    store
        .record(
            &id(1),
            &a,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome(json!({
                    "type": "failed", "completed": [],
                    "failed": {"type": "operation", "operation": operation("web"), "error": error},
                    "unexecuted": [operation("api")]
                }))),
                removed: Vec::new(),
            },
        )
        .unwrap();
    let states: Vec<(String, String, RowState)> = rows(&store, &who, 1)
        .into_iter()
        .flat_map(|(node, rows)| {
            rows.into_iter()
                .map(move |row| (node.clone(), row.server, row.state))
        })
        .collect();
    assert_eq!(
        states,
        [
            ("api".to_owned(), "beta".to_owned(), RowState::NotAttempted),
            (
                "web".to_owned(),
                "alpha".to_owned(),
                RowState::Failed {
                    reason: "remove Container failed".into(),
                    cause: vec!["the daemon is busy".into(), "connection refused".into()],
                    step: Some(FailedAt::Remove),
                    log: Some(LogText::Withheld {
                        why: Withheld::Unanswered
                    }),
                    unanswered: false,
                    out_of_memory: None,
                    stopped_by: StoppedBy::NOTHING,
                }
            ),
        ]
    );
}

#[test]
fn mounted_resource_rows_keep_distinct_machines_and_failed_mounting_services() {
    for (resource, api_status) in ["data", "sentry"].into_iter().flat_map(|resource| {
        [
            json!({"type": "completed"}),
            json!({"type": "pending"}),
            waiting(1_000),
        ]
        .map(|status| (resource, status))
    }) {
        let api_finished = api_status["type"] == "completed";
        let (store, who) = shop();
        if resource == "data" {
            store
                .write(
                    &who,
                    &ployz_store::CreateVolume {
                        id: ployz_store::VolumeId::parse("00000000-0000-4000-8000-000000000005")
                            .unwrap(),
                        environment: EnvironmentRef::default(),
                        name: ployz_store::VolumeName::parse("data").unwrap(),
                        storage: ployz_core::config::VolumeKind::Docker {},
                        shared_writes: true,
                        storage_locked: false,
                        restored_from: None,
                        mounts: ["web", "api"]
                            .into_iter()
                            .map(|service| ployz_store::Mount {
                                service: ServiceName::parse(service).unwrap(),
                                path: "/data".into(),
                            })
                            .collect(),
                    },
                )
                .unwrap();
        } else {
            sentry(&store, &who);
            store
                .write(
                    &who,
                    &AttachConfig {
                        environment: EnvironmentRef::default(),
                        service: ServiceName::parse("api").unwrap(),
                        config: ConfigName::parse("sentry").unwrap(),
                        dir: "/etc/sentry".into(),
                    },
                )
                .unwrap();
        }
        admit(&store, &who, 1, &[], None).unwrap();
        let a = runner("runner-a");
        store.claim(&id(1), &a).unwrap();
        store
            .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
            .unwrap();
        let mut snapshot = progress(&[
            ("api", "alpha", api_status),
            (
                "web",
                "alpha",
                json!({"type": "failed", "error": {
                    "type": "machine", "action": "RemoveContainer",
                    "error": {"code": "internal", "message": "disk failure", "details": {}}
                }}),
            ),
            ("web", "beta", waiting(5_000)),
        ]);
        for row in &mut snapshot {
            row.machine_name = Some("same-name".parse().unwrap());
        }
        let mut changes = RowTracker::default().changes(&snapshot);
        for row in &mut changes {
            if let RowState::Failed { log, .. } = &mut row.state {
                *log = Some(LogText::Shown(LogExcerpt::scrub(
                    ["alpha disk log\n".to_owned()],
                    &SealedValues::none(),
                    LineCap::default(),
                )));
            }
        }
        store
            .record(&id(1), &a, RunEvidence::Progress(changes))
            .unwrap();
        let deployment = store
            .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
            .unwrap();
        let serialized = serde_json::to_value(&deployment).unwrap();
        for name in ["web", resource] {
            let node = serialized["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["name"] == name)
                .unwrap();
            let actual: Vec<_> = node["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    json!({
                        "machine_id": row["machine_id"], "server": row["server"],
                        "state": row["state"], "phase": row["phase"],
                        "reason": row["reason"], "cause": row["cause"], "step": row["step"],
                        "log": row["log"]
                    })
                })
                .collect();
            assert_eq!(
                actual,
                [
                    json!({
                        "machine_id": snapshot[1].machine_id, "server": "same-name",
                        "state": "failed", "phase": null,
                        "reason": "remove Container failed", "cause": ["disk failure"],
                        "step": "remove",
                        "log": {"status": "shown", "lines": ["alpha disk log"], "cut": false, "redacted": 0}
                    }),
                    json!({
                        "machine_id": snapshot[2].machine_id, "server": "same-name",
                        "state": "running", "phase": "waiting_for_health",
                        "reason": null, "cause": null, "step": null, "log": null
                    })
                ]
            );
        }
        let view = rows(&store, &who, 1);
        let volume = &view.iter().find(|(name, _)| name == resource).unwrap().1;
        assert_eq!(volume.len(), 2, "Machine names are not identities");
        assert!(volume.iter().all(|row| row.server == "same-name"));
        assert!(volume.iter().any(
            |row| matches!(&row.state, RowState::Failed { cause, .. } if cause == &["disk failure"])
        ));
        assert!(volume.iter().any(|row| row.state
            == RowState::Running {
                phase: RowPhase::WaitingForHealth,
                serving: false,
            }));
        assert!(volume.iter().all(|row| row.started_at.is_some()));
        assert!(
            volume.iter().all(|row| row.history.is_none()),
            "merged resource rows carry no history"
        );
        let web = &view.iter().find(|(name, _)| name == "web").unwrap().1;
        assert!(web.iter().all(|row| row.history.is_some()));
        assert_eq!(
            volume
                .iter()
                .filter(|row| row.finished_at.is_some())
                .count(),
            usize::from(api_finished)
        );
    }
}

#[test]
fn deployment_rows_require_identity_but_older_node_outcomes_need_no_rows() {
    let (store, who) = shop();
    admit(&store, &who, 1, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    let snapshot = progress(&[("web", "alpha", json!({"type": "completed"}))]);
    store
        .record(
            &id(1),
            &a,
            RunEvidence::Progress(RowTracker::default().changes(&snapshot)),
        )
        .unwrap();
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    let mut node = serde_json::to_value(&view.nodes[0]).unwrap();
    let round_trip: ployz_store::NodeOutcome = serde_json::from_value(node.clone()).unwrap();
    assert_eq!(round_trip.rows[0].machine_id, snapshot[0].machine_id);
    node["rows"][0]
        .as_object_mut()
        .unwrap()
        .remove("machine_id");
    assert!(serde_json::from_value::<ployz_store::NodeOutcome>(node.clone()).is_err());
    node["rows"][0]["machine_id"] = json!("same-name");
    assert!(serde_json::from_value::<ployz_store::NodeOutcome>(node.clone()).is_err());
    node.as_object_mut().unwrap().remove("rows");
    let older: ployz_store::NodeOutcome = serde_json::from_value(node).unwrap();
    assert!(older.rows.is_empty());
    assert_eq!(older.node, round_trip.node);
    assert_eq!(older.outcome, round_trip.outcome);
}

#[test]
fn forgetting_the_cluster_leaves_unfinished_rows_unknown() {
    let (store, who) = shop();
    admit(&store, &who, 1, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    store
        .record(
            &id(1),
            &a,
            RunEvidence::Progress(RowTracker::default().changes(&progress(&[
                ("web", "alpha", json!({"type":"completed"})),
                ("web", "beta", waiting(0)),
                ("web", "delta", json!({"type":"pending"})),
            ]))),
        )
        .unwrap();
    store
        .system(
            &who.organization,
            &ployz_store::SystemEvent::ClusterForgotten,
            &Trusted::default(),
        )
        .unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Cancelled);
    let row = &rows(&store, &who, 1)[0].1;
    assert_eq!(
        row.iter().map(|row| &row.state).collect::<Vec<_>>(),
        [&RowState::Completed, &RowState::Unknown, &RowState::Unknown]
    );
    assert!(row[0].finished_at.is_some());
    assert_eq!(row[1].finished_at, None);
    assert_eq!(row[2].started_at, None);
    assert_eq!(
        code(store.record(&id(1), &a, RunEvidence::Alive)),
        RpcErrorCode::Conflict
    );
}

#[test]
fn a_lost_runners_unfinished_rows_read_unknown() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    let mut tracker = RowTracker::default();
    let snapshot = progress(&[
        ("web", "alpha", json!({"type": "completed"})),
        ("web", "beta", waiting(5_000)),
        ("web", "delta", json!({"type": "pending"})),
    ]);
    store
        .record(
            &id(1),
            &a,
            RunEvidence::Progress(tracker.changes(&snapshot)),
        )
        .unwrap();
    let running = |store: &ConfigStore| -> Vec<(String, RowState)> {
        rows(store, &who, 1)
            .into_iter()
            .flat_map(|(_, rows)| rows.into_iter().map(|row| (row.server, row.state)))
            .collect()
    };
    assert_eq!(
        running(&store)[1],
        (
            "beta".to_owned(),
            RowState::Running {
                phase: RowPhase::WaitingForHealth,
                serving: false,
            }
        )
    );
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &runner("runner-b")).unwrap();
    assert_eq!(
        running(&store),
        [
            ("alpha".to_owned(), RowState::Completed),
            ("beta".to_owned(), RowState::Unknown),
            ("delta".to_owned(), RowState::Unknown),
        ]
    );
}

#[test]
fn nothing_executed_applies_nothing() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(
            &id(1),
            &a,
            RunEvidence::NotExecuted("No Server answered".into()),
        )
        .unwrap();
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::NotAttempted),
            ("web".to_owned(), NodeStatus::NotAttempted)
        ]
    );
    assert_eq!(changed(&store, &who), ["api", "web"]);
}

/// #3275: a Deploy saves Working State before it runs. When it runs nothing, the save
/// still holds its changes, and the review says they never ran.
#[test]
fn a_save_whose_deploy_ran_nothing_is_marked_in_the_review() {
    let (store, who) = shop();
    let a = runner("runner-a");
    admit(&store, &who, 1, &[], None).unwrap();
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["api", "web"]))
        .unwrap();
    assert!(!diff(&store, &who).saved_ran_nothing);

    set_replicas(&store, &who, "web", 3);
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &a).unwrap();
    store
        .record(
            &id(2),
            &a,
            RunEvidence::NotExecuted("Storage capacity is unknown".into()),
        )
        .unwrap();
    assert!(diff(&store, &who).saved_ran_nothing);

    let discarded = store
        .write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Head),
                path: None,
                version: None,
            },
        )
        .unwrap();
    assert!(discarded.saved_ran_nothing);
    // Saved #2 still holds the change, so it stays reviewable; the mark says it never ran.
    let view = diff(&store, &who);
    assert!(view.changes.is_empty());
    let setting = &view.draft_changes[0].settings[0];
    assert_eq!(
        (
            setting.path.as_str(),
            setting.before.clone(),
            setting.after.clone()
        ),
        ("web.replicas", json!(3), json!(1))
    );
    assert!(view.saved_ran_nothing);

    // Deploying it again, and it running, clears the mark.
    admit(&store, &who, 3, &[], None).unwrap();
    store.claim(&id(3), &a).unwrap();
    store
        .record(&id(3), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(3), &a, succeeded(&["api", "web"]))
        .unwrap();
    assert!(!diff(&store, &who).saved_ran_nothing);
}

#[test]
fn admission_replays_supersedes_and_checks_the_reviewed_version() {
    let (store, who) = shop();
    let first = admit(&store, &who, 1, &[], None).unwrap();
    assert_eq!(admit(&store, &who, 1, &[], None).unwrap(), first);
    assert_eq!(
        code(admit(&store, &who, 1, &["web"], None)),
        RpcErrorCode::Conflict
    );

    // The newest admission replaces the pending one.
    let reviewed = diff(&store, &who).version;
    set_replicas(&store, &who, "web", 2);
    let stale = admit(&store, &who, 2, &[], Some(reviewed)).unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::Conflict);
    assert!(stale.details.get("diff").is_some());
    let second = admit(&store, &who, 2, &[], Some(diff(&store, &who).version)).unwrap();
    assert_eq!((second.number, second.saved), (Some(2), Revision(2)));
    assert_eq!(
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
            .unwrap()
            .deployment
            .status,
        DeploymentStatus::Superseded
    );
    assert_eq!(
        code(store.claim(&id(1), &runner("runner-a"))),
        RpcErrorCode::Conflict
    );
}

#[test]
fn a_new_runner_leaves_the_replaced_deployment_unknown() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &runner("runner-b")).unwrap();
    let replaced = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    assert_eq!(replaced.deployment.status, DeploymentStatus::Unknown);
    assert!(
        replaced
            .nodes
            .iter()
            .all(|node| node.outcome == NodeStatus::Unknown)
    );
    assert_eq!(
        code(store.record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))),
        RpcErrorCode::Conflict
    );
}

#[test]
fn a_taken_over_run_moves_only_what_it_confirmed_into_applied_state() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(
            &id(1),
            &a,
            RunEvidence::Confirmed(vec![ServiceName::parse("web").unwrap()]),
        )
        .unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    let b = runner("runner-b");
    store.claim(&id(2), &b).unwrap();
    // api finished after the last confirmation, but another runner owns the Environment now.
    let interrupted = RunEvidence::Executed {
        progress: Vec::new(),
        outcome: Box::new(outcome(json!({
            "type": "interrupted",
            "completed": [operation("web"), operation("api")],
            "unexecuted": []
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, interrupted).unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Unknown);
    // With nothing in flight, the diff is against Applied State alone.
    store
        .record(&id(2), &b, RunEvidence::NotExecuted("stopped".into()))
        .unwrap();
    assert_eq!(changed(&store, &who), ["api"]);
}

fn cancel(store: &ConfigStore, who: &Actor, n: u8) -> Result<DeploymentSummary, RpcError> {
    store.write(who, &Cancel { deployment: id(n) })
}

fn status(store: &ConfigStore, who: &Actor, n: u8) -> DeploymentStatus {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id(n) })
        .unwrap()
        .deployment
        .status
}

#[test]
fn a_cancelled_queued_deployment_never_runs() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let other = Actor::system(OrganizationId::parse("other").unwrap());
    assert_eq!(code(cancel(&store, &other, 1)), RpcErrorCode::NotFound);
    assert_eq!(
        cancel(&store, &who, 1).unwrap().status,
        DeploymentStatus::Cancelled
    );
    // Cancelling again changes nothing; its runner finds nothing to run.
    assert_eq!(
        cancel(&store, &who, 1).unwrap().status,
        DeploymentStatus::Cancelled
    );
    assert_eq!(
        code(store.claim(&id(1), &runner("runner-a"))),
        RpcErrorCode::Conflict
    );
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::NotAttempted),
            ("web".to_owned(), NodeStatus::NotAttempted)
        ]
    );
    assert_eq!(changed(&store, &who), ["api", "web"]);
}

#[test]
fn a_cancelled_running_deployment_keeps_its_confirmed_node_outcomes() {
    let (store, who) = shop();
    let a = runner("runner-a");
    let stopped_by = |n: u8, error: serde_json::Value| {
        admit(&store, &who, n, &[], None).unwrap();
        store.claim(&id(n), &a).unwrap();
        store
            .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
            .unwrap();
        assert_eq!(
            cancel(&store, &who, n).unwrap().status,
            DeploymentStatus::Cancelling
        );
        // Its runner stops it partway and records what ran.
        let stopped = RunEvidence::Executed {
            progress: Vec::new(),
            outcome: Box::new(outcome(json!({
                "type": "failed", "completed": [operation("web")],
                "failed": {"type": "operation", "operation": operation("api"), "error": error},
                "unexecuted": []
            }))),
            removed: Vec::new(),
        };
        store.record(&id(n), &a, stopped).unwrap();
        assert_eq!(status(&store, &who, n), DeploymentStatus::Cancelled);
        assert_eq!(
            nodes(&store, &who, n),
            [
                ("api".to_owned(), NodeStatus::Failed),
                ("web".to_owned(), NodeStatus::Deployed)
            ]
        );
        (
            cancelled_nodes(&store, &who, n),
            outcome_cancelled(&store, &who, n),
        )
    };
    assert_eq!(
        stopped_by(1, json!({"type": "cancelled"})),
        (vec!["api".to_owned()], true),
        "the reason only repeats the cancel"
    );
    assert_eq!(changed(&store, &who), ["api"]);
    assert_eq!(
        stopped_by(
            2,
            json!({
                "type": "machine", "action": "CreateContainer",
                "error": {"code": "internal", "message": "no space left on device", "details": {}}
            })
        ),
        (Vec::<String>::new(), false),
        "work that failed on its own stays failed, even once cancelled"
    );
}

fn create_error() -> Value {
    json!({
        "type": "machine", "action": "CreateContainer",
        "error": {"code": "internal", "message": "no space left on device", "details": {}}
    })
}

#[test]
fn a_copy_restored_after_another_service_failed_is_not_cancelled() {
    let (store, who) = shop();
    let a = runner("runner-a");
    let restored_after = |n: u8, failed: &str| {
        admit(&store, &who, n, &[], None).unwrap();
        store.claim(&id(n), &a).unwrap();
        store
            .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
            .unwrap();
        let error = |service: &str| {
            if service == "api" {
                create_error()
            } else {
                json!({"type": "cancelled"})
            }
        };
        let unexecuted = if failed == "api" { "web" } else { "api" };
        let executed = RunEvidence::Executed {
            progress: RowTracker::default().changes(&progress(&[
                (
                    "web",
                    "alpha",
                    json!({"type": "failed", "error": error("web")}),
                ),
                (
                    "api",
                    "alpha",
                    json!({"type": "failed", "error": error("api")}),
                ),
            ])),
            outcome: Box::new(outcome(json!({
                "type": "failed", "completed": [],
                "failed": {"type": "operation", "operation": operation(failed), "error": error(failed)},
                "unexecuted": [operation(unexecuted)]
            }))),
            removed: Vec::new(),
        };
        store.record(&id(n), &a, executed).unwrap();
        assert_eq!(status(&store, &who, n), DeploymentStatus::Failed);
        let web = rows(&store, &who, n)
            .into_iter()
            .find(|(node, _)| node == "web")
            .unwrap();
        assert!(
            matches!(
                web.1[0].state,
                RowState::Failed {
                    stopped_by: StoppedBy::NOTHING,
                    ..
                }
            ),
            "nobody cancelled it: {:?}",
            web.1[0].state
        );
        cancelled_nodes(&store, &who, n)
    };
    for (n, failed) in [(1, "api"), (2, "web")] {
        assert!(restored_after(n, failed).is_empty(), "{failed} first");
    }
}

fn web_row(store: &ConfigStore, who: &Actor) -> ServerRow {
    rows(store, who, 1)
        .into_iter()
        .find(|(node, _)| node == "web")
        .unwrap()
        .1
        .remove(0)
}

#[test]
fn a_real_failure_after_a_cancel_on_the_same_service_reads_failed_on_its_row_and_node() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "web"])))
        .unwrap();
    cancel(&store, &who, 1).unwrap();
    let cancelled = json!({"type": "failed", "error": {"type": "cancelled"}});
    let mut tracker = RowTracker::default();
    let first = tracker.changes(&progress(&[
        ("web", "alpha", cancelled.clone()),
        ("web", "alpha", waiting(5_000)),
    ]));
    store
        .record(&id(1), &a, RunEvidence::Progress(first))
        .unwrap();
    let state = &web_row(&store, &who).state;
    assert!(
        matches!(
            state,
            RowState::Failed {
                stopped_by: StoppedBy::CANCEL,
                ..
            }
        ),
        "{state:?}"
    );
    let executed = RunEvidence::Executed {
        progress: tracker.changes(&progress(&[
            ("web", "alpha", cancelled),
            (
                "web",
                "alpha",
                json!({"type": "failed", "error": create_error()}),
            ),
        ])),
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [],
            "failed": {"type": "operation", "operation": operation("web"), "error": {"type": "cancelled"}},
            "unexecuted": [operation("web")]
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, executed).unwrap();
    let state = &web_row(&store, &who).state;
    assert!(
        matches!(
            state,
            RowState::Failed {
                stopped_by: StoppedBy::NOTHING,
                ..
            }
        ),
        "{state:?}"
    );
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::Unchanged),
            ("web".to_owned(), NodeStatus::Failed)
        ]
    );
    assert!(cancelled_nodes(&store, &who, 1).is_empty());
}

#[test]
fn a_service_the_cancel_stopped_reads_cancelled_at_its_node_beside_a_real_failure() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(
            &id(1),
            &a,
            RunEvidence::Prepared(preview(&["web", "web", "api"])),
        )
        .unwrap();
    cancel(&store, &who, 1).unwrap();
    let executed = RunEvidence::Executed {
        progress: RowTracker::default().changes(&progress(&[
            ("web", "alpha", json!({"type": "completed"})),
            (
                "web",
                "alpha",
                json!({"type": "failed", "error": {"type": "cancelled"}}),
            ),
            (
                "api",
                "alpha",
                json!({"type": "failed", "error": create_error()}),
            ),
        ])),
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [operation("web")],
            "failed": {"type": "operation", "operation": operation("api"), "error": create_error()},
            "unexecuted": [operation("web")]
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, executed).unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Cancelled);
    let state = &web_row(&store, &who).state;
    assert!(
        matches!(
            state,
            RowState::Failed {
                stopped_by: StoppedBy::CANCEL,
                ..
            }
        ),
        "{state:?}"
    );
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::Failed),
            ("web".to_owned(), NodeStatus::Failed)
        ]
    );
    assert_eq!(
        cancelled_nodes(&store, &who, 1),
        ["web"],
        "the cancel, not api's failure, stopped web, so its node reads as its row does"
    );
    assert!(
        !outcome_cancelled(&store, &who, 1),
        "the reason names api's real failure"
    );
}

/// A cancel stops web while api's create, running beside it, fails on its own. The
/// runner names only the first failure it saw, web's cancel, and reports api's
/// operation as unexecuted; api's row holds its real failure.
#[test]
fn a_real_failure_beside_the_cancelled_work_the_outcome_names_wins_the_reason_and_its_node() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    cancel(&store, &who, 1).unwrap();
    let executed = RunEvidence::Executed {
        progress: RowTracker::default().changes(&progress(&[
            (
                "web",
                "alpha",
                json!({"type": "failed", "error": {"type": "cancelled"}}),
            ),
            (
                "api",
                "alpha",
                json!({"type": "failed", "error": create_error()}),
            ),
        ])),
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [],
            "failed": {"type": "operation", "operation": operation("web"), "error": {"type": "cancelled"}},
            "unexecuted": [operation("api")]
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, executed).unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Cancelled);
    assert!(
        matches!(
            web_row(&store, &who).state,
            RowState::Failed {
                stopped_by: StoppedBy::CANCEL,
                ..
            }
        ),
        "the cancel stopped web's row"
    );
    assert!(
        !outcome_cancelled(&store, &who, 1),
        "api failed on its own, so the reason is no repeat of the cancel"
    );
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::Failed),
            ("web".to_owned(), NodeStatus::Failed)
        ]
    );
    assert_eq!(cancelled_nodes(&store, &who, 1), ["web"]);

    // The dashboard reads this snapshot (store-deployments.test.ts).
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    let snapshot = json!({
        "outcome": view.deployment.outcome,
        "nodes": view.nodes.iter().map(|node| json!({
            "name": node.node.name(), "outcome": node.outcome, "cancelled": node.cancelled
        })).collect::<Vec<_>>(),
    });
    assert_eq!(
        snapshot["outcome"]["reason"], "api: create Container failed",
        "{snapshot:#}"
    );
    let mut text = serde_json::to_string_pretty(&snapshot).unwrap();
    text.push('\n');
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots/cancel_beside_a_real_failure.json");
    if std::env::var_os("PLOYZ_BLESS").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap_or_default(),
        text,
        "rerun with PLOYZ_BLESS=1 and review {}",
        path.display()
    );
}

/// A cancel stops web, then api fails on its own, then web's next operation fails on
/// its own on the same row. The Store keeps web's row finished from the cancel, but
/// api failed on its own first, so api names the reason.
#[test]
fn the_first_real_failure_names_the_reason_though_a_cancel_finished_another_row_first() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(
            &id(1),
            &a,
            RunEvidence::Prepared(preview(&["web", "api", "web"])),
        )
        .unwrap();
    cancel(&store, &who, 1).unwrap();
    let cancelled = json!({"type": "failed", "error": {"type": "cancelled"}});
    let failed = json!({"type": "failed", "error": create_error()});
    let mut tracker = RowTracker::default();
    let mut report = |rows: &[(&str, &str, Value)]| tracker.changes(&progress(rows));
    // real-wait: the Store stamps rows with the wall clock in whole seconds.
    let next_second = || std::thread::sleep(std::time::Duration::from_secs(1));
    let web_stopped = report(&[
        ("web", "alpha", cancelled.clone()),
        ("api", "alpha", waiting(5_000)),
    ]);
    store
        .record(&id(1), &a, RunEvidence::Progress(web_stopped))
        .unwrap();
    next_second();
    let api_failed = report(&[
        ("web", "alpha", cancelled.clone()),
        ("api", "alpha", failed.clone()),
    ]);
    store
        .record(&id(1), &a, RunEvidence::Progress(api_failed))
        .unwrap();
    next_second();
    let executed = RunEvidence::Executed {
        progress: report(&[
            ("web", "alpha", cancelled),
            ("api", "alpha", failed.clone()),
            ("web", "alpha", failed),
        ]),
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [],
            "failed": {"type": "operation", "operation": operation("web"), "error": {"type": "cancelled"}},
            "unexecuted": [operation("api"), operation("web")]
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, executed).unwrap();
    let web = web_row(&store, &who);
    assert!(
        matches!(
            web.state,
            RowState::Failed {
                stopped_by: StoppedBy::NOTHING,
                ..
            }
        ),
        "{:?}",
        web.state
    );
    let api = &rows(&store, &who, 1)
        .into_iter()
        .find(|(node, _)| node == "api")
        .unwrap()
        .1[0];
    assert!(
        web.finished_at < api.finished_at,
        "the cancel finished web's row before api failed: {web:?} {api:?}"
    );
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    let Some(ployz_store::Outcome::Executed {
        reason, cancelled, ..
    }) = view.deployment.outcome
    else {
        panic!("expected an executed outcome");
    };
    assert_eq!(
        reason.as_deref(),
        Some("api: create Container failed"),
        "api failed on its own before web did"
    );
    assert!(!cancelled);
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::Failed),
            ("web".to_owned(), NodeStatus::Failed)
        ]
    );
    assert!(
        cancelled_nodes(&store, &who, 1).is_empty(),
        "web failed on its own, so its node reads Failed, not Cancelled"
    );
}

/// The row state and Node Outcome a Store from before Cancelled decodes, as it
/// shipped them.
mod before_cancelled {
    use std::collections::BTreeMap;

    use ployz_store::{FailedAt, RowPhase};
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    pub enum RowState {
        Pending,
        Running {
            #[expect(dead_code, reason = "decoded as it shipped")]
            phase: RowPhase,
            #[serde(default)]
            #[expect(dead_code, reason = "decoded as it shipped")]
            serving: bool,
        },
        Completed,
        Failed {
            reason: String,
            #[expect(dead_code, reason = "decoded as it shipped")]
            cause: Vec<String>,
            #[serde(default)]
            #[expect(dead_code, reason = "decoded as it shipped")]
            step: Option<FailedAt>,
            #[serde(default)]
            #[expect(dead_code, reason = "decoded as it shipped")]
            log: Option<Value>,
            #[serde(default)]
            #[expect(dead_code, reason = "decoded as it shipped")]
            unanswered: bool,
            #[serde(default)]
            #[expect(dead_code, reason = "decoded as it shipped")]
            out_of_memory: Option<Value>,
        },
        NotAttempted,
        Unknown,
    }

    #[derive(Debug, Deserialize)]
    pub struct RowTransition {
        #[expect(dead_code, reason = "decoded as it shipped")]
        pub at: i64,
        #[serde(flatten)]
        pub state: RowState,
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
    #[serde(rename_all = "snake_case")]
    pub enum NodeStatus {
        Pending,
        Deployed,
        Removed,
        Failed,
        NotAttempted,
        Unchanged,
        Unknown,
    }

    /// The run document, as far as its Node Outcomes.
    #[derive(Debug, Deserialize)]
    pub struct Run {
        #[serde(default)]
        pub nodes: BTreeMap<String, NodeStatus>,
    }
}

#[test]
fn a_store_from_before_cancelled_reads_the_work_a_cancel_stopped_as_failed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let (store, who) =
        shop_in(ConfigStore::open(&format!("sqlite:{}", path.display()), backend::key()).unwrap());
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    cancel(&store, &who, 1).unwrap();
    let cancelled = json!({"type": "failed", "error": {"type": "cancelled"}});
    let mut tracker = RowTracker::default();
    let stopped = tracker.changes(&progress(&[
        ("web", "alpha", json!({"type": "completed"})),
        ("api", "alpha", cancelled),
    ]));
    let executed = RunEvidence::Executed {
        progress: stopped,
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [operation("web")],
            "failed": {"type": "operation", "operation": operation("api"), "error": {"type": "cancelled"}},
            "unexecuted": []
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, executed).unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Cancelled);
    assert_eq!(cancelled_nodes(&store, &who, 1), ["api"]);
    let api = rows(&store, &who, 1)
        .into_iter()
        .find(|(node, _)| node == "api")
        .unwrap();
    assert!(
        matches!(
            api.1[0].state,
            RowState::Failed {
                stopped_by: StoppedBy::CANCEL,
                ..
            }
        ),
        "{:?}",
        api.1[0].state
    );

    // A Store rolled back past Cancelled still reads this Deployment, as Failed.
    let sql = rusqlite::Connection::open(&path).unwrap();
    let run: String = sql
        .query_row("SELECT run FROM config_deployment", [], |row| row.get(0))
        .unwrap();
    let run: before_cancelled::Run = serde_json::from_str(&run).unwrap();
    let mut outcomes: Vec<_> = run.nodes.into_values().collect();
    outcomes.sort();
    assert_eq!(
        outcomes,
        [
            before_cancelled::NodeStatus::Deployed,
            before_cancelled::NodeStatus::Failed
        ]
    );
    let (state, history): (String, String) = sql
        .query_row(
            "SELECT state, history FROM config_deployment_row WHERE service = 'api'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let state: before_cancelled::RowState = serde_json::from_str(&state).unwrap();
    assert!(
        matches!(&state, before_cancelled::RowState::Failed { reason, .. } if reason == "Cancelled"),
        "{state:?}"
    );
    let history: Vec<before_cancelled::RowTransition> = serde_json::from_str(&history).unwrap();
    assert!(
        matches!(
            history.last().map(|entry| &entry.state),
            Some(before_cancelled::RowState::Failed { .. })
        ),
        "{history:?}"
    );
}

#[test]
fn a_runner_that_loses_track_after_preparing_leaves_the_outcome_unknown() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    // Its step is retried: it may have executed, so nothing replays.
    let retried = store.claim(&id(1), &a).unwrap_err();
    assert_eq!(retried.code, RpcErrorCode::Conflict);
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Unknown);
    assert!(
        nodes(&store, &who, 1)
            .iter()
            .all(|(_, outcome)| *outcome == NodeStatus::Unknown)
    );
    assert_eq!(
        code(store.record(&id(1), &a, succeeded(&["web", "api"]))),
        RpcErrorCode::Conflict
    );
    assert_eq!(changed(&store, &who), ["api", "web"]);

    // Abandoned after preparing: unknown too.
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store.record(&id(2), &a, RunEvidence::Abandoned).unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Unknown);
    store.record(&id(2), &a, RunEvidence::Abandoned).unwrap();
}

#[test]
fn a_runner_that_stops_before_preparing_executed_nothing() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    // Only its owner may abandon it, once it claimed it.
    assert_eq!(
        code(store.record(&id(1), &a, RunEvidence::Abandoned)),
        RpcErrorCode::Conflict
    );
    store.claim(&id(1), &a).unwrap();
    store.record(&id(1), &a, RunEvidence::Abandoned).unwrap();
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Failed);
    assert!(matches!(
        view.deployment.outcome,
        Some(ployz_store::Outcome::NotExecuted { .. })
    ));

    // Abandoning after an outcome changes nothing.
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(2), &a, succeeded(&["web", "api"]))
        .unwrap();
    store.record(&id(2), &a, RunEvidence::Abandoned).unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Applied);
    // An ended Deployment can't be cancelled.
    assert_eq!(code(cancel(&store, &who, 2)), RpcErrorCode::Conflict);
}

fn retry(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    source: u8,
) -> Result<DeploymentSummary, RpcError> {
    store.write_trusted(
        who,
        &Admit::Retry(Retry {
            id: id(n),
            deployment: id(source),
            rolls_back: None,
            version: None,
            accept_volume_loss: Vec::new(),
        }),
        &ployz_store::Trusted::default(),
    )
}

fn start(store: &ConfigStore, who: &Actor, n: u8) -> Result<DeploymentSummary, RpcError> {
    store.write(who, &Start { deployment: id(n) })
}

/// Deployment `n` claimed by `runner-a`, which fails `api` after applying `web`.
fn fail_api(store: &ConfigStore, n: u8) -> ployz_core::DeployIntent {
    let a = runner("runner-a");
    let intent = store.claim(&id(n), &a).unwrap().intent;
    store
        .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let failed = RunEvidence::Executed {
        progress: Vec::new(),
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [operation("web")],
            "failed": {"type": "operation", "operation": operation("api"), "error": {"type": "cancelled"}},
            "unexecuted": []
        }))),
        removed: Vec::new(),
    };
    store.record(&id(n), &a, failed).unwrap();
    intent
}

#[test]
fn a_retry_ships_exactly_what_the_failed_deployment_froze() {
    let (store, who) = shop();
    let first = admit(&store, &who, 1, &[], None).unwrap();
    let frozen = fail_api(&store, 1);
    // A newer Saved revision, admitted and cancelled before it ran.
    set_replicas(&store, &who, "web", 3);
    admit(&store, &who, 2, &[], None).unwrap();
    cancel(&store, &who, 2).unwrap();

    let retried = retry(&store, &who, 3, 1).unwrap();
    assert_eq!(
        (
            retried.number,
            retried.status,
            retried.saved,
            retried.services
        ),
        (
            Some(3),
            DeploymentStatus::Queued,
            first.saved,
            first.services
        )
    );
    // Replaying it is idempotent; its ID with another source is not.
    assert_eq!(retry(&store, &who, 3, 1).unwrap().number, Some(3));
    assert_eq!(code(retry(&store, &who, 3, 2)), RpcErrorCode::Conflict);
    // The failed one keeps its Node Outcomes; the retry targets every node again.
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::Failed),
            ("web".to_owned(), NodeStatus::Deployed)
        ]
    );
    assert!(
        nodes(&store, &who, 3)
            .iter()
            .all(|(_, outcome)| *outcome == NodeStatus::Pending)
    );
    let claimed = store.claim(&id(3), &runner("runner-b")).unwrap();
    assert_eq!(claimed.intent, frozen);
    assert_eq!(
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(3) })
            .unwrap()
            .namespace
            .as_str(),
        "shop-production"
    );
    // A cancelled Deployment can be retried too, with its own Saved revision, once
    // the newer #3 ends.
    let b = runner("runner-b");
    store
        .record(&id(3), &b, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(3), &b, succeeded(&["web", "api"]))
        .unwrap();
    let newer = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap()
        .deployment
        .saved;
    assert_ne!(newer, first.saved);
    assert_eq!(retry(&store, &who, 4, 2).unwrap().saved, newer);
}

/// Deployment `n` claimed by `runner-a` and applied; the Deploy Intent it ran.
fn apply(store: &ConfigStore, n: u8) -> ployz_core::DeployIntent {
    let a = runner("runner-a");
    let intent = store.claim(&id(n), &a).unwrap().intent;
    store
        .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(n), &a, succeeded(&["web", "api"]))
        .unwrap();
    intent
}

fn rolls_back(store: &ConfigStore, who: &Actor, n: u8) -> Vec<u64> {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id(n) })
        .unwrap()
        .rolls_back
}

#[test]
fn redeploying_an_older_deployment_rolls_back_to_what_it_froze() {
    let (store, who) = shop();
    let first = admit(&store, &who, 1, &[], None).unwrap();
    let frozen = apply(&store, 1);
    set_replicas(&store, &who, "web", 3);
    let second = admit(&store, &who, 2, &[], None).unwrap();
    apply(&store, 2);
    assert_ne!(second.saved, first.saved);
    assert_eq!(rolls_back(&store, &who, 1), [2]);
    assert!(rolls_back(&store, &who, 2).is_empty());

    // #1 applied and #2 applied after it: redeploying #1 ships #1's revision again.
    let rolled_back = retry(&store, &who, 3, 1).unwrap();
    assert_eq!(
        (rolled_back.number, rolled_back.status, rolled_back.saved),
        (Some(3), DeploymentStatus::Queued, first.saved)
    );
    assert_eq!(apply(&store, 3), frozen);
    assert_eq!(rolls_back(&store, &who, 1), [2, 3]);
    // What #2 saved is still the latest Saved revision, so it shows as a change to
    // deploy, and the next Deploy moves forward to it.
    assert_eq!(changed(&store, &who), ["web"]);
    assert_eq!(
        admit(&store, &who, 4, &[], None).unwrap().saved,
        second.saved
    );

    // The newest applied Deployment redeploys too, rolling nothing back.
    apply(&store, 4);
    assert!(rolls_back(&store, &who, 4).is_empty());
    assert_eq!(retry(&store, &who, 5, 4).unwrap().saved, second.saved);
}

#[test]
fn a_redeploy_is_refused_unless_its_deployment_ended() {
    let (store, who) = shop();
    let other = Actor::system(OrganizationId::parse("other").unwrap());
    admit(&store, &who, 1, &[], None).unwrap();
    assert_eq!(code(retry(&store, &other, 9, 1)), RpcErrorCode::NotFound);
    assert_eq!(code(retry(&store, &who, 9, 8)), RpcErrorCode::NotFound);
    // Queued, running or cancelling: not ended yet.
    assert_eq!(code(retry(&store, &who, 9, 1)), RpcErrorCode::Conflict);
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    assert_eq!(code(retry(&store, &who, 9, 1)), RpcErrorCode::Conflict);
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    cancel(&store, &who, 1).unwrap();
    assert_eq!(code(retry(&store, &who, 9, 1)), RpcErrorCode::Conflict);
    // Its runner finished before it saw the cancel: it applied, and redeploys.
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Applied);
    // A superseded one never ran: deploy again instead.
    admit(&store, &who, 2, &[], None).unwrap();
    admit(&store, &who, 3, &[], None).unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Superseded);
    assert_eq!(code(retry(&store, &who, 9, 2)), RpcErrorCode::Conflict);
    // A refused redeploy queued nothing, and an accepted one supersedes what's queued.
    fail_api(&store, 3);
    assert_eq!(retry(&store, &who, 9, 3).unwrap().number, Some(4));
    assert_eq!(status(&store, &who, 3), DeploymentStatus::Failed);
    admit(&store, &who, 5, &[], None).unwrap();
    retry(&store, &who, 6, 3).unwrap();
    assert_eq!(status(&store, &who, 5), DeploymentStatus::Superseded);
    assert_eq!(status(&store, &who, 9), DeploymentStatus::Superseded);
    assert_eq!(retry(&store, &who, 7, 1).unwrap().number, Some(7));
    assert_eq!(status(&store, &who, 6), DeploymentStatus::Superseded);
}

/// Redeploy `source` as `n`, confirming that it rolls back `confirmed`.
fn rollback(
    store: &ConfigStore,
    who: &Actor,
    (n, source): (u8, u8),
    confirmed: &[u64],
) -> Result<DeploymentSummary, RpcError> {
    store.write_trusted(
        who,
        &Admit::Retry(Retry {
            id: id(n),
            deployment: id(source),
            rolls_back: Some(confirmed.to_vec()),
            version: None,
            accept_volume_loss: Vec::new(),
        }),
        &ployz_store::Trusted::default(),
    )
}

#[test]
fn a_redeploy_rolls_back_every_newer_deployment_that_may_have_changed_the_servers() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    apply(&store, 1);
    // #2 lost track after preparing: it may have changed the Servers.
    set_replicas(&store, &who, "web", 3);
    admit(&store, &who, 2, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store.record(&id(2), &a, RunEvidence::Abandoned).unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Unknown);
    // #3 was cancelled before it started, #4 superseded and #5 still queued: none ran.
    admit(&store, &who, 3, &[], None).unwrap();
    cancel(&store, &who, 3).unwrap();
    admit(&store, &who, 4, &[], None).unwrap();
    admit(&store, &who, 5, &[], None).unwrap();
    assert_eq!(rolls_back(&store, &who, 1), [2]);

    // What the caller confirmed must still be what it rolls back.
    let stale = rollback(&store, &who, (6, 1), &[]).unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(stale.details["rolls_back"], json!([2]));
    assert_eq!(status(&store, &who, 5), DeploymentStatus::Queued);
    assert_eq!(
        rollback(&store, &who, (6, 1), &[2]).unwrap().number,
        Some(6)
    );
}

#[test]
fn redeploying_an_older_deployment_waits_for_a_newer_one_in_flight() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    apply(&store, 1);
    set_replicas(&store, &who, "web", 3);
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &runner("runner-a")).unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Running);

    // Applied State may yet change under #2, so nothing older redeploys meanwhile.
    let waiting = retry(&store, &who, 3, 1).unwrap_err();
    assert_eq!(waiting.code, RpcErrorCode::Conflict);
    assert_eq!(waiting.message, "Wait for #2 to finish, then redeploy.");
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Running);
}

#[test]
fn a_newer_deployment_whose_runner_is_gone_does_not_hold_up_a_redeploy() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    admit(&store, &who, 1, &[], None).unwrap();
    apply(&store, 1);
    set_replicas(&store, &who, "web", 3);
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &runner("runner-a")).unwrap();
    backend::execute(&url, "UPDATE config_deployment SET lease = 0");
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Unknown);

    assert_eq!(retry(&store, &who, 3, 1).unwrap().number, Some(3));
}

#[test]
fn a_full_rollback_retires_the_services_added_since() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &RemoveService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("api").unwrap(),
            },
        )
        .unwrap();
    admit(&store, &who, 1, &[], None).unwrap();
    apply(&store, 1);
    // #2 adds `cron`.
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("cron").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    apply(&store, 2);
    assert!(changed(&store, &who).is_empty());

    // Rolling back to #1 takes `cron` off, and its saved settings show as staged.
    retry(&store, &who, 3, 1).unwrap();
    assert!(
        nodes(&store, &who, 3)
            .iter()
            .any(|(name, _)| name == "cron")
    );
    apply(&store, 3);
    assert_eq!(changed(&store, &who), ["cron"]);
}

#[test]
fn only_a_queued_deployment_starts() {
    let (store, who) = shop();
    let other = Actor::system(OrganizationId::parse("other").unwrap());
    admit(&store, &who, 1, &[], None).unwrap();
    assert_eq!(code(start(&store, &other, 1)), RpcErrorCode::NotFound);
    // Starting changes nothing: a runner still claims it.
    assert_eq!(
        start(&store, &who, 1).unwrap().status,
        DeploymentStatus::Queued
    );
    assert_eq!(
        start(&store, &who, 1).unwrap().status,
        DeploymentStatus::Queued
    );
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    assert_eq!(code(start(&store, &who, 1)), RpcErrorCode::Conflict);
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(code(start(&store, &who, 1)), RpcErrorCode::Conflict);
    admit(&store, &who, 2, &[], None).unwrap();
    admit(&store, &who, 3, &[], None).unwrap();
    assert_eq!(code(start(&store, &who, 2)), RpcErrorCode::Conflict);
    cancel(&store, &who, 3).unwrap();
    assert_eq!(code(start(&store, &who, 3)), RpcErrorCode::Conflict);
}

#[test]
fn a_targeted_deploy_and_its_plan_cover_only_the_named_services() {
    let (store, who) = shop();
    let plan = store
        .read(
            &who,
            &PlanQuery {
                services: vec![ServiceName::parse("web").unwrap()],
                ..PlanQuery::default()
            },
        )
        .unwrap();
    assert_eq!(plan.version, diff(&store, &who).version);
    assert_eq!(
        plan.changes
            .iter()
            .map(|change| change.name.as_str())
            .collect::<Vec<_>>(),
        ["web"]
    );
    assert_eq!(plan.unresolved, ["operations"]);
    assert_eq!(plan.namespace.as_str(), "shop-production");
    assert_eq!(
        code(admit(&store, &who, 1, &["nope"], None)),
        RpcErrorCode::NotFound
    );

    admit(&store, &who, 1, &["web"], None).unwrap();
    let claimed = store.claim(&id(1), &runner("runner-a")).unwrap();
    assert_eq!(
        claimed
            .intent
            .options
            .selected
            .iter()
            .map(|attempt| attempt.name.as_str())
            .collect::<Vec<_>>(),
        ["web"]
    );
    assert_eq!(changed(&store, &who), ["api"]);
}

#[test]
fn deployments_page_newest_first_within_the_organization() {
    let (store, who) = shop();
    for n in 1..=3 {
        admit(&store, &who, n, &[], None).unwrap();
    }
    let page = |cursor: Option<String>| {
        store
            .read(
                &who,
                &DeploymentsQuery {
                    limit: Some(2),
                    cursor,
                    ..DeploymentsQuery::default()
                },
            )
            .unwrap()
    };
    let first = page(None);
    assert_eq!(
        first
            .deployments
            .iter()
            .map(|d| d.number)
            .collect::<Vec<_>>(),
        [Some(3), Some(2)]
    );
    let last = page(first.next_cursor.clone());
    assert_eq!(
        last.deployments
            .iter()
            .map(|d| d.number)
            .collect::<Vec<_>>(),
        [Some(1)]
    );
    assert_eq!(last.next_cursor, None);
    let bad_limit = store.read(
        &who,
        &DeploymentsQuery {
            limit: Some(0),
            ..DeploymentsQuery::default()
        },
    );
    assert_eq!(code(bad_limit), RpcErrorCode::InvalidArgument);
    let stranger = Actor::system(OrganizationId::parse("other").unwrap());
    assert_eq!(
        code(store.read(&stranger, &ployz_store::DeploymentQuery { id: id(1) })),
        RpcErrorCode::NotFound
    );
}

#[test]
fn a_staged_removal_leaves_applied_state_until_its_deploy_confirms_it() {
    let (store, who) = shop();
    let a = runner("runner-a");
    admit(&store, &who, 1, &[], None).unwrap();
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();

    let remove = RemoveService {
        environment: EnvironmentRef::default(),
        service: ServiceName::parse("web").unwrap(),
    };
    store.write(&who, &remove).unwrap();
    // Nothing ran: `web` is still applied, and lists as removed by the next Deploy.
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::Deployed),
            ("web".to_owned(), NodeStatus::Deployed)
        ]
    );
    let listed = store.read(&who, &ServicesQuery::default()).unwrap();
    assert_eq!(
        listed
            .services
            .iter()
            .map(|listing| (listing.service.name.as_str(), listing.change))
            .collect::<Vec<_>>(),
        [("api", None), ("web", Some(ReviewLifecycleKind::Delete))]
    );
    assert_eq!(changed(&store, &who), ["web"]);
    let removed = store
        .read(
            &who,
            &ServiceQuery {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(removed.values["image"], json!("nginx:1"));

    // The Deploy removes it; once confirmed, it is gone.
    admit(&store, &who, 2, &[], None).unwrap();
    let claimed = store.claim(&id(2), &a).unwrap();
    assert_eq!(claimed.intent.target.len(), 1);
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    store.record(&id(2), &a, succeeded(&["web"])).unwrap();
    let listed = store.read(&who, &ServicesQuery::default()).unwrap();
    assert_eq!(listed.services.len(), 1);
    assert!(changed(&store, &who).is_empty());
}

#[test]
fn renaming_a_deployed_service_is_a_staged_change_discard_undoes() {
    let (store, who) = shop();
    let a = runner("runner-a");
    admit(&store, &who, 1, &[], None).unwrap();
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    store
        .write(
            &who,
            &Command::RenameService(RenameService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                name: ServiceName::parse("front").unwrap(),
            }),
        )
        .unwrap();
    let change = diff(&store, &who).changes.remove(0);
    assert_eq!(
        (change.name.as_str(), change.settings[0].path.as_str()),
        ("front", "front.name")
    );
    assert_eq!(
        (&change.settings[0].before, &change.settings[0].after),
        (&json!("web"), &json!("front"))
    );

    store
        .write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse("front").unwrap()),
                version: None,
            },
        )
        .unwrap();
    assert!(changed(&store, &who).is_empty());
}

#[test]
fn a_successful_deploy_applies_a_rename_the_runtime_had_nothing_to_do_for() {
    let (store, who) = shop();
    let a = runner("runner-a");
    admit(&store, &who, 1, &[], None).unwrap();
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    store
        .write(
            &who,
            &Command::RenameService(RenameService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                name: ServiceName::parse("front").unwrap(),
            }),
        )
        .unwrap();
    assert_eq!(changed(&store, &who), ["front"]);
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&[])))
        .unwrap();
    // Nothing ran, yet the Deploy covered the rename: Applied State holds it.
    store.record(&id(2), &a, succeeded(&[])).unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Applied);
    assert!(changed(&store, &who).is_empty());
}

#[test]
fn an_environments_namespace_is_the_one_its_deployments_use() {
    let (store, who) = shop();
    let query: Query = serde_json::from_value(json!({"query": "namespace"})).unwrap();
    let View::Namespace(before) = store.read(&who, &query).unwrap() else {
        panic!("a namespace query answers a namespace view");
    };
    assert_eq!(before.namespace.as_str(), "shop-production");
    admit(&store, &who, 1, &[], None).unwrap();
    let after = store.read(&who, &NamespaceQuery::default()).unwrap();
    assert_eq!(after, before);
    let stranger = Actor::system(OrganizationId::parse("other").unwrap());
    assert_eq!(
        code(store.read(&stranger, &NamespaceQuery::default())),
        RpcErrorCode::NotFound
    );
}

#[test]
fn each_service_builds_from_its_own_upload_and_cloud_keeps_it() {
    let (store, who) = shop();
    for (n, name) in [(6, "front"), (7, "back")] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: None,
                    template: None,
                },
            )
            .unwrap();
    }
    let upload = |c: char| UploadedSource {
        digest: ployz_core::UploadDigest::parse(c.to_string().repeat(64)).unwrap(),
        base: None,
        uploader: None,
    };
    let deploy = |n: u8, services: &[&str], upload: Option<UploadedSource>| {
        store.write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id(n),
                environment: EnvironmentRef::default(),
                services: services
                    .iter()
                    .map(|name| ServiceName::parse(*name).unwrap())
                    .collect(),
                version: None,
                upload,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &Trusted::default(),
        )
    };
    let a = runner("cli-a");
    let end = |n: u8| {
        store.claim(&id(n), &a).unwrap();
        store
            .record(&id(n), &a, RunEvidence::NotExecuted("stopped".into()))
            .unwrap();
    };
    let kept = |deployments: &[u8]| -> Vec<bool> {
        store
            .read(
                &who,
                &ployz_store::UploadsQuery {
                    deployments: deployments.iter().map(|n| id(*n)).collect(),
                },
            )
            .unwrap()
            .uploads
            .iter()
            .map(|held| held.kept)
            .collect()
    };
    deploy(1, &[], Some(upload('a'))).unwrap();
    end(1);
    // An upload for back alone leaves front's.
    deploy(2, &["back"], Some(upload('b'))).unwrap();
    end(2);
    assert_eq!(kept(&[1, 2]), [true, true]);
    // Without a new upload, front builds from its own.
    assert_eq!(
        deploy(3, &["front"], None).unwrap().upload,
        Some(upload('a'))
    );
    end(3);
    // A full Deploy builds each from its own upload, and carries the newer for Cloud to
    // extract: back's, sent after front's. #3 failed, so it moved front onto nothing.
    assert_eq!(deploy(4, &[], None).unwrap().upload, Some(upload('b')));
    let own = |name: &str, c: char| (ServiceName::parse(name).unwrap(), Some(upload(c).digest));
    assert_eq!(
        store.claim(&id(4), &a).unwrap().uploads,
        std::collections::BTreeMap::from([own("back", 'b'), own("front", 'a')])
    );
    store
        .record(&id(4), &a, RunEvidence::NotExecuted("stopped".into()))
        .unwrap();
    // It doesn't make back's upload front's: back alone still builds from b.
    assert_eq!(
        deploy(40, &["back"], None).unwrap().upload,
        Some(upload('b'))
    );
    end(40);
    assert_eq!(kept(&[1, 2]), [true, true]);
    // A retry of an older Deployment ships its upload, and back's latest upload stays.
    assert_eq!(retry(&store, &who, 5, 1).unwrap().upload, Some(upload('a')));
    end(5);
    assert_eq!(kept(&[1, 2]), [true, true]);
    // A Deployment that may still run keeps its upload, however many come after it.
    deploy(6, &["back"], Some(upload('c'))).unwrap();
    store.claim(&id(6), &a).unwrap();
    for n in 7..=30 {
        deploy(n, &["front"], None).unwrap();
    }
    deploy(31, &["back"], Some(upload('d'))).unwrap();
    // A newer upload replaced back's b; Deployment 99 was never admitted.
    assert_eq!(kept(&[1, 2, 6, 31, 99]), [true, false, true, true]);
}

/// The Store with an uploaded `app` Service.
fn uploaded_app() -> (ConfigStore, Actor) {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("app").unwrap(),
                image: None,
                template: None,
            },
        )
        .unwrap();
    (store, who)
}

/// Deploy `n` of every Service, sending `upload` if any.
fn deploy_sent(store: &ConfigStore, who: &Actor, n: u8, upload: Option<char>) -> DeploymentSummary {
    store
        .write_trusted(
            who,
            &Admit::Deploy(Deploy {
                id: id(n),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: upload.map(digested),
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &Trusted::default(),
        )
        .unwrap()
}

fn digested(c: char) -> UploadedSource {
    UploadedSource {
        digest: ployz_core::UploadDigest::parse(c.to_string().repeat(64)).unwrap(),
        base: None,
        uploader: None,
    }
}

fn fail(store: &ConfigStore, n: u8) {
    let a = runner("cli-a");
    store.claim(&id(n), &a).unwrap();
    store
        .record(&id(n), &a, RunEvidence::NotExecuted("stopped".into()))
        .unwrap();
}

fn kept_of(store: &ConfigStore, who: &Actor, deployments: &[u8]) -> Vec<bool> {
    store
        .read(
            who,
            &ployz_store::UploadsQuery {
                deployments: deployments.iter().map(|n| id(*n)).collect(),
            },
        )
        .unwrap()
        .uploads
        .iter()
        .map(|held| held.kept)
        .collect()
}

#[test]
fn a_failed_retry_of_a_released_upload_leaves_the_service_on_its_latest_upload() {
    let (store, who) = uploaded_app();
    let deploy = |n: u8, upload: Option<char>| deploy_sent(&store, &who, n, upload);
    deploy(1, Some('a'));
    fail(&store, 1);
    deploy(2, Some('b'));
    fail(&store, 2);
    deploy(3, Some('a'));
    fail(&store, 3);
    // a sent again replaced b: Cloud releases b's chunks.
    assert_eq!(kept_of(&store, &who, &[2, 3]), [false, true]);
    // The retry of b finds no upload and fails.
    retry(&store, &who, 4, 2).unwrap();
    fail(&store, 4);
    // The next settings-only Deploy builds from a, which Cloud still holds.
    let next = deploy(5, None);
    assert_eq!(next.upload, Some(digested('a')));
    let claimed = store.claim(&id(5), &runner("cli-a")).unwrap();
    assert_eq!(
        claimed.uploads,
        std::collections::BTreeMap::from([(
            ServiceName::parse("app").unwrap(),
            Some(digested('a').digest)
        )])
    );
    assert_eq!(kept_of(&store, &who, &[3]), [true]);
}

#[test]
fn the_same_upload_sent_again_keeps_one_copy() {
    let (store, who) = uploaded_app();
    let deploy = |n: u8, upload: Option<char>| deploy_sent(&store, &who, n, upload);
    // `ployz up` twice with unchanged source: two Deployments send the same content.
    deploy(1, Some('a'));
    fail(&store, 1);
    deploy(2, Some('a'));
    fail(&store, 2);
    assert_eq!(kept_of(&store, &who, &[1, 2]), [false, true]);
    // A settings-only Deploy builds from it, and the one copy stays.
    assert_eq!(deploy(3, None).upload, Some(digested('a')));
    assert_eq!(kept_of(&store, &who, &[2]), [true]);
}

#[test]
fn a_reupload_stays_kept_whether_a_retry_of_an_older_upload_comes_before_or_after_it() {
    let upload = |c: char| UploadedSource {
        digest: ployz_core::UploadDigest::parse(c.to_string().repeat(64)).unwrap(),
        base: None,
        uploader: None,
    };
    // Cloud holds chunks only for the Deployments that sent an upload, and asks about those.
    for retry_first in [false, true] {
        let (store, who) = shop();
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("app").unwrap(),
                    image: None,
                    template: None,
                },
            )
            .unwrap();
        let deploy = |n: u8, upload: Option<UploadedSource>| {
            store
                .write_trusted(
                    &who,
                    &Admit::Deploy(Deploy {
                        id: id(n),
                        environment: EnvironmentRef::default(),
                        services: Vec::new(),
                        version: None,
                        upload,
                        accept_volume_loss: Vec::new(),
                        message: None,
                        prune: false,
                        no_prune: false,
                    }),
                    &Trusted::default(),
                )
                .unwrap()
        };
        let a = runner("cli-a");
        let end = |n: u8| {
            store.claim(&id(n), &a).unwrap();
            store
                .record(&id(n), &a, RunEvidence::NotExecuted("stopped".into()))
                .unwrap();
        };
        let kept = |deployments: &[u8]| -> Vec<bool> {
            store
                .read(
                    &who,
                    &ployz_store::UploadsQuery {
                        deployments: deployments.iter().map(|n| id(*n)).collect(),
                    },
                )
                .unwrap()
                .uploads
                .iter()
                .map(|held| held.kept)
                .collect()
        };
        deploy(1, Some(upload('a')));
        end(1);
        // A second upload fails.
        deploy(2, Some(upload('b')));
        end(2);
        if retry_first {
            // Its retry builds b again, then the user uploads a again: the newest upload wins.
            retry(&store, &who, 3, 2).unwrap();
            end(3);
            deploy(4, Some(upload('a')));
            end(4);
            // #4 sent a again: one copy of a stays.
            assert_eq!(kept(&[1, 2, 4]), [false, false, true]);
        } else {
            // The user uploads a again, then retries the failed one: a is still the newest
            // upload, kept once. The failed retry moved the Service onto nothing, so b goes.
            deploy(3, Some(upload('a')));
            end(3);
            retry(&store, &who, 4, 2).unwrap();
            end(4);
            assert_eq!(kept(&[1, 2, 3]), [false, false, true]);
        }
    }
}

#[test]
fn an_upload_is_recorded_kept_for_later_deployments_and_its_receipts_come_back() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("app").unwrap(),
                image: None,
                template: None,
            },
        )
        .unwrap();
    let upload = UploadedSource {
        digest: ployz_core::UploadDigest::parse("d".repeat(64)).unwrap(),
        base: Some(UploadBase {
            commit: backend::sha(&"c".repeat(40)),
            changed: true,
        }),
        uploader: None,
    };
    let with = |n: u8, upload: Option<UploadedSource>| {
        store.write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id(n),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &ployz_store::Trusted::default(),
        )
    };
    // A digest is a lowercase sha256, checked as it is read.
    let mut bad = serde_json::to_value(&upload).unwrap();
    bad["digest"] = json!("D".repeat(64));
    assert!(serde_json::from_value::<UploadedSource>(bad).is_err());
    let first = with(1, Some(upload.clone())).unwrap();
    assert_eq!(first.upload.as_ref(), Some(&upload));
    let a = runner("cli-a");
    let claimed = store.claim(&id(1), &a).unwrap();
    // The runner prepares the Service without a source from the lowering input.
    assert!(
        claimed
            .intent
            .target
            .iter()
            .all(|spec| spec.name.as_str() != "app")
    );
    assert_eq!(
        claimed.input["snapshots"][2]["config"]["source"]["type"],
        json!("empty")
    );
    assert!(claimed.receipts.is_empty());
    let receipt = json!({"fingerprint": "f".repeat(64)});
    let built =
        |receipt: Value| RunEvidence::Built([(ServiceName::parse("app").unwrap(), receipt)].into());
    assert_eq!(
        code(store.record(&id(1), &runner("cli-b"), built(receipt.clone()))),
        RpcErrorCode::Conflict
    );
    assert_eq!(
        code(store.record(&id(1), &a, built(json!("image")))),
        RpcErrorCode::InvalidArgument
    );
    store.record(&id(1), &a, built(receipt.clone())).unwrap();
    // Built from an upload, it lists as uploaded, not empty.
    let listed = store
        .read(&who, &ployz_store::ServicesQuery::default())
        .unwrap();
    let app = listed
        .services
        .iter()
        .find(|listing| listing.service.name.as_str() == "app")
        .unwrap();
    assert_eq!(app.source, ployz_store::SourceKind::Uploaded);
    store
        .record(&id(1), &a, RunEvidence::NotExecuted("stopped".into()))
        .unwrap();
    // A later Deployment without a new upload keeps building from the latest one.
    let second = with(2, None).unwrap();
    assert_eq!(second.upload.as_ref(), Some(&upload));
    let claimed = store.claim(&id(2), &a).unwrap();
    assert_eq!(
        claimed.receipts.get(&ServiceName::parse("app").unwrap()),
        Some(&vec![receipt.clone()])
    );
    assert_eq!(
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
            .unwrap()
            .deployment
            .upload,
        Some(upload.clone())
    );
    // A retry ships the failed one's upload, not the Environment's latest.
    store
        .record(&id(2), &a, RunEvidence::NotExecuted("stopped".into()))
        .unwrap();
    let mut newer = upload.clone();
    newer.digest = ployz_core::UploadDigest::parse("e".repeat(64)).unwrap();
    with(3, Some(newer)).unwrap();
    assert_eq!(
        retry(&store, &who, 4, 1).unwrap().upload,
        Some(upload.clone())
    );
    // Once no Service builds from an upload, a Deployment carries none.
    store
        .write(
            &who,
            &RemoveService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("app").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(with(6, None).unwrap().upload, None);
    // Another Environment of the Project without its own receipt borrows this one;
    // preparation reuses it only if its fingerprint matches.
    let staging = EnvironmentRef {
        project: None,
        environment: Some(ployz_store::EnvironmentName::parse("staging").unwrap()),
    };
    store
        .write(
            &who,
            &ployz_store::CreateEnvironment {
                id: EnvironmentId::parse("00000000-0000-4000-8000-000000000099").unwrap(),
                project: None,
                name: ployz_store::EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000098").unwrap(),
                environment: staging.clone(),
                name: ServiceName::parse("app").unwrap(),
                image: None,
                template: None,
            },
        )
        .unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id(5),
                environment: staging,
                services: Vec::new(),
                version: None,
                upload: Some(upload),
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    assert_eq!(
        store.claim(&id(5), &a).unwrap().receipts[&ServiceName::parse("app").unwrap()],
        [receipt]
    );
}

#[test]
fn cloud_names_the_uploader_and_uploaded_builds_report_like_git_ones() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("app").unwrap(),
                image: None,
                template: None,
            },
        )
        .unwrap();
    let app = ServiceName::parse("app").unwrap();
    let upload = |uploader: Option<&str>| UploadedSource {
        digest: ployz_core::UploadDigest::parse("d".repeat(64)).unwrap(),
        base: None,
        uploader: uploader.map(|name| Principal::parse(name).unwrap()),
    };
    let admit = |n: u8, who: &Actor| {
        let command = Command::Admit(Admit::Deploy(Deploy {
            id: id(n),
            environment: EnvironmentRef::default(),
            services: Vec::new(),
            version: None,
            // A caller can't name the uploader: only Cloud's authentication does.
            upload: Some(upload(Some("mallory"))),
            accept_volume_loss: Vec::new(),
            message: None,
            prune: false,
            no_prune: false,
        }));
        // Cloud names who acts and what they hold; the Deployment records them as its
        // admitter and uploader.
        let held = Trusted {
            permissions: Some(Permission::ALL.iter().copied().collect()),
            ..Trusted::default()
        };
        let Written::Deployment(summary) = store.write_trusted(who, &command, &held).unwrap()
        else {
            panic!("an admit writes a Deployment");
        };
        summary
    };
    let nick = Actor {
        principal: Some(Principal::parse("nick").unwrap()),
        ..who.clone()
    };
    let admitted = admit(1, &nick);
    assert_eq!(admitted.upload, Some(upload(Some("nick"))));
    assert_eq!(admitted.admitted_by, nick.principal);
    assert!(admitted.admitted_at > 0);
    assert_eq!((admitted.started_at, admitted.ended_at), (None, None));
    assert_eq!(admit(2, &who).upload, Some(upload(None)));
    let a = runner("cloud-a");
    let claimed = store.claim(&id(2), &a).unwrap();
    assert_eq!(
        claimed.uploads,
        std::collections::BTreeMap::from([(app.clone(), Some(upload(None).digest))])
    );
    assert!(claimed.sources.is_empty());
    let report = |status| {
        RunEvidence::Build(ployz_store::BuildReport {
            service: app.clone(),
            status,
            message: None,
            log: "GitHub can't build uploaded source\n".into(),
            server: None,
        })
    };
    store
        .record(&id(2), &a, report(ployz_store::BuildStatus::Building))
        .unwrap();
    store
        .record(&id(2), &a, report(ployz_store::BuildStatus::Failed))
        .unwrap();
    store
        .record(&id(2), &a, RunEvidence::UploadNeeded(vec![app.clone()]))
        .unwrap();
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Failed);
    assert_eq!(view.builds.len(), 1);
    assert_eq!(view.builds[0].commit, None);
    assert_eq!(view.builds[0].status, ployz_store::BuildStatus::Failed);
    let Some(ployz_store::Outcome::NotExecuted { needs_upload, .. }) = view.deployment.outcome
    else {
        panic!("nothing executed");
    };
    assert_eq!(needs_upload, vec![app]);
    // A retry keeps the upload's provenance.
    assert_eq!(
        retry(&store, &who, 3, 2).unwrap().upload,
        Some(upload(None))
    );
}

#[test]
fn a_deploy_message_shows_on_the_deployment_and_its_retry() {
    let (store, who) = shop();
    let admit = |n: u8, message: String| {
        store.write(
            &who,
            &Admit::Deploy(Deploy {
                id: id(n),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: Some(message),
                prune: false,
                no_prune: false,
            }),
        )
    };
    let too_long = admit(1, "x".repeat(501)).unwrap_err();
    assert_eq!(too_long.code, RpcErrorCode::InvalidArgument);
    let admitted = admit(1, "  Ship the new header  ".into()).unwrap();
    assert_eq!(admitted.message.as_deref(), Some("Ship the new header"));
    store
        .write(&who, &ployz_store::Cancel { deployment: id(1) })
        .unwrap();
    let retried = store
        .write(
            &who,
            &Admit::Retry(ployz_store::Retry {
                id: id(2),
                deployment: id(1),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    assert_eq!(retried.message.as_deref(), Some("Ship the new header"));
    let shown = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap();
    assert_eq!(
        shown.deployment.message.as_deref(),
        Some("Ship the new header")
    );
}

#[test]
fn deploying_a_service_with_nothing_to_run_is_refused_naming_it() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("blank").unwrap(),
                image: None,
                template: None,
            },
        )
        .unwrap();
    let refused = admit(&store, &who, 1, &[], None).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        refused.message,
        "blank has nothing to run yet: add an image, connect a repository, or run ployz up in its directory"
    );
    assert_eq!(refused.details["service"], "blank");
    // Deploying other Services leaves it out.
    admit(&store, &who, 1, &["web"], None).unwrap();
}

#[test]
fn live_commands_find_containers_by_the_runtime_name_that_deployed() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    // A staged Private DNS change isn't live until it deploys.
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("web.privateDns").unwrap(),
                    value: json!("front"),
                }],
            },
        )
        .unwrap();
    let web = ployz_core::ServiceName::parse("web").unwrap();
    let namespace = store.read(&who, &NamespaceQuery::default()).unwrap();
    assert_eq!(namespace.services.get(&web), Some(&web));
    let deployment = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    assert_eq!(deployment.runtime_names.get(&web), Some(&web));
}

#[test]
fn a_deployment_is_found_by_its_number() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let by_number = |number| {
        store.read(
            &who,
            &ployz_store::NumberedDeploymentQuery {
                environment: EnvironmentRef::default(),
                number: Some(number),
            },
        )
    };
    assert_eq!(by_number(1).unwrap().deployment.id, id(1));
    assert_eq!(code(by_number(2)), RpcErrorCode::NotFound);
}

#[test]
fn unclaimed_lists_queued_deployments_no_runner_took() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let later = i64::MAX;
    let unclaimed = store.unclaimed(later).unwrap();
    assert_eq!(
        unclaimed
            .iter()
            .map(|unclaimed| (&unclaimed.deployment, unclaimed.number))
            .collect::<Vec<_>>(),
        [(&id(1), 1)]
    );
    assert!(store.unclaimed(0).unwrap().is_empty());
    store.claim(&id(1), &runner("runner-a")).unwrap();
    assert!(store.unclaimed(later).unwrap().is_empty());
}

#[test]
fn sources_of_an_unknown_deployment_are_not_found() {
    let (store, _) = shop();
    assert_eq!(code(store.sources(&id(9))), RpcErrorCode::NotFound);
}

#[test]
fn a_service_confirmed_mid_run_stays_deployed_when_the_runner_is_lost() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    let web = ServiceName::parse("web").unwrap();
    assert_eq!(
        code(store.record(&id(1), &a, RunEvidence::Confirmed(vec![web.clone()]))),
        RpcErrorCode::Conflict
    );
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, RunEvidence::Confirmed(vec![web]))
        .unwrap();
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::Pending),
            ("web".to_owned(), NodeStatus::Deployed)
        ]
    );
    store.record(&id(1), &a, RunEvidence::Abandoned).unwrap();
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("api".to_owned(), NodeStatus::Unknown),
            ("web".to_owned(), NodeStatus::Deployed)
        ]
    );
    // web is in Applied State: only api is still to deploy.
    assert_eq!(changed(&store, &who), ["api"]);
}

fn rewrite_run(url: &str, n: u8, rewrite: impl FnOnce(&mut Value)) {
    let id = id(n);
    let select = "SELECT run FROM config_deployment WHERE id = ";
    let update = "UPDATE config_deployment SET run = ";
    match url.strip_prefix("sqlite:") {
        Some(path) => {
            let db = rusqlite::Connection::open(path).unwrap();
            let text: String = db
                .query_row(&format!("{select}?1"), [id.as_str()], |row| row.get(0))
                .unwrap();
            let mut run = serde_json::from_str(&text).unwrap();
            rewrite(&mut run);
            db.execute(
                &format!("{update}?1 WHERE id = ?2"),
                [run.to_string().as_str(), id.as_str()],
            )
            .unwrap();
        }
        None => {
            let mut db = postgres::Client::connect(url, postgres::NoTls).unwrap();
            let text: String = db
                .query_one(&format!("{select}$1"), &[&id.as_str()])
                .unwrap()
                .get(0);
            let mut run = serde_json::from_str(&text).unwrap();
            rewrite(&mut run);
            db.execute(
                &format!("{update}$1 WHERE id = $2"),
                &[&run.to_string(), &id.as_str()],
            )
            .unwrap();
        }
    }
}

#[test]
fn a_preview_recorded_with_hook_environment_still_matches_its_replay_and_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    admit(&store, &who, 1, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    let run_web = json!({
        "type": "run_container", "machine_id": "a".repeat(32), "skip_health_monitor": false,
        "spec": {
            "service_id": "c".repeat(32), "name": "web", "mode": {"mode": "replicated", "replicas": 1},
            "container": {"image": "nginx:1", "pull_policy": "missing"},
            "pre_deploy": {"command": ["migrate"], "environment": {"DATABASE_URL": "hook-secret"}}
        }
    });
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": [{"index": 0, "machine_id": "a".repeat(32), "service_name": "web",
            "operation": run_web, "status": {"type": "pending"}}],
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview.clone()))
        .unwrap();
    rewrite_run(&url, 1, |run| {
        run["preview"]["operations"][0]["operation"]["spec"]["pre_deploy"]["environment"] =
            json!({"DATABASE_URL": "hook-secret"});
    });

    store
        .record(&id(1), &a, RunEvidence::Prepared(preview))
        .unwrap();
    let executed = store
        .record(
            &id(1),
            &a,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome(json!({"type": "success", "completed": [run_web]}))),
                removed: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(executed.status, DeploymentStatus::Applied);
}

#[test]
fn deleted_config_discard_does_not_target_same_name_replacement() {
    let (store, who) = shop();
    sentry(&store, &who);
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    let query = |selector: &str| ployz_store::ConfigItemQuery {
        environment: EnvironmentRef::default(),
        config: ployz_store::ConfigRef::parse(selector).unwrap(),
    };
    let old = store.read(&who, &query("sentry")).unwrap();
    let old_selector = format!("@{}", old.config.config.id);
    store
        .write(
            &who,
            &DeleteConfig {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("sentry").unwrap(),
            },
        )
        .unwrap();
    let replacement = ConfigId::parse("00000000-0000-4000-8000-000000000010").unwrap();
    store
        .write(
            &who,
            &CreateConfig {
                id: replacement.clone(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("sentry").unwrap(),
                mounts: vec![],
            },
        )
        .unwrap();
    put_sentry(&store, &who, "REPLACEMENT_TEXT_MUST_SURVIVE");
    let before = store.read(&who, &ConfigsQuery::default()).unwrap();
    assert_eq!(
        before
            .configs
            .iter()
            .filter(|c| c.config.name.as_str() == "sentry")
            .count(),
        2
    );
    let replacement_before = store.read(&who, &query("sentry")).unwrap();
    assert_eq!(replacement_before.config.config.id, replacement);
    assert_eq!(
        store.read(&who, &query(&old_selector)).unwrap().contents,
        old.contents
    );
    let discard = |path: &str| {
        store.write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse(path).unwrap()),
                version: None,
            },
        )
    };
    assert_eq!(
        discard("configs.sentry").unwrap_err().code,
        RpcErrorCode::Ambiguous
    );
    assert_eq!(
        discard(&format!("configs.{old_selector}"))
            .unwrap_err()
            .code,
        RpcErrorCode::Conflict
    );
    assert_eq!(store.read(&who, &ConfigsQuery::default()).unwrap(), before);
    assert_eq!(
        store.read(&who, &query("sentry")).unwrap(),
        replacement_before
    );
    let missing = "@00000000-0000-4000-8000-000000000099";
    assert_eq!(
        store.read(&who, &query(missing)).unwrap_err().code,
        RpcErrorCode::NotFound
    );
    discard(&format!("configs.@{replacement}")).unwrap();
    assert_eq!(
        store
            .read(&who, &query(&format!("@{replacement}")))
            .unwrap_err()
            .code,
        RpcErrorCode::NotFound
    );
    discard(&format!("configs.{old_selector}")).unwrap();
    assert_eq!(
        store.read(&who, &query(&old_selector)).unwrap().contents,
        old.contents
    );
}

#[test]
fn discard_config_mount_preserves_same_name_replacement() {
    let (store, who) = shop();
    sentry(&store, &who);
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    store
        .write(
            &who,
            &DeleteConfig {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("sentry").unwrap(),
            },
        )
        .unwrap();
    let replacement = ConfigId::parse("00000000-0000-4000-8000-000000000010").unwrap();
    store
        .write(
            &who,
            &CreateConfig {
                id: replacement.clone(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("sentry").unwrap(),
                mounts: vec![ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/replacement".into(),
                }],
            },
        )
        .unwrap();
    put_sentry(&store, &who, "REPLACEMENT_TEXT_MUST_SURVIVE");
    let query = ployz_store::ConfigItemQuery {
        environment: EnvironmentRef::default(),
        config: replacement.clone().into(),
    };
    let before = store.read(&who, &query).unwrap();
    let review = diff(&store, &who);
    let web = review
        .changes
        .iter()
        .find(|change| change.name == "web")
        .unwrap();
    let removed = web
        .settings
        .iter()
        .find(|row| row.before == json!("/etc/sentry") && row.after.is_null())
        .unwrap();
    let result = store.write(
        &who,
        &Discard {
            environment: EnvironmentRef::default(),
            target: Some(ployz_store::DiscardTarget::Head),
            path: Some(SettingPath::parse(&removed.path).unwrap()),
            version: Some(review.version.clone()),
        },
    );
    assert_eq!(result.unwrap_err().code, ployz_core::RpcErrorCode::Conflict);
    assert_eq!(removed.config_name.as_ref().unwrap().as_str(), "sentry");
    let ambiguous = store
        .write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse("web.configs.sentry").unwrap()),
                version: None,
            },
        )
        .unwrap_err();
    assert_eq!(ambiguous.code, ployz_core::RpcErrorCode::Ambiguous);
    assert_eq!(
        ambiguous.details["valid_children"],
        json!([
            "configs.@00000000-0000-4000-8000-000000000009",
            "configs.@00000000-0000-4000-8000-000000000010",
        ])
    );
    let after = store.read(&who, &query).unwrap();
    assert_eq!(diff(&store, &who), review);
    assert_eq!(after.contents, before.contents);
    assert_eq!(
        after.config.mounts, before.config.mounts,
        "Discarding A's deleted mount must preserve B's independently authored replacement mount"
    );
}

/// Cloud's evidence that nothing runs beyond what the Environment holds.
fn nothing_else_runs() -> Option<RunningObservation> {
    Some(RunningObservation::default())
}

fn deploy_as(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    approval: Approval,
) -> Result<DeploymentSummary, RpcError> {
    deploy_trusted(
        store,
        who,
        n,
        &Trusted {
            approval,
            running: nothing_else_runs(),
            ..Trusted::default()
        },
    )
}

fn agent_deploys(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    approval: Approval,
) -> Result<DeploymentSummary, RpcError> {
    deploy_trusted(
        store,
        who,
        n,
        &Trusted {
            approval,
            asks: Asks::EveryDeploy,
            running: nothing_else_runs(),
            ..Trusted::default()
        },
    )
}

fn deploy_trusted(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    deploy_choosing(store, who, n, trusted, |_| {})
}

/// A Deploy of every Service under `trusted`, as `choose` sets its prune choice.
fn deploy_choosing(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    trusted: &Trusted,
    choose: impl FnOnce(&mut Deploy),
) -> Result<DeploymentSummary, RpcError> {
    let mut deploy = Deploy {
        id: id(n),
        environment: EnvironmentRef::default(),
        services: Vec::new(),
        version: None,
        upload: None,
        accept_volume_loss: Vec::new(),
        message: None,
        prune: false,
        no_prune: false,
    };
    choose(&mut deploy);
    store.write_trusted(who, &Admit::Deploy(deploy), trusted)
}

/// Cloud's evidence for a Deploy token: its grant leaves out `changes:destroy`.
fn deploy_token() -> Trusted {
    Trusted {
        permissions: Some(
            Permission::ALL
                .iter()
                .copied()
                .filter(|permission| *permission != Permission::ChangesDestroy)
                .collect(),
        ),
        ..Trusted::default()
    }
}

/// Cloud's evidence that the Servers run `services` in shop-production: `web`,
/// `api`, and any the Environment never held.
fn running(services: &[&str]) -> Option<RunningObservation> {
    Some(RunningObservation {
        services: services
            .iter()
            .map(|service| QualifiedService::parse(format!("shop-production/{service}")).unwrap())
            .chain([QualifiedService::parse("other-production/extra2").unwrap()])
            .collect(),
        unanswered: Vec::new(),
        failure: None,
        dropped: Vec::new(),
    })
}

fn removes(services: &[&str]) -> Vec<(String, String)> {
    services
        .iter()
        .map(|service| ("removes_service".to_owned(), (*service).to_owned()))
        .collect()
}

fn effects(refused: &RpcError) -> Vec<(String, String)> {
    refused.details["effects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|effect| {
            (
                effect["kind"].as_str().unwrap().to_owned(),
                effect["name"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn a_deploy_token_cant_prune_what_its_environment_never_held() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    let token = |running| Trusted {
        running,
        ..deploy_token()
    };
    let refused =
        deploy_trusted(&store, &who, 2, &token(running(&["web", "api", "extra2"]))).unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert!(
        refused.message.starts_with("Needs changes:destroy"),
        "{}",
        refused.message
    );
    assert!(refused.message.contains("extra2"), "{}", refused.message);
    assert!(
        refused.message.contains("--no-prune"),
        "{}",
        refused.message
    );
    // When Cloud couldn't look, it can't tell the prune removes nothing.
    let unseen = deploy_trusted(&store, &who, 2, &token(None)).unwrap_err();
    assert_eq!(unseen.code.as_str(), "forbidden", "{unseen:?}");
    assert!(unseen.message.contains("--no-prune"), "{}", unseen.message);
    // An older client's explicit prune is refused too.
    let older = deploy_choosing(&store, &who, 2, &token(running(&["extra2"])), |deploy| {
        deploy.prune = true;
    })
    .unwrap_err();
    assert_eq!(older.code.as_str(), "forbidden");
    // Keeping them, or a prune that removes nothing, deploys.
    let kept = deploy_choosing(&store, &who, 2, &token(None), |deploy| {
        deploy.no_prune = true
    })
    .unwrap();
    assert!(!kept.prune);
    cancel(&store, &who, 2).unwrap();
    assert!(
        deploy_trusted(&store, &who, 3, &token(running(&["web", "api"])))
            .unwrap()
            .prune
    );
}

#[test]
fn an_agents_ask_names_each_service_its_prune_removes_and_its_approval_binds_them() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    let agent = |approval, running| Trusted {
        approval,
        asks: Asks::EveryDeploy,
        running,
        ..Trusted::default()
    };
    let asked = deploy_trusted(
        &store,
        &who,
        2,
        &agent(Approval::Required, running(&["web", "api", "extra2"])),
    )
    .unwrap_err();
    assert_eq!(effects(&asked), removes(&["extra2"]), "{asked:?}");
    assert_eq!(asked.details.get("prunes"), None);
    // Unseen, the ask says it prunes without naming what.
    let unseen = deploy_trusted(&store, &who, 2, &agent(Approval::Required, None)).unwrap_err();
    assert_eq!(unseen.details["prunes"], true, "{unseen:?}");
    let keep = |deploy: &mut Deploy| deploy.no_prune = true;
    let keeps =
        deploy_choosing(&store, &who, 2, &agent(Approval::Required, None), keep).unwrap_err();
    assert_eq!(keeps.details.get("prunes"), None, "{keeps:?}");
    // Approving the Deploy that keeps them doesn't admit the one that prunes.
    let refused = deploy_trusted(&store, &who, 2, &agent(approved(&keeps), None)).unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    let pruning = deploy_trusted(
        &store,
        &who,
        2,
        &agent(approved(&asked), running(&["web", "api", "extra2"])),
    )
    .unwrap();
    assert!(pruning.prune);
}

#[test]
fn a_prune_that_removes_a_service_asks_first_though_nothing_else_destroys() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    let person = |approval| Trusted {
        approval,
        running: running(&["web", "api", "extra2"]),
        ..Trusted::default()
    };
    let asked = deploy_trusted(&store, &who, 2, &person(Approval::Required)).unwrap_err();
    assert_eq!(asked.code.as_str(), "approval_required", "{asked:?}");
    assert_eq!(effects(&asked), removes(&["extra2"]));
    assert!(
        deploy_trusted(&store, &who, 2, &person(approved(&asked)))
            .unwrap()
            .prune
    );
}

#[test]
fn a_renamed_service_runs_as_its_old_name_so_a_deploy_token_still_deploys() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    store
        .write(
            &who,
            &RenameService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                name: ServiceName::parse("frontend").unwrap(),
            },
        )
        .unwrap();
    let token = Trusted {
        running: running(&["web", "api"]),
        ..deploy_token()
    };
    assert!(deploy_trusted(&store, &who, 2, &token).unwrap().prune);
}

#[test]
fn a_server_that_didnt_answer_leaves_the_prune_unseen() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    let partial = |services: &[&str]| {
        running(services).map(|running| RunningObservation {
            unanswered: vec![MachineId::parse("e".repeat(32)).unwrap()],
            ..running
        })
    };
    let token = Trusted {
        running: partial(&["web", "api"]),
        ..deploy_token()
    };
    let refused = deploy_trusted(&store, &who, 2, &token).unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert!(
        refused.message.contains("--no-prune"),
        "{}",
        refused.message
    );
    let agent = Trusted {
        approval: Approval::Required,
        asks: Asks::EveryDeploy,
        running: partial(&["web", "api", "extra2"]),
        ..Trusted::default()
    };
    let asked = deploy_trusted(&store, &who, 2, &agent).unwrap_err();
    assert_eq!(effects(&asked), removes(&["extra2"]), "{asked:?}");
    assert_eq!(asked.details["prunes"], true);
}

#[test]
fn an_unseen_prune_says_which_servers_drop_large_packets() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    let unanswered = |dropped: &[&str]| {
        running(&["web", "api"]).map(|running| RunningObservation {
            unanswered: vec![MachineId::parse("e".repeat(32)).unwrap()],
            dropped: dropped
                .iter()
                .map(|name| MachineName::parse(*name).unwrap())
                .collect(),
            ..running
        })
    };
    let token = Trusted {
        running: unanswered(&["machine-3"]),
        ..deploy_token()
    };
    let refused = deploy_trusted(&store, &who, 2, &token).unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert!(
        refused
            .message
            .contains("(large packets to machine-3 are dropped on the mesh path)"),
        "{}",
        refused.message
    );
    let agent = |approval, dropped: &[&str]| Trusted {
        approval,
        asks: Asks::EveryDeploy,
        running: unanswered(dropped),
        ..Trusted::default()
    };
    let asked = deploy_trusted(&store, &who, 2, &agent(Approval::Required, &[])).unwrap_err();
    assert_eq!(asked.details["prunes"], true);
    assert!(asked.details.get("dropped").is_none(), "{asked:?}");
    let explained =
        deploy_trusted(&store, &who, 2, &agent(Approval::Required, &["machine-3"])).unwrap_err();
    assert_eq!(explained.details["dropped"], json!(["machine-3"]));
    // Only an explanation: the approval given without it still admits.
    assert!(
        deploy_trusted(&store, &who, 2, &agent(approved(&asked), &["machine-3"]))
            .unwrap()
            .prune
    );
}

#[test]
fn an_unseen_prune_asks_first_where_destroys_ask() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    let person = Trusted {
        approval: Approval::Required,
        ..Trusted::default()
    };
    let asked = deploy_trusted(&store, &who, 2, &person).unwrap_err();
    assert_eq!(asked.code.as_str(), "approval_required", "{asked:?}");
    assert_eq!(asked.details["prunes"], true);
    assert!(
        deploy_trusted(
            &store,
            &who,
            2,
            &Trusted {
                approval: approved(&asked),
                ..person
            }
        )
        .is_ok()
    );
}

#[test]
fn a_deploy_token_cant_redeploy_a_prune_that_removes_what_runs_now() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    let redeploy = |n: u8, running| {
        store.write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(n),
                deployment: id(1),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &Trusted {
                running,
                ..deploy_token()
            },
        )
    };
    let refused = redeploy(2, running(&["web", "api", "extra2"])).unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert!(refused.message.contains("extra2"), "{}", refused.message);
    assert!(redeploy(2, running(&["web", "api"])).unwrap().prune);
}

#[test]
fn an_approval_given_while_every_server_answered_doesnt_admit_an_unseen_prune() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    let agent = |approval, running| Trusted {
        approval,
        asks: Asks::EveryDeploy,
        running,
        ..Trusted::default()
    };
    let seen = deploy_trusted(
        &store,
        &who,
        2,
        &agent(Approval::Required, running(&["web", "api"])),
    )
    .unwrap_err();
    let unseen = deploy_trusted(&store, &who, 2, &agent(approved(&seen), None)).unwrap_err();
    assert_eq!(unseen.code.as_str(), "approval_required", "{unseen:?}");
    assert_eq!(unseen.details["prunes"], true);
    let redeploy = |approval, running| {
        store.write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(3),
                deployment: id(1),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &agent(approval, running),
        )
    };
    let seen = redeploy(Approval::Required, running(&["web", "api"])).unwrap_err();
    let unseen = redeploy(approved(&seen), None).unwrap_err();
    assert_eq!(unseen.code.as_str(), "approval_required", "{unseen:?}");
}

fn apply_both(store: &ConfigStore, who: &Actor, n: u8) {
    admit(store, who, n, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(n), &a).unwrap();
    store
        .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(n), &a, succeeded(&["web", "api"]))
        .unwrap();
}

fn remove_web(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &RemoveService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
}

#[test]
fn a_deploy_naming_a_service_staged_for_removal_says_a_full_deploy_removes_it() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    remove_web(&store, &who);
    let refused = admit(&store, &who, 2, &["web"], None).unwrap_err();
    assert_eq!(refused.code.as_str(), "not_found");
    assert_eq!(
        refused.message,
        "Service web is staged for removal; a full Deploy removes it"
    );
    let missing = admit(&store, &who, 2, &["shop"], None).unwrap_err();
    assert_eq!(missing.message, "No Service named shop to deploy");
}

fn approved(refused: &RpcError) -> Approval {
    assert_eq!(refused.code.as_str(), "approval_required");
    let approval = refused.details["approval"].as_str().unwrap();
    Approval::Approved(ApprovalDigest::parse(approval).unwrap())
}

#[test]
fn a_deploy_that_removes_a_deployed_service_waits_for_approval() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    remove_web(&store, &who);
    let refused = deploy_as(&store, &who, 2, Approval::Required).unwrap_err();
    assert_eq!(changed(&store, &who), ["web"]);
    deploy_as(&store, &who, 2, approved(&refused)).unwrap();
    assert!(changed(&store, &who).is_empty());
}

#[test]
fn a_new_deploy_asks_again_for_a_removal_still_queued() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    remove_web(&store, &who);
    let refused = deploy_as(&store, &who, 2, Approval::Required).unwrap_err();
    deploy_as(&store, &who, 2, approved(&refused)).unwrap();
    set_replicas(&store, &who, "api", 2);
    let again = deploy_as(&store, &who, 3, Approval::Required).unwrap_err();
    assert_eq!(again.details["effects"][0]["name"], "web");
    assert_ne!(again.details["approval"], refused.details["approval"]);
}

#[test]
fn deploying_or_retrying_saved_state_asks_nothing_again() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    remove_web(&store, &who);
    let refused = deploy_as(&store, &who, 2, Approval::Required).unwrap_err();
    deploy_as(&store, &who, 2, approved(&refused)).unwrap();
    cancel(&store, &who, 2).unwrap();
    deploy_as(&store, &who, 3, Approval::Required).unwrap();
    cancel(&store, &who, 3).unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(4),
                deployment: id(2),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &Trusted {
                approval: Approval::Required,
                running: nothing_else_runs(),
                ..Trusted::default()
            },
        )
        .unwrap();
}

#[test]
fn an_agent_deploy_that_destroys_nothing_waits_for_approval() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    let refused = agent_deploys(&store, &who, 2, Approval::Required).unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    assert_eq!(refused.message, "A human must approve this deploy first.");
    assert_eq!(refused.details["effects"], json!([]));
    let version = refused.details["diff"]["version"].as_str().unwrap();
    let approval = refused.details["approval"].as_str().unwrap();
    assert!(approval.starts_with(&format!("{version}:")), "{approval}");
    assert_eq!(changed(&store, &who), ["api"]);
    agent_deploys(&store, &who, 2, approved(&refused)).unwrap();
    assert!(changed(&store, &who).is_empty());
}

#[test]
fn an_agent_approval_refuses_once_the_plan_changes() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    let refused = agent_deploys(&store, &who, 2, Approval::Required).unwrap_err();
    set_replicas(&store, &who, "api", 3);
    let again = agent_deploys(&store, &who, 2, approved(&refused)).unwrap_err();
    assert_eq!(again.code.as_str(), "approval_required");
    assert_ne!(again.details["approval"], refused.details["approval"]);
}

#[test]
fn an_agent_approval_admits_one_deploy() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    let refused = agent_deploys(&store, &who, 2, Approval::Required).unwrap_err();
    let approval = approved(&refused);
    agent_deploys(&store, &who, 2, approval.clone()).unwrap();
    agent_deploys(&store, &who, 2, approval.clone()).unwrap();
    let again = agent_deploys(&store, &who, 3, approval).unwrap_err();
    assert_eq!(again.code.as_str(), "approval_required");
    assert_ne!(again.details["approval"], refused.details["approval"]);
}

fn agent_deploys_only(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    service: &str,
    approval: Approval,
) -> Result<DeploymentSummary, RpcError> {
    store.write_trusted(
        who,
        &Admit::Deploy(Deploy {
            id: id(n),
            environment: EnvironmentRef::default(),
            services: vec![ServiceName::parse(service).unwrap()],
            version: None,
            upload: None,
            accept_volume_loss: Vec::new(),
            message: None,
            prune: false,
            no_prune: false,
        }),
        &Trusted {
            approval,
            asks: Asks::EveryDeploy,
            ..Trusted::default()
        },
    )
}

#[test]
fn an_agent_approval_admits_only_the_services_it_named() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "web", 2);
    set_replicas(&store, &who, "api", 2);
    let refused = agent_deploys_only(&store, &who, 2, "web", Approval::Required).unwrap_err();
    let other = agent_deploys_only(&store, &who, 2, "api", approved(&refused)).unwrap_err();
    assert_eq!(other.code.as_str(), "approval_required");
    assert_ne!(other.details["approval"], refused.details["approval"]);
    agent_deploys_only(&store, &who, 2, "web", approved(&refused)).unwrap();
}

fn agent_deploys_upload(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    upload: char,
    approval: Approval,
) -> Result<DeploymentSummary, RpcError> {
    store.write_trusted(
        who,
        &Admit::Deploy(Deploy {
            id: id(n),
            environment: EnvironmentRef::default(),
            services: Vec::new(),
            version: None,
            upload: Some(UploadedSource {
                digest: ployz_core::UploadDigest::parse(upload.to_string().repeat(64)).unwrap(),
                base: None,
                uploader: None,
            }),
            accept_volume_loss: Vec::new(),
            message: None,
            prune: false,
            no_prune: false,
        }),
        &Trusted {
            approval,
            asks: Asks::EveryDeploy,
            ..Trusted::default()
        },
    )
}

#[test]
fn an_agent_approval_admits_only_the_upload_it_named() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    let refused = agent_deploys_upload(&store, &who, 2, 'a', Approval::Required).unwrap_err();
    let other = agent_deploys_upload(&store, &who, 2, 'b', approved(&refused)).unwrap_err();
    assert_eq!(other.code.as_str(), "approval_required");
    assert_ne!(other.details["approval"], refused.details["approval"]);
    let none = agent_deploys(&store, &who, 2, approved(&refused)).unwrap_err();
    assert_eq!(none.code.as_str(), "approval_required");
    agent_deploys_upload(&store, &who, 2, 'a', approved(&refused)).unwrap();
}

fn agent_retries(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    source: u8,
    approval: Approval,
) -> Result<DeploymentSummary, RpcError> {
    store.write_trusted(
        who,
        &Admit::Retry(Retry {
            id: id(n),
            deployment: id(source),
            rolls_back: None,
            version: None,
            accept_volume_loss: Vec::new(),
        }),
        &Trusted {
            approval,
            asks: Asks::EveryDeploy,
            ..Trusted::default()
        },
    )
}

#[test]
fn a_token_outside_its_environments_is_refused_before_a_human_is_asked() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    cancel(&store, &who, 2).unwrap();
    let staging = Trusted {
        approval: Approval::Required,
        asks: Asks::EveryDeploy,
        environments: Some([ployz_store::EnvironmentName::parse("staging").unwrap()].into()),
        ..Trusted::default()
    };
    let deploy = deploy_trusted(&store, &who, 3, &staging).unwrap_err();
    assert_eq!(deploy.code.as_str(), "forbidden", "{deploy:?}");
    assert_eq!(deploy.message, "This token can deploy only to staging.");
    let retry = store
        .write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(3),
                deployment: id(2),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &staging,
        )
        .unwrap_err();
    assert_eq!(retry.code.as_str(), "forbidden", "{retry:?}");
    assert_eq!(retry.message, "This token can deploy only to staging.");
}

#[test]
fn an_agent_approval_admits_one_retry() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    cancel(&store, &who, 2).unwrap();
    set_replicas(&store, &who, "web", 3);
    let refused = agent_retries(&store, &who, 3, 2, Approval::Required).unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    assert_eq!(refused.message, "A human must approve this redeploy first.");
    assert_eq!(refused.details["effects"], json!([]));
    assert_eq!(
        refused.details["diff"]["changes"],
        json!([]),
        "a retry ships none of the staged changes"
    );
    let version = refused.details["diff"]["version"].as_str().unwrap();
    let approval = refused.details["approval"].as_str().unwrap();
    assert!(approval.starts_with(&format!("{version}:")), "{approval}");
    agent_retries(&store, &who, 3, 2, approved(&refused)).unwrap();
    agent_retries(&store, &who, 3, 2, approved(&refused)).unwrap();
    cancel(&store, &who, 3).unwrap();
    let again = agent_retries(&store, &who, 4, 2, approved(&refused)).unwrap_err();
    assert_eq!(again.code.as_str(), "approval_required");
    assert_ne!(again.details["approval"], refused.details["approval"]);
}

#[test]
fn an_agent_deploy_of_saved_state_asks() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    publish_asking(&store, &who, Approval::Required).unwrap();
    let refused = agent_deploys(&store, &who, 2, Approval::Required).unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    agent_deploys(&store, &who, 2, approved(&refused)).unwrap();
}

#[test]
fn a_deploy_that_destroys_nothing_asks_nothing_unless_an_agent_asked() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    deploy_as(&store, &who, 2, Approval::Required).unwrap();
}

#[test]
fn an_agent_save_that_destroys_nothing_asks_nothing() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    store
        .write_trusted(
            &who,
            &Command::Publish(Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
            &Trusted {
                approval: Approval::Required,
                asks: Asks::EveryDeploy,
                ..Trusted::default()
            },
        )
        .unwrap();
}

fn publish_asking(
    store: &ConfigStore,
    who: &Actor,
    approval: Approval,
) -> Result<Written, RpcError> {
    store.write_trusted(
        who,
        &Command::Publish(Publish {
            id: None,
            environment: EnvironmentRef::default(),
            version: None,
            message: None,
            accept_volume_loss: Vec::new(),
        }),
        &Trusted {
            approval,
            ..Trusted::default()
        },
    )
}

fn asked(refused: &RpcError) -> Vec<(Value, Value)> {
    refused.details["effects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|effect| (effect["kind"].clone(), effect["name"].clone()))
        .collect()
}

#[test]
fn removing_a_service_the_deployment_in_flight_creates_asks() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    remove_web(&store, &who);
    let refused = publish_asking(&store, &who, Approval::Required).unwrap_err();
    assert_eq!(asked(&refused), [(json!("removes_service"), json!("web"))]);
    publish_asking(&store, &who, approved(&refused)).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    deploy_as(&store, &who, 2, Approval::Required).unwrap();
}

#[test]
fn a_newer_queued_deployment_keeps_a_running_ones_creates_deployed() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    store.claim(&id(1), &runner("runner-a")).unwrap();
    set_replicas(&store, &who, "api", 2);
    admit(&store, &who, 2, &["api"], None).unwrap();
    remove_web(&store, &who);
    let refused = publish_asking(&store, &who, Approval::Required).unwrap_err();
    assert_eq!(asked(&refused), [(json!("removes_service"), json!("web"))]);
}

#[test]
fn a_queued_retry_keeps_a_running_deployments_creates_deployed() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    store.claim(&id(1), &runner("runner-a")).unwrap();
    admit(&store, &who, 2, &["api"], None).unwrap();
    cancel(&store, &who, 2).unwrap();
    store
        .write(
            &who,
            &Admit::Retry(Retry {
                id: id(3),
                deployment: id(2),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    remove_web(&store, &who);
    let refused = publish_asking(&store, &who, Approval::Required).unwrap_err();
    assert_eq!(asked(&refused), [(json!("removes_service"), json!("web"))]);
}

#[test]
fn removing_a_volume_no_service_mounts_asks_nothing_while_its_deployment_runs() {
    let (store, who) = shop();
    let data = ployz_store::VolumeName::parse("data").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateVolume {
                id: ployz_store::VolumeId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: data.clone(),
                storage: ployz_core::config::VolumeKind::Docker {},
                shared_writes: false,
                mounts: Vec::new(),
                storage_locked: false,
                restored_from: None,
            },
        )
        .unwrap();
    admit(&store, &who, 1, &[], None).unwrap();
    store
        .write(
            &who,
            &ployz_store::RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: data,
            },
        )
        .unwrap();
    assert!(diff(&store, &who).effects.is_empty());
    publish_asking(&store, &who, Approval::Required).unwrap();
}

fn failed_row(snapshots: &[Vec<OperationRow>]) -> RowState {
    let mut tracker = RowTracker::default();
    let mut last = None;
    for snapshot in snapshots {
        for change in tracker.changes(snapshot) {
            last = Some(change.state);
        }
    }
    last.unwrap()
}

#[test]
fn a_missing_image_reads_as_one_sentence_at_the_pull_step() {
    let docker = "Docker responded with status code 404: failed to resolve reference \"docker.io/library/nginx:doesnotexist-soak\": docker.io/library/nginx:doesnotexist-soak: not found";
    let pull = json!({"type": "failed", "error": {
        "type": "machine", "action": "PullImage",
        "error": {
            "code": "not_found",
            "message": "image pull failed",
            "details": {"missing_image": "nginx:doesnotexist-soak"},
            "cause": [docker],
        }
    }});
    let RowState::Failed {
        reason,
        cause,
        step,
        ..
    } = failed_row(&[progress(&[("web", "alpha", pull)])])
    else {
        panic!("expected a failed row");
    };
    assert_eq!(
        reason,
        "Image nginx:doesnotexist-soak not found, or its registry needs credentials"
    );
    assert_eq!(cause, vec![docker.to_owned()]);
    assert_eq!(step, Some(FailedAt::Pull));
}

#[test]
fn a_full_disk_names_itself_at_the_pull_step_and_keeps_dockers_error() {
    let docker = "Docker stream error: failed to extract layer: failed to Lchown \"/var/lib/containerd/tmpmounts/m/usr/share/locale/zh_CN/LC_MESSAGES/shadow.mo\" for UID 0, GID 0: no such file or directory";
    let pull = json!({"type": "failed", "error": {
        "type": "machine", "action": "PullImage",
        "error": {
            "code": "internal",
            "message": "The Server's disk is full: 48 MB free under /var/lib/docker",
            "details": {"disk_full": {"docker_root": "/var/lib/docker", "free_bytes": 48_000_000}},
            "cause": [docker],
        }
    }});
    let RowState::Failed {
        reason,
        cause,
        step,
        ..
    } = failed_row(&[progress(&[("web", "alpha", pull)])])
    else {
        panic!("expected a failed row");
    };
    // The row is one Server's, so it names that Server.
    assert_eq!(
        reason,
        "Server alpha's disk is full: 48 MB free under /var/lib/docker"
    );
    assert_eq!(cause, vec![docker.to_owned()]);
    assert_eq!(step, Some(FailedAt::Pull));
}

/// Several Servers ran the deploy and one filled up: the reason says which.
#[test]
fn a_full_disk_names_the_server_that_ran_out() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    let prepared: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": [{
            "index": 0, "machine_id": "a".repeat(32), "machine_name": "machine-3",
            "service_name": "web", "operation": operation("web"), "status": {"type": "pending"}
        }],
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(prepared))
        .unwrap();
    let full = json!({
        "type": "machine", "action": "PullImage",
        "error": {
            "code": "internal",
            "message": "The Server's disk is full: 0 MB free under /var/lib/containerd",
            "details": {"disk_full": {"docker_root": "/var/lib/containerd", "free_bytes": 0}},
            "cause": ["Docker stream error: failed to copy: no space left on device"],
        }
    });
    let executed = RunEvidence::Executed {
        progress: Vec::new(),
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [],
            "failed": {"type": "operation", "operation": operation("web"), "error": full},
            "unexecuted": []
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, executed).unwrap();
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    let Some(ployz_store::Outcome::Executed { reason, .. }) = view.deployment.outcome else {
        panic!("an executed outcome");
    };
    assert_eq!(
        reason.as_deref(),
        Some("Server machine-3's disk is full: 0 MB free under /var/lib/containerd")
    );
}

#[test]
fn a_failed_row_records_the_step_it_failed_at() {
    let health = json!({"type": "failed", "error": {
        "type": "health", "container_id": "c".repeat(64), "failure": {"type": "cancelled"}
    }});
    let RowState::Failed { step, log, .. } = failed_row(&[progress(&[("web", "alpha", health)])])
    else {
        panic!("expected a failed row");
    };
    assert_eq!(step, Some(FailedAt::Health));
    assert_eq!(
        log,
        Some(LogText::Withheld {
            why: Withheld::Unanswered
        })
    );
    let inspect = json!({"type": "failed", "error": {
        "type": "machine", "action": "InspectContainer",
        "error": {"code": "internal", "message": "gone", "details": {}}
    }});
    let state = failed_row(&[
        progress(&[("web", "alpha", waiting(1_000))]),
        progress(&[("web", "alpha", inspect.clone())]),
    ]);
    assert!(
        matches!(
            state,
            RowState::Failed {
                step: Some(FailedAt::Health),
                ..
            }
        ),
        "an error without a step falls back to the last phase: {state:?}"
    );
    let state = failed_row(&[progress(&[("web", "alpha", inspect)])]);
    assert!(
        matches!(
            state,
            RowState::Failed {
                step: None,
                log: None,
                ..
            }
        ),
        "{state:?}"
    );
}

#[test]
fn a_raw_tail_stored_before_scrubbing_is_never_shown() {
    let stored = json!({
        "state": "failed", "reason": "Deploy failed", "cause": [],
        "log": ["DATABASE_URL=postgres://app:raw-secret@db/app"]
    });
    let state: RowState = serde_json::from_value(stored).unwrap();
    assert_eq!(
        state,
        RowState::Failed {
            reason: "Deploy failed".into(),
            cause: Vec::new(),
            step: None,
            log: Some(LogText::Withheld {
                why: Withheld::NoSecretContext
            }),
            unanswered: false,
            out_of_memory: None,
            stopped_by: StoppedBy::NOTHING,
        }
    );
    let reserialized = serde_json::to_string(&state).unwrap();
    assert!(!reserialized.contains("raw-secret"), "{reserialized}");
    let empty: RowState = serde_json::from_value(json!({
        "state": "failed", "reason": "Deploy failed", "cause": [], "log": []
    }))
    .unwrap();
    assert!(matches!(empty, RowState::Failed { log: None, .. }));
}

/// Cloud's evidence for a person who holds every permission but `environments:protect`.
fn unprotecting(approval: Approval) -> Trusted {
    Trusted {
        approval,
        permissions: Some(
            Permission::ALL
                .iter()
                .copied()
                .filter(|permission| *permission != Permission::EnvironmentsProtect)
                .collect(),
        ),
        ..Trusted::default()
    }
}

fn assert_admin_asked(refused: &RpcError) {
    assert_eq!(refused.code.as_str(), "approval_required");
    assert_eq!(
        refused.message,
        "production is Protected; an admin must approve this deploy."
    );
    assert_eq!(refused.details["protected"], json!(true));
}

#[test]
fn a_deploy_into_a_protected_environment_asks_an_admin_even_when_cloud_requires_nothing() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    let refused =
        deploy_trusted(&store, &who, 2, &unprotecting(Approval::NotRequired)).unwrap_err();
    assert_admin_asked(&refused);
    deploy_trusted(&store, &who, 2, &unprotecting(approved(&refused))).unwrap();

    set_replicas(&store, &who, "api", 3);
    let admin = Trusted {
        permissions: Some(Permission::ALL.iter().copied().collect()),
        ..Trusted::default()
    };
    deploy_trusted(&store, &who, 3, &admin).unwrap();
}

#[test]
fn a_retry_into_a_protected_environment_asks_an_admin() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    cancel(&store, &who, 2).unwrap();
    let retry = |approval| {
        store.write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(3),
                deployment: id(2),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &unprotecting(approval),
        )
    };
    let refused = retry(Approval::NotRequired).unwrap_err();
    assert_admin_asked(&refused);
    retry(approved(&refused)).unwrap();
}

/// Cloud's evidence for an owner's Deploy token: it can protect, but its grant leaves
/// out `environments:protect` and `changes:destroy`. Nothing runs that a prune removes.
fn protectors_deploy_token(approval: Approval) -> Trusted {
    Trusted {
        protector: true,
        running: Some(RunningObservation::default()),
        permissions: unprotecting(approval.clone()).permissions.map(|held| {
            held.into_iter()
                .filter(|p| *p != Permission::ChangesDestroy)
                .collect()
        }),
        ..unprotecting(approval)
    }
}

#[test]
fn a_protectors_deploy_token_ships_into_protected_production_unasked() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    deploy_trusted(
        &store,
        &who,
        2,
        &protectors_deploy_token(Approval::NotRequired),
    )
    .unwrap();
}

#[test]
fn a_protectors_agent_still_asks_an_admin_in_protected_production() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    let agent = Trusted {
        asks: Asks::EveryDeploy,
        ..protectors_deploy_token(Approval::NotRequired)
    };
    let refused = deploy_trusted(&store, &who, 2, &agent).unwrap_err();
    assert_admin_asked(&refused);
}

#[test]
fn a_protectors_deploy_token_cannot_ship_a_saved_removal_into_protected_production() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    remove_web(&store, &who);
    save(&store, &who);
    let refused = deploy_trusted(
        &store,
        &who,
        2,
        &protectors_deploy_token(Approval::NotRequired),
    )
    .unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
}

/// Retry `source` as `id` under `trusted`, confirming no rollback.
fn retry_as(
    store: &ConfigStore,
    who: &Actor,
    (n, source): (u8, u8),
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    store.write_trusted(
        who,
        &Admit::Retry(Retry {
            id: id(n),
            deployment: id(source),
            rolls_back: None,
            version: None,
            accept_volume_loss: Vec::new(),
        }),
        trusted,
    )
}

#[test]
fn a_protectors_deploy_token_retries_what_destroys_nothing_unasked() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    cancel(&store, &who, 2).unwrap();
    let retried = retry_as(
        &store,
        &who,
        (3, 2),
        &protectors_deploy_token(Approval::NotRequired),
    )
    .unwrap();
    assert_eq!(retried.status, DeploymentStatus::Queued);
}

#[test]
fn a_members_retry_that_destroys_nothing_still_asks_an_admin() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    cancel(&store, &who, 2).unwrap();
    let member = Trusted {
        running: nothing_else_runs(),
        ..unprotecting(Approval::NotRequired)
    };
    let refused = retry_as(&store, &who, (3, 2), &member).unwrap_err();
    assert_admin_asked(&refused);
}

/// Cloud's evidence for an admin's token that can protect and destroy, but whose
/// grant leaves out `environments:protect`.
fn admins_token(running: Option<RunningObservation>) -> Trusted {
    Trusted {
        protector: true,
        running,
        ..unprotecting(Approval::NotRequired)
    }
}

#[test]
fn a_protectors_retry_that_prunes_still_asks_an_admin() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    cancel(&store, &who, 2).unwrap();
    let refused = retry_as(
        &store,
        &who,
        (3, 2),
        &admins_token(running(&["web", "api", "extra2"])),
    )
    .unwrap_err();
    assert_admin_asked(&refused);
    let unseen = running(&["web", "api"]).map(|running| RunningObservation {
        unanswered: vec![MachineId::parse("e".repeat(32)).unwrap()],
        ..running
    });
    let refused = retry_as(&store, &who, (3, 2), &admins_token(unseen)).unwrap_err();
    assert_admin_asked(&refused);
}

#[test]
fn a_protectors_rollback_still_asks_an_admin_even_when_it_takes_nothing_off() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    apply(&store, 2);
    let refused = retry_as(&store, &who, (3, 1), &admins_token(nothing_else_runs())).unwrap_err();
    assert_admin_asked(&refused);
    assert_eq!(refused.details["effects"], json!([]));
    assert_eq!(refused.details["redeploy"]["rolls_back"], json!([2]));
}

#[test]
fn a_protectors_rollback_that_takes_a_service_off_still_asks_an_admin() {
    let (store, who) = cron_added();
    let refused = retry_as(&store, &who, (3, 1), &admins_token(nothing_else_runs())).unwrap_err();
    assert_admin_asked(&refused);
    assert_eq!(refused.details["effects"][0]["name"], json!("cron"));
}

#[test]
fn a_protectors_retry_whose_revision_removes_a_domain_still_asks_an_admin() {
    let (store, who) = shop();
    add_web_domain(&store, &who, "app.example.com");
    apply_both(&store, &who, 1);
    remove_domain(&store, &who, "app.example.com");
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    cancel(&store, &who, 2).unwrap();
    let refused = retry_as(&store, &who, (3, 2), &admins_token(nothing_else_runs())).unwrap_err();
    assert_admin_asked(&refused);
    assert_eq!(
        effects(&refused),
        [("removes_domain".to_owned(), "app.example.com".to_owned())]
    );
}

/// Add domain `hostname` to `web`, as staged.
fn add_web_domain(store: &ConfigStore, who: &Actor, hostname: &str) {
    store
        .write(
            who,
            &ployz_store::AddDomain {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                hostname: Some(ployz_store::Hostname::parse(hostname).unwrap()),
                prefix: None,
                port: None,
            },
        )
        .unwrap();
}

fn remove_domain(store: &ConfigStore, who: &Actor, hostname: &str) {
    store
        .write(
            who,
            &ployz_store::RemoveDomain {
                environment: EnvironmentRef::default(),
                domain: hostname.into(),
            },
        )
        .unwrap();
}

#[test]
fn a_protectors_retry_removing_a_domain_an_unknown_deployment_may_have_left_still_asks_an_admin() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    add_web_domain(&store, &who, "app.example.com");
    // #2 lost track after preparing: it may have put the domain in place.
    lose_track(&store, &who, 2);
    remove_domain(&store, &who, "app.example.com");
    deploy_as(&store, &who, 3, Approval::NotRequired).unwrap();
    cancel(&store, &who, 3).unwrap();
    let refused = retry_as(&store, &who, (4, 3), &admins_token(nothing_else_runs())).unwrap_err();
    assert_admin_asked(&refused);
}

/// Admit Deployment `n` of every Service, then lose track of it after preparing.
fn lose_track(store: &ConfigStore, who: &Actor, n: u8) {
    deploy_as(store, who, n, Approval::NotRequired).unwrap();
    let a = runner("runner-a");
    store.claim(&id(n), &a).unwrap();
    store
        .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store.record(&id(n), &a, RunEvidence::Abandoned).unwrap();
}

#[test]
fn a_protectors_retry_weighs_every_unknown_deployment_of_a_service() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    add_web_domain(&store, &who, "a.example.com");
    lose_track(&store, &who, 2);
    add_web_domain(&store, &who, "b.example.com");
    lose_track(&store, &who, 3);
    remove_domain(&store, &who, "b.example.com");
    deploy_as(&store, &who, 4, Approval::NotRequired).unwrap();
    cancel(&store, &who, 4).unwrap();
    let refused = retry_as(&store, &who, (5, 4), &admins_token(nothing_else_runs())).unwrap_err();
    assert_admin_asked(&refused);
}

#[test]
fn a_protectors_retry_replaying_a_teardown_still_asks_an_admin() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    remove_web(&store, &who);
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    cancel(&store, &who, 2).unwrap();
    let refused = retry_as(&store, &who, (3, 2), &admins_token(nothing_else_runs())).unwrap_err();
    assert_admin_asked(&refused);
    assert_eq!(effects(&refused), removes(&["web"]));
}

#[test]
fn a_protectors_deploy_token_redeploying_a_teardown_asks_an_admin_naming_what_it_removes() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    remove_web(&store, &who);
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    cancel(&store, &who, 2).unwrap();
    // Its source's review let it remove web, so it asks rather than refuses, and
    // its card names what the admin lets it remove.
    let refused = retry_as(
        &store,
        &who,
        (3, 2),
        &protectors_deploy_token(Approval::NotRequired),
    )
    .unwrap_err();
    assert_admin_asked(&refused);
    assert_eq!(effects(&refused), removes(&["web"]));
    assert_eq!(
        refused.details["redeploy"],
        json!({ "deployment": 2, "rolls_back": [] })
    );
    let retried = retry_as(
        &store,
        &who,
        (3, 2),
        &protectors_deploy_token(approved(&refused)),
    )
    .unwrap();
    assert_eq!(retried.status, DeploymentStatus::Queued);
}

#[test]
fn a_protectors_deploy_token_redeploys_an_applied_teardown_unasked() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    remove_web(&store, &who);
    let trusted = Trusted {
        approval: Approval::NotRequired,
        running: nothing_else_runs(),
        ..Trusted::default()
    };
    // No prune, so only its teardown of web can remove anything.
    deploy_choosing(&store, &who, 2, &trusted, |deploy| deploy.no_prune = true).unwrap();
    let a = runner("runner-a");
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let applied = store
        .record(&id(2), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(applied.status, DeploymentStatus::Applied);
    let token = |running| Trusted {
        running,
        ..protectors_deploy_token(Approval::NotRequired)
    };
    // A copy of web on a Server #2 couldn't reach still runs: replaying the
    // teardown removes it, so it asks.
    let refused = retry_as(&store, &who, (3, 2), &token(running(&["api", "web"]))).unwrap_err();
    assert_admin_asked(&refused);
    assert_eq!(effects(&refused), removes(&["web"]));
    // So it does while a Server that should answer didn't.
    let unanswered = Some(RunningObservation {
        unanswered: vec![MachineId::parse("b".repeat(32)).unwrap()],
        ..RunningObservation::default()
    });
    assert_admin_asked(&retry_as(&store, &who, (3, 2), &token(unanswered)).unwrap_err());
    // #2 took web off and every Server answered without it: replaying its teardown
    // destroys nothing.
    let retried = retry_as(&store, &who, (3, 2), &token(running(&["api"]))).unwrap();
    assert_eq!(retried.status, DeploymentStatus::Queued);
}

#[test]
fn a_deploy_tokens_redeploy_refused_for_an_unseen_prune_names_a_way_out_redeploy_has() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    set_replicas(&store, &who, "api", 2);
    deploy_as(&store, &who, 2, Approval::NotRequired).unwrap();
    cancel(&store, &who, 2).unwrap();
    let token = |running| Trusted {
        running,
        ..deploy_token()
    };
    let unseen = retry_as(&store, &who, (3, 2), &token(None)).unwrap_err();
    assert_eq!(unseen.code.as_str(), "forbidden", "{unseen:?}");
    assert!(!unseen.message.contains("--no-prune"), "{}", unseen.message);
    assert!(
        unseen.message.ends_with(
            "Redeploy #2 once every server answers, or have an owner or admin redeploy it."
        ),
        "{}",
        unseen.message
    );
    assert_eq!(unseen.details.get("next"), None);
    let named = retry_as(
        &store,
        &who,
        (3, 2),
        &token(running(&["web", "api", "extra2"])),
    )
    .unwrap_err();
    assert!(
        named.message.contains("this redeploy removes extra2"),
        "{}",
        named.message
    );
    assert!(
        named
            .message
            .ends_with("Have an owner or admin redeploy #2."),
        "{}",
        named.message
    );
}

/// History's (live, failed) marks.
fn marks(store: &ConfigStore, who: &Actor) -> (Option<Revision>, Option<Revision>) {
    let view = store
        .read(who, &ployz_store::HistoryQuery::default())
        .unwrap();
    (view.live, view.failed)
}

fn save(store: &ConfigStore, who: &Actor) -> Revision {
    store
        .write_trusted(
            who,
            &ployz_store::Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &Trusted::default(),
        )
        .unwrap()
        .saved
        .unwrap()
}

#[test]
fn history_marks_the_last_applied_revision_live_and_a_newest_one_that_failed() {
    let (store, who) = shop();
    let first = save(&store, &who);
    assert_eq!(marks(&store, &who), (None, None), "saved, never deployed");

    apply_both(&store, &who, 1);
    assert_eq!(marks(&store, &who), (Some(first), None));

    set_replicas(&store, &who, "web", 3);
    let second = admit(&store, &who, 2, &[], None).unwrap().saved;
    fail_api(&store, 2);
    assert_eq!(
        marks(&store, &who),
        (Some(first), Some(second)),
        "the Servers still run the version before the failed one"
    );

    retry(&store, &who, 3, 2).unwrap();
    assert_eq!(
        marks(&store, &who),
        (Some(first), None),
        "a later run of the failed version clears Failed"
    );
    let a = runner("runner-a");
    store.claim(&id(3), &a).unwrap();
    store
        .record(&id(3), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(3), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(marks(&store, &who), (Some(second), None));

    set_replicas(&store, &who, "web", 4);
    let third = save(&store, &who);
    assert_ne!(third, second);
    assert_eq!(
        marks(&store, &who),
        (Some(second), None),
        "a newer version not deployed yet has no mark"
    );
}

#[test]
fn a_superseded_retry_of_the_failed_version_clears_failed() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    fail_api(&store, 1);
    set_replicas(&store, &who, "web", 3);
    let second = admit(&store, &who, 2, &[], None).unwrap().saved;
    fail_api(&store, 2);
    assert_eq!(marks(&store, &who).1, Some(second));

    retry(&store, &who, 3, 2).unwrap();
    retry(&store, &who, 4, 1).unwrap();
    assert_eq!(status(&store, &who, 3), DeploymentStatus::Superseded);
    assert_eq!(
        marks(&store, &who).1,
        None,
        "the newest version's latest attempt was superseded, not failed"
    );
}

#[test]
fn verifier_partial_retry_keeps_successfully_applied_service_upload() {
    let (store, who) = shop();
    for name in ["web", "api"] {
        store
            .write(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Unset {
                        path: SettingPath::parse(&format!("{name}.image")).unwrap(),
                    }],
                },
            )
            .unwrap();
    }
    let a = runner("cli-a");
    for (n, digest) in [(1, 'a'), (2, 'b')] {
        deploy_sent(&store, &who, n, Some(digest));
        store.claim(&id(n), &a).unwrap();
        store
            .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
            .unwrap();
        store
            .record(&id(n), &a, succeeded(&["web", "api"]))
            .unwrap();
    }
    retry(&store, &who, 3, 1).unwrap();
    store.claim(&id(3), &a).unwrap();
    store
        .record(&id(3), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(
            &id(3),
            &a,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome(json!({
                    "type": "failed",
                    "completed": [operation("web")],
                    "failed": {"type": "operation", "operation": operation("api"), "error": {
                        "type": "machine", "action": "RemoveContainer",
                        "error": {"code": "internal", "message": "disk failure", "details": {}}
                    }},
                    "unexecuted": []
                }))),
                removed: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(status(&store, &who, 3), DeploymentStatus::Failed);
    assert!(nodes(&store, &who, 3).contains(&("web".to_owned(), NodeStatus::Deployed)));
    assert_eq!(
        kept_of(&store, &who, &[1, 2, 3]),
        [false, true, true],
        "web runs a, so its newest copy stays"
    );
    admit(&store, &who, 4, &["web"], None).unwrap();
    assert_eq!(
        store.claim(&id(4), &a).unwrap().uploads,
        std::collections::BTreeMap::from([(
            ServiceName::parse("web").unwrap(),
            Some(digested('a').digest)
        )])
    );
}

#[test]
fn verifier_superseding_a_queued_retry_keeps_latest_sent_upload() {
    let (store, who) = uploaded_app();
    deploy_sent(&store, &who, 1, Some('a'));
    fail(&store, 1);
    deploy_sent(&store, &who, 2, Some('b'));
    fail(&store, 2);
    deploy_sent(&store, &who, 3, Some('a'));
    fail(&store, 3);
    assert_eq!(kept_of(&store, &who, &[2, 3]), [false, true]);
    retry(&store, &who, 4, 2).unwrap();
    let next = deploy_sent(&store, &who, 5, None);
    assert_eq!(status(&store, &who, 4), DeploymentStatus::Superseded);
    assert_eq!(next.upload, Some(digested('a')));
}

#[test]
fn a_deploy_admitted_behind_a_running_retry_builds_from_the_retried_upload() {
    let (store, who) = shop();
    for name in ["web", "api"] {
        store
            .write(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Unset {
                        path: SettingPath::parse(&format!("{name}.image")).unwrap(),
                    }],
                },
            )
            .unwrap();
    }
    let a = runner("cli-a");
    for (n, digest) in [(1, 'a'), (2, 'b')] {
        deploy_sent(&store, &who, n, Some(digest));
        store.claim(&id(n), &a).unwrap();
        store
            .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
            .unwrap();
        store
            .record(&id(n), &a, succeeded(&["web", "api"]))
            .unwrap();
    }
    retry(&store, &who, 3, 1).unwrap();
    store.claim(&id(3), &a).unwrap();
    admit(&store, &who, 4, &["web"], None).unwrap();
    assert_eq!(status(&store, &who, 3), DeploymentStatus::Running);
    assert_eq!(
        kept_of(&store, &who, &[1, 2, 3]),
        [false, true, true],
        "web runs b until the retry lands"
    );
    store
        .record(&id(3), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(3), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(
        store.claim(&id(4), &a).unwrap().uploads,
        std::collections::BTreeMap::from([(
            ServiceName::parse("web").unwrap(),
            Some(digested('a').digest)
        )])
    );
}

/// `web` and `api` without a source, each deployed from `a` then from `b`.
fn uploaded_twice(store: &ConfigStore, who: &Actor) {
    for name in ["web", "api"] {
        store
            .write(
                who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Unset {
                        path: SettingPath::parse(&format!("{name}.image")).unwrap(),
                    }],
                },
            )
            .unwrap();
    }
    let a = runner("cli-a");
    for (n, digest) in [(1, 'a'), (2, 'b')] {
        deploy_sent(store, who, n, Some(digest));
        store.claim(&id(n), &a).unwrap();
        store
            .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
            .unwrap();
        store
            .record(&id(n), &a, succeeded(&["web", "api"]))
            .unwrap();
    }
}

/// Deployment `n`, claimed, applies `completed` and fails on `failed`.
fn partly(store: &ConfigStore, n: u8, completed: &str, failed: &str) {
    let a = runner("cli-a");
    store
        .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(
            &id(n),
            &a,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome(json!({
                    "type": "failed",
                    "completed": [operation(completed)],
                    "failed": {"type": "operation", "operation": operation(failed), "error": {
                        "type": "machine", "action": "RemoveContainer",
                        "error": {"code": "internal", "message": "disk failure", "details": {}}
                    }},
                    "unexecuted": []
                }))),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

#[test]
fn verifier_a_running_retry_that_fails_a_service_leaves_its_queued_deploy_on_its_confirmed_upload()
{
    let (store, who) = shop();
    uploaded_twice(&store, &who);
    let a = runner("cli-a");
    retry(&store, &who, 3, 1).unwrap();
    store.claim(&id(3), &a).unwrap();
    admit(&store, &who, 4, &["web"], None).unwrap();
    partly(&store, 3, "api", "web");
    assert!(nodes(&store, &who, 3).contains(&("web".to_owned(), NodeStatus::Failed)));
    let claimed = store.claim(&id(4), &a).unwrap();
    assert_eq!(
        claimed.uploads,
        std::collections::BTreeMap::from([(
            ServiceName::parse("web").unwrap(),
            Some(digested('b').digest)
        )])
    );
    assert_eq!(claimed.deployment.upload, Some(digested('b')));
    assert_eq!(
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(4) })
            .unwrap()
            .deployment
            .upload,
        Some(digested('b')),
        "the Deployment shows the upload it runs"
    );
}

#[test]
fn a_running_retry_that_applies_a_service_moves_its_queued_deploy_onto_the_retried_upload() {
    let (store, who) = shop();
    uploaded_twice(&store, &who);
    let a = runner("cli-a");
    retry(&store, &who, 3, 1).unwrap();
    store.claim(&id(3), &a).unwrap();
    admit(&store, &who, 4, &["web"], None).unwrap();
    partly(&store, 3, "web", "api");
    let claimed = store.claim(&id(4), &a).unwrap();
    assert_eq!(
        claimed.uploads,
        std::collections::BTreeMap::from([(
            ServiceName::parse("web").unwrap(),
            Some(digested('a').digest)
        )])
    );
    assert_eq!(claimed.deployment.upload, Some(digested('a')));
}

#[test]
fn a_cancelled_queued_deploy_no_longer_chooses_its_upload_as_it_starts() {
    let (store, who) = shop();
    uploaded_twice(&store, &who);
    retry(&store, &who, 3, 1).unwrap();
    store.claim(&id(3), &runner("cli-a")).unwrap();
    let queued = admit(&store, &who, 4, &["web"], None).unwrap();
    assert!(queued.chooses_upload_at_start);
    let cancelled = cancel(&store, &who, 4).unwrap();
    assert!(
        !cancelled.chooses_upload_at_start,
        "a Deployment that will never start chooses nothing as it starts"
    );
    assert_eq!(
        cancelled,
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(4) })
            .unwrap()
            .deployment,
        "the cancel answers what a later read shows"
    );
}

#[test]
fn a_claimed_deployment_reads_preparing_until_its_runner_records_a_deploy_preview() {
    let (store, who) = shop();
    let queued = admit(&store, &who, 1, &[], None).unwrap();
    assert!(!queued.preparing, "a queued Deployment has not started");
    let a = runner("runner-a");
    let read = |store: &ConfigStore| {
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
            .unwrap()
            .deployment
    };
    let claimed = store.claim(&id(1), &a).unwrap();
    assert!(claimed.deployment.preparing);
    assert!(
        read(&store).preparing,
        "a read shows what the claim answered"
    );
    assert!(
        store.claim(&id(1), &a).unwrap().deployment.preparing,
        "renewing the claim before the preview still prepares"
    );
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let running = read(&store);
    assert_eq!(running.status, DeploymentStatus::Running);
    assert!(
        !running.preparing,
        "a recorded Deploy Preview ends preparation"
    );
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert!(!read(&store).preparing);
}

#[test]
fn cloud_keeps_every_upload_a_queued_deploy_may_start_from_until_it_ends() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.image").unwrap(),
                }],
            },
        )
        .unwrap();
    let a = runner("cli-a");
    let only_web = |n: u8, upload: Option<char>| {
        store
            .write_trusted(
                &who,
                &Admit::Deploy(Deploy {
                    id: id(n),
                    environment: EnvironmentRef::default(),
                    services: vec![ServiceName::parse("web").unwrap()],
                    version: None,
                    upload: upload.map(digested),
                    accept_volume_loss: Vec::new(),
                    message: None,
                    prune: false,
                    no_prune: false,
                }),
                &Trusted::default(),
            )
            .unwrap()
    };
    for (n, digest) in [(1, 'a'), (2, 'b')] {
        only_web(n, Some(digest));
        store.claim(&id(n), &a).unwrap();
        store
            .record(&id(n), &a, RunEvidence::Prepared(preview(&["web"])))
            .unwrap();
        store.record(&id(n), &a, succeeded(&["web"])).unwrap();
    }
    retry(&store, &who, 3, 1).unwrap();
    store.claim(&id(3), &a).unwrap();
    only_web(4, None);
    // web takes an image while #4 waits, so no Service without a source keeps b.
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("web.image").unwrap(),
                    value: json!("nginx:1"),
                }],
            },
        )
        .unwrap();
    save(&store, &who);
    store
        .record(&id(3), &a, RunEvidence::NotExecuted("stopped".into()))
        .unwrap();
    assert_eq!(
        kept_of(&store, &who, &[1, 2]),
        [true, true],
        "#4 starts from b now, and a while it may still change"
    );
    assert_eq!(
        store.claim(&id(4), &a).unwrap().deployment.upload,
        Some(digested('b'))
    );
    store
        .record(&id(4), &a, RunEvidence::NotExecuted("stopped".into()))
        .unwrap();
    assert_eq!(
        kept_of(&store, &who, &[1, 2]),
        [false, false],
        "nothing builds from either once #4 ended"
    );
}

#[test]
fn verifier_a_failed_upload_keeps_the_upload_a_partial_retry_applied() {
    let (store, who) = shop();
    uploaded_twice(&store, &who);
    retry(&store, &who, 3, 1).unwrap();
    store.claim(&id(3), &runner("cli-a")).unwrap();
    partly(&store, 3, "web", "api");
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id(4),
                environment: EnvironmentRef::default(),
                services: vec![ServiceName::parse("web").unwrap()],
                version: None,
                upload: Some(digested('c')),
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &Trusted::default(),
        )
        .unwrap();
    fail(&store, 4);
    assert_eq!(
        kept_of(&store, &who, &[1, 2, 4]),
        [true, true, true],
        "web still runs a, api b, and c is web's latest sent"
    );
}

#[test]
fn verifier_a_successful_no_op_retry_keeps_the_service_on_the_retried_upload() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.image").unwrap(),
                }],
            },
        )
        .unwrap();
    let a = runner("cli-a");
    let only_web = |n: u8, upload: Option<char>| {
        store
            .write_trusted(
                &who,
                &Admit::Deploy(Deploy {
                    id: id(n),
                    environment: EnvironmentRef::default(),
                    services: vec![ServiceName::parse("web").unwrap()],
                    version: None,
                    upload: upload.map(digested),
                    accept_volume_loss: Vec::new(),
                    message: None,
                    prune: false,
                    no_prune: false,
                }),
                &Trusted::default(),
            )
            .unwrap();
    };
    only_web(1, Some('b'));
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    store.record(&id(1), &a, succeeded(&["web"])).unwrap();
    only_web(2, Some('c'));
    fail(&store, 2);
    retry(&store, &who, 3, 1).unwrap();
    store.claim(&id(3), &a).unwrap();
    store
        .record(&id(3), &a, RunEvidence::Prepared(preview(&[])))
        .unwrap();
    store.record(&id(3), &a, succeeded(&[])).unwrap();
    assert_eq!(status(&store, &who, 3), DeploymentStatus::Applied);
    assert!(nodes(&store, &who, 3).contains(&("web".to_owned(), NodeStatus::Unchanged)));
    only_web(4, None);
    assert_eq!(
        store.claim(&id(4), &a).unwrap().uploads,
        std::collections::BTreeMap::from([(
            ServiceName::parse("web").unwrap(),
            Some(digested('b').digest)
        )])
    );
}

#[test]
fn verifier_failed_retry_of_a_pre_0022_upload_does_not_replace_current_upload() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("app").unwrap(),
                image: None,
                template: None,
            },
        )
        .unwrap();
    deploy_sent(&store, &who, 1, Some('b'));
    fail(&store, 1);
    backend::execute(&url, "UPDATE config_deployment SET service_uploads = NULL");
    deploy_sent(&store, &who, 2, Some('a'));
    fail(&store, 2);
    assert_eq!(kept_of(&store, &who, &[1, 2]), [false, true]);
    retry(&store, &who, 3, 1).unwrap();
    fail(&store, 3);
    assert_eq!(
        deploy_sent(&store, &who, 4, None).upload,
        Some(digested('a'))
    );
}

/// Cloud's evidence for a caller who holds every permission, as an owner's or
/// admin's sign-in or token does, asking as `asks` does: `EveryDeploy` for an
/// agent's token, such as `ployz mcp`'s.
fn protector(approval: Approval, asks: Asks) -> Trusted {
    Trusted {
        approval,
        asks,
        permissions: Some(Permission::ALL.iter().copied().collect()),
        running: nothing_else_runs(),
        ..Trusted::default()
    }
}

/// [`unprotecting`], a member's sign-in or token, asking as `asks` does.
fn member(approval: Approval, asks: Asks) -> Trusted {
    Trusted {
        asks,
        running: nothing_else_runs(),
        ..unprotecting(approval)
    }
}

/// `trusted` for the user Cloud authenticated as `user`, by their immutable id.
fn signed_in(user: &str, trusted: Trusted) -> Trusted {
    Trusted {
        user: Some(user.to_owned()),
        ..trusted
    }
}

fn as_person(who: &Actor, name: &str) -> Actor {
    Actor {
        principal: Some(Principal::parse(name).unwrap()),
        ..who.clone()
    }
}

/// [`shop`] with #1 running `web` and `api`, then #2 adding `cron`.
fn cron_added() -> (ConfigStore, Actor) {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("cron").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    apply(&store, 2);
    (store, who)
}

#[test]
fn a_rollback_that_takes_a_service_off_asks_and_names_it() {
    let (store, who) = cron_added();
    let rollback = |trusted: &Trusted| {
        store.write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(3),
                deployment: id(1),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            trusted,
        )
    };
    for (trusted, protected) in [
        (protector(Approval::Required, Asks::Destroys), false),
        (protector(Approval::Required, Asks::EveryDeploy), false),
        (member(Approval::Required, Asks::Destroys), true),
        (member(Approval::Required, Asks::EveryDeploy), true),
    ] {
        let refused = rollback(&trusted).unwrap_err();
        assert_eq!(refused.code.as_str(), "approval_required", "{trusted:?}");
        assert_eq!(refused.details.get("protected").is_some(), protected);
        let removes: Vec<&str> = refused.details["effects"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|effect| effect["kind"] == "removes_service")
            .map(|effect| effect["name"].as_str().unwrap())
            .collect();
        assert_eq!(removes, ["cron"], "{trusted:?}");
    }
    let owner = protector(Approval::Required, Asks::Destroys);
    let refused = rollback(&owner).unwrap_err();
    let rolled = rollback(&Trusted {
        approval: approved(&refused),
        ..owner
    })
    .unwrap();
    assert_eq!(rolled.status, DeploymentStatus::Queued);
}

#[test]
fn a_retry_that_takes_nothing_off_still_asks_nothing() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    fail_api(&store, 1);
    let retried = store
        .write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(2),
                deployment: id(1),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &protector(Approval::Required, Asks::Destroys),
        )
        .unwrap();
    assert_eq!(retried.status, DeploymentStatus::Queued);
}

#[test]
fn cancelling_someone_elses_deployment_in_protected_production_asks_an_admin() {
    let (store, system) = shop();
    let (grace, linus) = (as_person(&system, "grace"), as_person(&system, "linus"));
    let cancel_as = |who: &Actor, n: u8, trusted: &Trusted| {
        store.write_trusted(who, &Cancel { deployment: id(n) }, trusted)
    };
    deploy_trusted(
        &store,
        &grace,
        1,
        &protector(Approval::NotRequired, Asks::Destroys),
    )
    .unwrap();

    for asks in [Asks::Destroys, Asks::EveryDeploy] {
        let refused = cancel_as(&linus, 1, &member(Approval::Required, asks)).unwrap_err();
        assert_eq!(refused.code.as_str(), "approval_required");
        assert_eq!(
            refused.message,
            "production is Protected; an admin must approve this cancel."
        );
        assert_eq!(refused.details["protected"], json!(true));
        assert_eq!(status(&store, &system, 1), DeploymentStatus::Queued);
    }
    let refused = cancel_as(&linus, 1, &member(Approval::NotRequired, Asks::Destroys)).unwrap_err();
    let cancelled = cancel_as(&linus, 1, &member(approved(&refused), Asks::Destroys)).unwrap();
    assert_eq!(cancelled.status, DeploymentStatus::Cancelled);

    // Grace may cancel anyone's, and Linus his own once it runs as his.
    deploy_trusted(
        &store,
        &linus,
        2,
        &signed_in("u-linus", protector(Approval::NotRequired, Asks::Destroys)),
    )
    .unwrap();
    cancel_as(
        &linus,
        2,
        &signed_in("u-linus", member(Approval::NotRequired, Asks::Destroys)),
    )
    .unwrap();
    deploy_trusted(
        &store,
        &linus,
        3,
        &protector(Approval::NotRequired, Asks::Destroys),
    )
    .unwrap();
    cancel_as(&grace, 3, &protector(Approval::Required, Asks::Destroys)).unwrap();

    // Cancelling one already stopping changes nothing, so nobody is asked.
    deploy_trusted(
        &store,
        &grace,
        4,
        &protector(Approval::NotRequired, Asks::Destroys),
    )
    .unwrap();
    store.claim(&id(4), &runner("runner-a")).unwrap();
    let stopping = cancel_as(&grace, 4, &protector(Approval::Required, Asks::Destroys)).unwrap();
    assert_eq!(stopping.status, DeploymentStatus::Cancelling);
    let again = cancel_as(&linus, 4, &member(Approval::Required, Asks::Destroys)).unwrap();
    assert_eq!(again.status, DeploymentStatus::Cancelling);
}

#[test]
fn a_member_named_like_the_admitter_still_asks_before_cancelling_their_deployment() {
    let (store, system) = shop();
    let linus = as_person(&system, "linus");
    deploy_trusted(
        &store,
        &linus,
        1,
        &signed_in("u-linus", protector(Approval::NotRequired, Asks::Destroys)),
    )
    .unwrap();

    // Another member who calls themselves linus too.
    let namesake = signed_in("u-other", member(Approval::Required, Asks::Destroys));
    let refused = store
        .write_trusted(&linus, &Cancel { deployment: id(1) }, &namesake)
        .unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    assert_eq!(refused.details["protected"], json!(true));
    assert_eq!(status(&store, &system, 1), DeploymentStatus::Queued);
}

#[test]
fn a_deployment_admitted_without_a_user_id_is_someones_own_by_name() {
    // As one an older Cloud admitted, or one from before the id: its row holds none.
    let (store, system) = shop();
    let (grace, linus) = (as_person(&system, "grace"), as_person(&system, "linus"));
    deploy_trusted(
        &store,
        &linus,
        1,
        &protector(Approval::NotRequired, Asks::Destroys),
    )
    .unwrap();

    // Linus cancels his own by name, though Cloud sends his id now.
    let mine = signed_in("u-linus", member(Approval::Required, Asks::Destroys));
    store
        .write_trusted(&linus, &Cancel { deployment: id(1) }, &mine)
        .unwrap();
    // Grace's is someone else's to him, with his id sent or not.
    deploy_trusted(
        &store,
        &grace,
        2,
        &protector(Approval::NotRequired, Asks::Destroys),
    )
    .unwrap();
    let refused = store
        .write_trusted(&linus, &Cancel { deployment: id(2) }, &mine)
        .unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    let refused = store
        .write_trusted(
            &linus,
            &Cancel { deployment: id(2) },
            &member(Approval::Required, Asks::Destroys),
        )
        .unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
}

#[test]
fn a_rollback_asks_about_a_service_a_deployment_of_unknown_outcome_may_have_started() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("cron").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(2), &a).unwrap();
    store
        .record(
            &id(2),
            &a,
            RunEvidence::Prepared(preview(&["web", "api", "cron"])),
        )
        .unwrap();
    // Its runner loses track of it: cron may run, though nothing confirmed it.
    store.claim(&id(2), &a).unwrap_err();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Unknown);

    let refused = store
        .write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(3),
                deployment: id(1),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &protector(Approval::Required, Asks::Destroys),
        )
        .unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    let removes: Vec<&str> = refused.details["effects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|effect| effect["kind"] == "removes_service")
        .map(|effect| effect["name"].as_str().unwrap())
        .collect();
    assert_eq!(removes, ["cron"]);
}

/// [`shop`] with #1 applying web and api, then #2 adding cron and starting on
/// the runner returned.
fn cron_started() -> (ConfigStore, Actor, RunnerId) {
    cron_started_in(backend::open())
}

fn cron_started_in(store: ConfigStore) -> (ConfigStore, Actor, RunnerId) {
    let (store, who) = shop_in(store);
    apply_both(&store, &who, 1);
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("cron").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(2), &a).unwrap();
    store
        .record(
            &id(2),
            &a,
            RunEvidence::Prepared(preview(&["web", "api", "cron"])),
        )
        .unwrap();
    (store, who, a)
}

/// [`cron_started`], then #2's runner lost track of it.
fn cron_lost() -> (ConfigStore, Actor) {
    let (store, who, a) = cron_started();
    store.claim(&id(2), &a).unwrap_err();
    (store, who)
}

/// What executing [`preview`] did when `done` completed, `failed` stopped it and
/// `unexecuted` never ran.
fn failed_at(done: &[&str], failed: &str, unexecuted: &[&str]) -> RunEvidence {
    RunEvidence::Executed {
        progress: Vec::new(),
        outcome: Box::new(outcome(json!({
            "type": "failed",
            "completed": done.iter().map(|service| operation(service)).collect::<Vec<_>>(),
            "failed": {"type": "operation", "operation": operation(failed), "error": {
                "type": "machine", "action": "CreateContainer",
                "error": {"code": "unavailable", "message": "connection lost", "details": {}}
            }},
            "unexecuted": unexecuted.iter().map(|service| operation(service)).collect::<Vec<_>>()
        }))),
        removed: Vec::new(),
    }
}

fn removes_service(refused: &RpcError) -> Vec<&str> {
    refused.details["effects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|effect| effect["kind"] == "removes_service")
        .map(|effect| effect["name"].as_str().unwrap())
        .collect()
}

fn rollback_to(
    store: &ConfigStore,
    who: &Actor,
    (n, to): (u8, u8),
) -> Result<DeploymentSummary, RpcError> {
    store.write_trusted(
        who,
        &Admit::Retry(Retry {
            id: id(n),
            deployment: id(to),
            rolls_back: None,
            version: None,
            accept_volume_loss: Vec::new(),
        }),
        &protector(Approval::Required, Asks::Destroys),
    )
}

#[test]
fn a_deploy_of_some_services_leaves_a_lost_deployments_service_in_doubt() {
    let (store, who) = cron_lost();
    // #3 deploys web alone: it settles nothing about cron.
    admit(&store, &who, 3, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(3), &a).unwrap();
    store
        .record(&id(3), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    store.record(&id(3), &a, succeeded(&["web"])).unwrap();
    assert_eq!(status(&store, &who, 3), DeploymentStatus::Applied);

    let refused = rollback_to(&store, &who, (4, 1)).unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    let removes: Vec<&str> = refused.details["effects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|effect| effect["kind"] == "removes_service")
        .map(|effect| effect["name"].as_str().unwrap())
        .collect();
    assert_eq!(removes, ["cron"]);
}

#[test]
fn rolling_back_to_a_deploy_of_some_services_leaves_the_others_running_unasked() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    admit(&store, &who, 2, &["web"], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    store.record(&id(2), &a, succeeded(&["web"])).unwrap();
    apply_both(&store, &who, 3);

    // Back to #2 ships web alone; api keeps running.
    let rolled = rollback_to(&store, &who, (4, 2)).unwrap();
    assert_eq!(rolled.status, DeploymentStatus::Queued);
}

#[test]
fn a_deploy_queued_behind_one_that_is_later_lost_asks_about_its_services_when_admitted() {
    // A plain retry asks only what its source's review asked, so that review has
    // to have counted a Deployment that was running then and is lost by the retry.
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("cron").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(2), &a).unwrap();
    store
        .write(
            &who,
            &RemoveService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("cron").unwrap(),
            },
        )
        .unwrap();

    let refused = deploy_trusted(
        &store,
        &who,
        3,
        &protector(Approval::Required, Asks::Destroys),
    )
    .unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    let removes: Vec<&str> = refused.details["effects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|effect| effect["kind"] == "removes_service")
        .map(|effect| effect["name"].as_str().unwrap())
        .collect();
    assert_eq!(removes, ["cron"]);
}

#[test]
fn a_rollback_asks_about_a_service_a_failed_deployment_may_have_left_running() {
    let (store, who, a) = cron_started();
    // Creating cron's Container didn't finish, so Applied State lacks it; its
    // Container may still run.
    store
        .record(&id(2), &a, failed_at(&["web", "api"], "cron", &[]))
        .unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Failed);
    assert!(nodes(&store, &who, 2).contains(&("cron".to_owned(), NodeStatus::Failed)));

    let refused = rollback_to(&store, &who, (3, 1)).unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    assert_eq!(removes_service(&refused), ["cron"]);
}

#[test]
fn a_rollback_asks_about_a_service_whose_outcome_a_failed_deployment_lost() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who, a) = cron_started_in(ConfigStore::open(&url, backend::key()).unwrap());
    store
        .record(&id(2), &a, failed_at(&["web"], "api", &["cron"]))
        .unwrap();
    // Its runner was lost before recording what became of cron.
    rewrite_run(&url, 2, |run| {
        run["nodes"]["00000000-0000-4000-8000-000000000005"] = json!("unknown");
    });
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Failed);
    assert!(nodes(&store, &who, 2).contains(&("cron".to_owned(), NodeStatus::Unknown)));

    let refused = rollback_to(&store, &who, (3, 1)).unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    assert_eq!(removes_service(&refused), ["cron"]);
}

#[test]
fn a_rollback_asks_about_a_service_a_cancelled_deployment_started_but_reported_unexecuted() {
    let (store, who, a) = cron_started();
    // api's and cron's Containers wait for health side by side when #2 is cancelled
    // and the Machine stops answering, so neither is cleaned up.
    let cancelled = json!({"type": "failed", "error": {"type": "cancelled"}});
    let mut tracker = RowTracker::default();
    store
        .record(
            &id(2),
            &a,
            RunEvidence::Progress(tracker.changes(&progress(&[
                ("web", "alpha", json!({"type": "completed"})),
                ("api", "alpha", waiting(0)),
                ("cron", "alpha", waiting(0)),
            ]))),
        )
        .unwrap();
    cancel(&store, &who, 2).unwrap();
    // The runner names api's cancel and reports cron's operation as unexecuted.
    store
        .record(
            &id(2),
            &a,
            RunEvidence::Executed {
                progress: tracker.changes(&progress(&[
                    ("web", "alpha", json!({"type": "completed"})),
                    ("api", "alpha", cancelled.clone()),
                    ("cron", "alpha", cancelled),
                ])),
                outcome: Box::new(outcome(json!({
                    "type": "failed", "completed": [operation("web")],
                    "failed": {"type": "operation", "operation": operation("api"), "error": {"type": "cancelled"}},
                    "unexecuted": [operation("cron")]
                }))),
                removed: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Cancelled);
    assert!(nodes(&store, &who, 2).contains(&("cron".to_owned(), NodeStatus::NotAttempted)));

    let refused = rollback_to(&store, &who, (3, 1)).unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    assert_eq!(removes_service(&refused), ["cron"]);
}

#[test]
fn a_rollback_leaves_alone_a_service_a_failed_deployment_never_attempted() {
    let (store, who, a) = cron_started();
    // #2 stopped at api, before cron: nothing of cron runs.
    store
        .record(&id(2), &a, failed_at(&["web"], "api", &["cron"]))
        .unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Failed);
    assert!(nodes(&store, &who, 2).contains(&("cron".to_owned(), NodeStatus::NotAttempted)));

    let rolled = rollback_to(&store, &who, (3, 1)).unwrap();
    assert_eq!(rolled.status, DeploymentStatus::Queued);
}

#[test]
fn a_deploy_tokens_rollback_that_destroys_names_what_and_who_can() {
    let (store, who) = shop();
    apply_both(&store, &who, 1);
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("extra").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(2), &a).unwrap();
    store
        .record(
            &id(2),
            &a,
            RunEvidence::Prepared(preview(&["web", "api", "extra"])),
        )
        .unwrap();
    store
        .record(&id(2), &a, succeeded(&["web", "api", "extra"]))
        .unwrap();
    // Rolling back to #1 takes extra off, which a protector's Deploy token can't
    // destroy: it names what and who can, rather than a bare permission.
    let token = Trusted {
        running: running(&["web", "api", "extra"]),
        ..protectors_deploy_token(Approval::NotRequired)
    };
    let refused = retry_as(&store, &who, (3, 1), &token).unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert_eq!(
        refused.message,
        "Needs changes:destroy: rolling back to #1 would remove Service extra. \
         Have an owner or admin redeploy #1."
    );
}

#[test]
fn a_deploy_tokens_destroying_deploy_outside_protection_names_any_member() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &ployz_store::SetEnvironmentProtected {
                environment: EnvironmentRef::default(),
                protected: false,
                seen: None,
            },
        )
        .unwrap();
    apply_both(&store, &who, 1);
    remove_web(&store, &who);
    // A member's Deploy token: it can't protect, so only protection would make it ask.
    let token = Trusted {
        approval: Approval::Required,
        running: running(&["web", "api"]),
        permissions: deploy_token().permissions.map(|held| {
            held.into_iter()
                .filter(|p| *p != Permission::EnvironmentsProtect)
                .collect()
        }),
        ..deploy_token()
    };
    let refused = deploy_trusted(&store, &who, 2, &token).unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert_eq!(
        refused.message,
        "Needs changes:destroy: this deploy would remove Service web. \
         Have a member, owner or admin deploy it."
    );
}
