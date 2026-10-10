#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Admission, runner ownership, replay and partial Node Outcomes, through the Store's
//! interface only, on SQLite and on Postgres (see `backend`).

use ployz_core::config::ReviewLifecycleKind;
use ployz_core::{
    ConfigFileName, ConfigName, DeployOutcome, DeployPreview, ExecutionError, OperationRow,
    RpcError, RpcErrorCode, ServiceName,
};
use ployz_store::{
    Actor, Admit, AttachConfig, Cancel, Change, Command, ConfigId, ConfigMountAt, ConfigStore,
    ConfigsQuery, CreateConfig, CreateProject, CreateService, DeleteConfig, Deploy, DeploymentId,
    DeploymentStatus, DeploymentSummary, DeploymentsQuery, DetachConfig, DiffQuery, DiffView,
    Discard, Edit, EnvironmentId, EnvironmentRef, NamespaceQuery, NodeStatus, OrganizationId,
    PlanQuery, Principal, ProjectId, ProjectName, PutConfigFile, Query, RemoveService,
    RenameConfig, RenameService, Retry, Revision, RowPhase, RowState, RowTracker, RunEvidence,
    RunnerId, ServerRow, ServiceLineageId, ServiceQuery, ServicesQuery, SettingPath, Start,
    Trusted, UploadBase, UploadedSource, View, Written,
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
fn a_deploy_publishes_then_its_runner_records_it_into_applied_state() {
    let (store, who) = shop();
    let admitted = admit(&store, &who, 1, &[], None).unwrap();
    assert_eq!(
        (admitted.status, admitted.saved, admitted.number),
        (DeploymentStatus::Queued, Revision(1), 1)
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
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Deployed)
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
            ("web".to_owned(), NodeStatus::Pending),
            ("api".to_owned(), NodeStatus::Unchanged)
        ]
    );
    // Ending without executing leaves what the preview settled: nothing ran on api either way.
    store
        .record(&id(2), &a, RunEvidence::NotExecuted("stopped".into()))
        .unwrap();
    assert_eq!(
        nodes(&store, &who, 2),
        [
            ("web".to_owned(), NodeStatus::NotAttempted),
            ("api".to_owned(), NodeStatus::Unchanged)
        ]
    );
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
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Failed),
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
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Pending),
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
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Failed)
        ]
    );
    // Only `api` is still to deploy; a different outcome can't overwrite this one.
    assert_eq!(changed(&store, &who), ["api"]);
    assert_eq!(
        code(store.record(&id(1), &a, succeeded(&["web", "api"]))),
        RpcErrorCode::Conflict
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
        assert_eq!(ployz_store::Failure::from(&error).cause, expected);
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
            (
                "web".to_owned(),
                "alpha".to_owned(),
                RowState::Failed {
                    reason: "remove Container failed".into(),
                    cause: vec!["the daemon is busy".into(), "connection refused".into()],
                    log: Vec::new(),
                }
            ),
            ("api".to_owned(), "beta".to_owned(), RowState::NotAttempted),
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
                *log = vec!["alpha disk log".into()];
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
                        "reason": row["reason"], "cause": row["cause"], "log": row["log"]
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
                        "log": ["alpha disk log"]
                    }),
                    json!({
                        "machine_id": snapshot[2].machine_id, "server": "same-name",
                        "state": "running", "phase": "waiting_for_health",
                        "reason": null, "cause": null, "log": null
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
                phase: RowPhase::WaitingForHealth
            }));
        assert!(volume.iter().all(|row| row.started_at.is_some()));
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
                phase: RowPhase::WaitingForHealth
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
            ("web".to_owned(), NodeStatus::NotAttempted),
            ("api".to_owned(), NodeStatus::NotAttempted)
        ]
    );
    assert_eq!(changed(&store, &who), ["web", "api"]);
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
    assert_eq!((second.number, second.saved), (2, Revision(2)));
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
            ("web".to_owned(), NodeStatus::NotAttempted),
            ("api".to_owned(), NodeStatus::NotAttempted)
        ]
    );
    assert_eq!(changed(&store, &who), ["web", "api"]);
}

#[test]
fn a_cancelled_running_deployment_keeps_its_confirmed_node_outcomes() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    assert_eq!(
        cancel(&store, &who, 1).unwrap().status,
        DeploymentStatus::Cancelling
    );
    // Its runner stops it partway and records what ran.
    let stopped = RunEvidence::Executed {
        progress: Vec::new(),
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [operation("web")],
            "failed": {"type": "operation", "operation": operation("api"), "error": {"type": "cancelled"}},
            "unexecuted": []
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, stopped).unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Cancelled);
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Failed)
        ]
    );
    assert_eq!(changed(&store, &who), ["api"]);
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
    assert_eq!(changed(&store, &who), ["web", "api"]);

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
        (3, DeploymentStatus::Queued, first.saved, first.services)
    );
    // Replaying it is idempotent; its ID with another source is not.
    assert_eq!(retry(&store, &who, 3, 1).unwrap().number, 3);
    assert_eq!(code(retry(&store, &who, 3, 2)), RpcErrorCode::Conflict);
    // The failed one keeps its Node Outcomes; the retry targets every node again.
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Failed)
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
    // A cancelled Deployment can be retried too, with its own Saved revision.
    let newer = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap()
        .deployment
        .saved;
    assert_ne!(newer, first.saved);
    assert_eq!(retry(&store, &who, 4, 2).unwrap().saved, newer);
}

#[test]
fn a_retry_is_refused_once_a_newer_deployment_applied() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    fail_api(&store, 1);
    admit(&store, &who, 2, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(2), &a, succeeded(&["web", "api"]))
        .unwrap();
    // Shipping #1's revision now would undo #2.
    let refused = retry(&store, &who, 3, 1).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert!(refused.message.contains("#2"), "{}", refused.message);
    assert_eq!(code(start(&store, &who, 3)), RpcErrorCode::NotFound);
}

#[test]
fn a_retry_is_refused_unless_its_deployment_ended_without_applying() {
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
    // Its runner finished before it saw the cancel: it applied, so nothing to retry.
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Applied);
    assert_eq!(code(retry(&store, &who, 9, 1)), RpcErrorCode::Conflict);
    // A superseded one never ran: deploy again instead.
    admit(&store, &who, 2, &[], None).unwrap();
    admit(&store, &who, 3, &[], None).unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Superseded);
    assert_eq!(code(retry(&store, &who, 9, 2)), RpcErrorCode::Conflict);
    // A refused retry queued nothing, and an accepted one supersedes what's queued.
    fail_api(&store, 3);
    assert_eq!(retry(&store, &who, 9, 3).unwrap().number, 4);
    assert_eq!(status(&store, &who, 3), DeploymentStatus::Failed);
    admit(&store, &who, 5, &[], None).unwrap();
    retry(&store, &who, 6, 3).unwrap();
    assert_eq!(status(&store, &who, 5), DeploymentStatus::Superseded);
    assert_eq!(status(&store, &who, 9), DeploymentStatus::Superseded);
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
        [3, 2]
    );
    let last = page(first.next_cursor.clone());
    assert_eq!(
        last.deployments
            .iter()
            .map(|d| d.number)
            .collect::<Vec<_>>(),
        [1]
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
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Deployed)
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
    let mut newer = upload.clone();
    newer.digest = ployz_core::UploadDigest::parse("e".repeat(64)).unwrap();
    with(3, Some(newer)).unwrap();
    assert_eq!(
        retry(&store, &who, 4, 1).unwrap().upload,
        Some(upload.clone())
    );
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
        }));
        let Written::Deployment(summary) = store.write(who, &command).unwrap() else {
            panic!("an admit writes a Deployment");
        };
        summary
    };
    // Cloud names who acts; the Deployment records them as its admitter and uploader.
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
    assert_eq!(claimed.uploads, vec![app.clone()]);
    assert!(claimed.sources.is_empty());
    let report = |status| {
        RunEvidence::Build(ployz_store::BuildReport {
            service: app.clone(),
            status,
            message: None,
            log: "GitHub can't build uploaded source\n".into(),
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
        "blank has nothing to run yet: add an image or connect a repository"
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
                number,
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
            .map(|unclaimed| &unclaimed.deployment)
            .collect::<Vec<_>>(),
        [&id(1)]
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
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Pending)
        ]
    );
    store.record(&id(1), &a, RunEvidence::Abandoned).unwrap();
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Unknown)
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
