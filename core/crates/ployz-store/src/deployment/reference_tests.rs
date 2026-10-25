//! A narrowed Deployment ships the Services the named ones reference at their
//! Saved snapshot, so it records them as it records the named ones.

use ployz_core::{
    ConfigFileName, ConfigName, DeployOutcome, DeployPreview, ExecutionError, ServiceName,
};
use serde_json::{Value, json};

use crate::{
    Actor, Admit, ConfigId, ConfigMountAt, ConfigStore, CreateConfig, CreateProject, CreateService,
    Deploy, DeploymentId, DeploymentStatus, DiffQuery, Edit, EnvironmentId, EnvironmentRef,
    NodeStatus, OrganizationId, ProjectId, ProjectName, PutConfigFile, RunEvidence, RunnerId,
    ServiceLineageId, SettingPath, UploadedSource, VolumeId, VolumeName,
};

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn name(name: &str) -> ServiceName {
    ServiceName::parse(name).unwrap()
}

fn deployment(n: u8) -> DeploymentId {
    DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap()
}

fn runner() -> RunnerId {
    RunnerId::parse("runner-a").unwrap()
}

fn store() -> (ConfigStore, Actor) {
    let store =
        ConfigStore::open("sqlite::memory:", crate::SealingKey::new(b"test").unwrap()).unwrap();
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
    (store, who)
}

fn create(store: &ConfigStore, who: &Actor, n: u8, service: &str) {
    create_from(store, who, n, service, Some("nginx:1"));
}

/// Create `service` pulling `image`, or, with none, built from an upload.
fn create_from(store: &ConfigStore, who: &Actor, n: u8, service: &str, image: Option<&str>) {
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(n)).unwrap(),
                environment: EnvironmentRef::default(),
                name: name(service),
                image: image.map(str::to_owned),
                template: None,
            },
        )
        .unwrap();
}

fn set(store: &ConfigStore, who: &Actor, changes: &[(&str, Value)]) {
    store
        .write(
            who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: changes
                    .iter()
                    .map(|(path, value)| crate::Change::Set {
                        path: SettingPath::parse(path).unwrap(),
                        value: value.clone(),
                    })
                    .collect(),
            },
        )
        .unwrap();
}

/// Save Working State and deploy `services` of it (none: every Service).
fn deploy(store: &ConfigStore, who: &Actor, n: u8, services: &[&str]) -> DeploymentId {
    admit(store, who, n, services, None)
}

/// As `deploy`, with an upload for the Services without a source of their own.
fn deploy_upload(store: &ConfigStore, who: &Actor, n: u8, services: &[&str]) -> DeploymentId {
    let upload = UploadedSource {
        digest: ployz_core::UploadDigest::parse("a".repeat(64)).unwrap(),
        base: None,
        uploader: None,
    };
    admit(store, who, n, services, Some(upload))
}

fn admit(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    services: &[&str],
    upload: Option<UploadedSource>,
) -> DeploymentId {
    store
        .write(
            who,
            &Admit::Deploy(Deploy {
                id: deployment(n),
                environment: EnvironmentRef::default(),
                services: services.iter().map(|service| name(service)).collect(),
                version: None,
                upload,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
        )
        .unwrap()
        .id
}

fn operation(service: &str) -> Value {
    let container = match service {
        "client" => "c",
        "postgres" => "d",
        _ => "e",
    }
    .repeat(64);
    json!({"type": "remove_container", "machine_id": "a".repeat(32), "container_id": container})
}

/// Run Deployment `id` to success, the runner planning and executing `ran`.
fn run(store: &ConfigStore, id: &DeploymentId, ran: &[&str]) -> DeploymentStatus {
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": ran.iter().enumerate().map(|(index, service)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": service,
            "operation": operation(service), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": [],
        "prune_refusal": "selected_services",
    }))
    .unwrap();
    let outcome: DeployOutcome<ExecutionError> = serde_json::from_value(json!({
        "type": "success",
        "completed": ran.iter().map(|service| operation(service)).collect::<Vec<_>>()
    }))
    .unwrap();
    store.claim(id, &runner()).unwrap();
    store
        .record(id, &runner(), RunEvidence::Prepared(preview))
        .unwrap();
    store
        .record(
            id,
            &runner(),
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap()
        .status
}

/// Each Service node of Deployment `id` with its outcome.
fn services(store: &ConfigStore, who: &Actor, id: &DeploymentId) -> Vec<(String, NodeStatus)> {
    store
        .read(who, &crate::DeploymentQuery { id: id.clone() })
        .unwrap()
        .nodes
        .into_iter()
        .filter(|node| matches!(node.node, crate::DeployedNode::Service { .. }))
        .map(|node| (node.node.name().to_owned(), node.outcome))
        .collect()
}

/// Every node of Deployment `id` with its outcome.
fn nodes(store: &ConfigStore, who: &Actor, id: &DeploymentId) -> Vec<(String, NodeStatus)> {
    store
        .read(who, &crate::DeploymentQuery { id: id.clone() })
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| (node.node.name().to_owned(), node.outcome))
        .collect()
}

fn publish(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &crate::Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
        )
        .unwrap();
}

fn put_file(store: &ConfigStore, who: &Actor, content: &str) {
    store
        .write(
            who,
            &PutConfigFile {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("app").unwrap(),
                file: ConfigFileName::parse("app.yml").unwrap(),
                content: content.into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
}

fn staged(store: &ConfigStore, who: &Actor) -> usize {
    store
        .read(
            who,
            &DiffQuery {
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap()
        .total_count
}

#[test]
fn a_narrowed_deploy_records_a_new_service_it_references() {
    let (store, who) = store();
    create(&store, &who, 3, "client");
    let first = deploy(&store, &who, 1, &[]);
    assert_eq!(run(&store, &first, &["client"]), DeploymentStatus::Applied);
    create(&store, &who, 4, "postgres");
    set(
        &store,
        &who,
        &[(
            "client.env.DB",
            json!("${{ postgres.PLOYZ_PRIVATE_DOMAIN }}"),
        )],
    );
    let narrowed = deploy(&store, &who, 2, &["client"]);
    assert_eq!(
        run(&store, &narrowed, &["postgres", "client"]),
        DeploymentStatus::Applied
    );
    assert_eq!(
        services(&store, &who, &narrowed),
        [
            ("client".to_owned(), NodeStatus::Deployed),
            ("postgres".to_owned(), NodeStatus::Deployed),
        ]
    );
    assert_eq!(staged(&store, &who), 0, "nothing stays staged");
}

#[test]
fn a_narrowed_deploy_lists_the_services_it_references_among_those_it_ships() {
    let (store, who) = store();
    create(&store, &who, 3, "client");
    let first = deploy(&store, &who, 1, &[]);
    assert_eq!(run(&store, &first, &["client"]), DeploymentStatus::Applied);
    create(&store, &who, 4, "postgres");
    set(
        &store,
        &who,
        &[(
            "client.env.DB",
            json!("${{ postgres.PLOYZ_PRIVATE_DOMAIN }}"),
        )],
    );
    let admitted = store
        .write(
            &who,
            &Admit::Deploy(Deploy {
                id: deployment(2),
                environment: EnvironmentRef::default(),
                services: vec![name("client")],
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
        )
        .unwrap();
    let listed = store
        .read(
            &who,
            &crate::DeploymentQuery {
                id: admitted.id.clone(),
            },
        )
        .unwrap()
        .deployment;
    for summary in [&admitted, &listed] {
        assert_eq!(summary.services, [name("client")]);
        assert_eq!(summary.ships, [name("client"), name("postgres")]);
    }
}

#[test]
fn a_rollback_reads_back_the_services_it_ships_as_admitted() {
    let (store, who) = store();
    create(&store, &who, 3, "client");
    let first = deploy(&store, &who, 1, &[]);
    assert_eq!(run(&store, &first, &["client"]), DeploymentStatus::Applied);
    create(&store, &who, 4, "postgres");
    let second = deploy(&store, &who, 2, &[]);
    assert_eq!(
        run(&store, &second, &["postgres", "client"]),
        DeploymentStatus::Applied
    );
    let admitted = store
        .write(
            &who,
            &Admit::Retry(crate::Retry {
                id: deployment(5),
                deployment: first,
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    let listed = store
        .read(
            &who,
            &crate::DeploymentQuery {
                id: admitted.id.clone(),
            },
        )
        .unwrap()
        .deployment;
    assert_eq!(admitted.ships, listed.ships);
}

#[test]
fn a_narrowed_deploy_records_the_saved_change_of_a_service_it_references() {
    let (store, who) = store();
    create(&store, &who, 3, "client");
    create(&store, &who, 4, "web");
    let first = deploy(&store, &who, 1, &[]);
    assert_eq!(
        run(&store, &first, &["client", "web"]),
        DeploymentStatus::Applied
    );
    set(
        &store,
        &who,
        &[
            ("web.env.MODE", json!("fast")),
            ("client.env.WEB", json!("${{ web.PLOYZ_PRIVATE_DOMAIN }}")),
        ],
    );
    let narrowed = deploy(&store, &who, 2, &["client"]);
    assert_eq!(
        run(&store, &narrowed, &["web", "client"]),
        DeploymentStatus::Applied
    );
    assert_eq!(
        services(&store, &who, &narrowed),
        [
            ("client".to_owned(), NodeStatus::Deployed),
            ("web".to_owned(), NodeStatus::Deployed),
        ]
    );
    assert_eq!(staged(&store, &who), 0, "web's change is applied");
}

#[test]
fn a_narrowed_deploy_leaves_out_a_referenced_service_without_a_saved_change() {
    let (store, who) = store();
    create(&store, &who, 3, "client");
    create(&store, &who, 4, "web");
    let first = deploy(&store, &who, 1, &[]);
    assert_eq!(
        run(&store, &first, &["client", "web"]),
        DeploymentStatus::Applied
    );
    set(
        &store,
        &who,
        &[("client.env.WEB", json!("${{ web.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    let narrowed = deploy(&store, &who, 2, &["client"]);
    assert_eq!(
        services(&store, &who, &narrowed),
        [("client".to_owned(), NodeStatus::Pending)]
    );
}

#[test]
fn a_narrowed_deploy_warns_of_a_fragile_file_a_service_it_references_mounts() {
    let (store, who) = store();
    create(&store, &who, 3, "client");
    create(&store, &who, 4, "web");
    let first = deploy(&store, &who, 1, &[]);
    assert_eq!(
        run(&store, &first, &["client", "web"]),
        DeploymentStatus::Applied
    );
    set(
        &store,
        &who,
        &[
            ("web.env.HOSTS", json!("a\nb")),
            ("client.env.WEB", json!("${{ web.PLOYZ_PRIVATE_DOMAIN }}")),
        ],
    );
    store
        .write(
            &who,
            &CreateConfig {
                id: ConfigId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("app").unwrap(),
                mounts: vec![ConfigMountAt {
                    service: name("web"),
                    dir: "/etc/app".into(),
                }],
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &PutConfigFile {
                environment: EnvironmentRef::default(),
                config: ConfigName::parse("app").unwrap(),
                file: ConfigFileName::parse("app.yml").unwrap(),
                content: "hosts: ${{ web.HOSTS }}\n".into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
    let narrowed = deploy(&store, &who, 2, &["client"]);
    let claimed = store.claim(&narrowed, &runner()).unwrap();
    assert_eq!(
        json!(claimed.deployment.warnings),
        json!([{ "config": "app", "file": "app.yml", "variable": "web.HOSTS" }])
    );
}

#[test]
fn a_narrowed_deploy_records_a_changed_file_a_service_it_references_mounts() {
    let (store, who) = store();
    create(&store, &who, 3, "client");
    create(&store, &who, 4, "web");
    store
        .write(
            &who,
            &CreateConfig {
                id: ConfigId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("app").unwrap(),
                mounts: vec![ConfigMountAt {
                    service: name("web"),
                    dir: "/etc/app".into(),
                }],
            },
        )
        .unwrap();
    put_file(&store, &who, "mode: slow\n");
    let first = deploy(&store, &who, 1, &[]);
    assert_eq!(
        run(&store, &first, &["client", "web"]),
        DeploymentStatus::Applied
    );
    put_file(&store, &who, "mode: fast\n");
    set(
        &store,
        &who,
        &[("client.env.WEB", json!("${{ web.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    let narrowed = deploy(&store, &who, 2, &["client"]);
    assert_eq!(
        run(&store, &narrowed, &["web", "client"]),
        DeploymentStatus::Applied
    );
    assert_eq!(
        nodes(&store, &who, &narrowed),
        [
            ("client".to_owned(), NodeStatus::Deployed),
            ("web".to_owned(), NodeStatus::Deployed),
            ("app".to_owned(), NodeStatus::Deployed),
        ]
    );
    assert_eq!(staged(&store, &who), 0, "the new file is applied");
}

#[test]
fn a_narrowed_deploy_records_a_changed_volume_a_service_it_references_mounts() {
    let (store, who) = store();
    create(&store, &who, 3, "client");
    create(&store, &who, 4, "web");
    store
        .write(
            &who,
            &crate::CreateVolume {
                id: VolumeId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                storage: ployz_core::config::VolumeKind::Docker {},
                mounts: vec![crate::Mount {
                    service: name("web"),
                    path: "/data".into(),
                }],
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
            },
        )
        .unwrap();
    let first = deploy(&store, &who, 1, &[]);
    assert_eq!(
        run(&store, &first, &["client", "web"]),
        DeploymentStatus::Applied
    );
    store
        .write(
            &who,
            &crate::SetVolumeSharedWrites {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
                shared_writes: true,
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        &[("client.env.WEB", json!("${{ web.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    let narrowed = deploy(&store, &who, 2, &["client"]);
    assert_eq!(
        run(&store, &narrowed, &["web", "client"]),
        DeploymentStatus::Applied
    );
    assert_eq!(
        nodes(&store, &who, &narrowed),
        [
            ("client".to_owned(), NodeStatus::Deployed),
            ("web".to_owned(), NodeStatus::Deployed),
            ("data".to_owned(), NodeStatus::Deployed),
        ]
    );
    assert_eq!(staged(&store, &who), 0, "the Volume change is applied");
}

#[test]
fn a_narrowed_deploy_of_an_uploaded_service_records_a_service_it_references() {
    let (store, who) = store();
    create_from(&store, &who, 3, "client", None);
    let first = deploy_upload(&store, &who, 1, &[]);
    assert_eq!(run(&store, &first, &["client"]), DeploymentStatus::Applied);
    create(&store, &who, 4, "postgres");
    set(
        &store,
        &who,
        &[(
            "client.env.DB",
            json!("${{ postgres.PLOYZ_PRIVATE_DOMAIN }}"),
        )],
    );
    let narrowed = deploy_upload(&store, &who, 2, &["client"]);
    assert_eq!(
        run(&store, &narrowed, &["postgres", "client"]),
        DeploymentStatus::Applied
    );
    assert_eq!(
        services(&store, &who, &narrowed),
        [
            ("client".to_owned(), NodeStatus::Deployed),
            ("postgres".to_owned(), NodeStatus::Deployed),
        ]
    );
    assert_eq!(staged(&store, &who), 0, "nothing stays staged");
}

#[test]
fn a_narrowed_deploy_of_an_uploaded_service_warns_of_its_secret_not_set() {
    use base64::Engine as _;
    let (store, who) = store();
    create_from(&store, &who, 3, "client", None);
    let sealed = json!({ "secret": {
        "sealedFor": uuid(2),
        "value": { "sealed": base64::engine::general_purpose::STANDARD.encode([1; 48]), "scope": { "service": uuid(3) }, "key": "KEY" },
    } });
    set(&store, &who, &[("client.env.KEY", sealed)]);
    publish(&store, &who);
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![crate::Change::Unset {
                    path: SettingPath::parse("client.env.KEY").unwrap(),
                }],
            },
        )
        .unwrap();
    publish(&store, &who);
    // Undoing the removal brings KEY back without its value.
    let query = crate::HistoryPreviewQuery {
        environment: EnvironmentRef::default(),
        revision: crate::Revision(2),
        action: crate::HistoryAction::Undo,
        change: None,
    };
    let preview = store.read(&who, &query).unwrap();
    store
        .write(
            &who,
            &crate::StageHistory {
                environment: EnvironmentRef::default(),
                revision: query.revision,
                action: query.action,
                change: None,
                version: preview.version,
                accept_overwrite: true,
            },
        )
        .unwrap();
    let narrowed = deploy_upload(&store, &who, 1, &["client"]);
    let claimed = store.claim(&narrowed, &runner()).unwrap();
    assert_eq!(
        json!(claimed.deployment.warnings),
        json!([{ "service": "client", "variable": "KEY" }])
    );
}

/// The restarts the plan of `deploy services` shows for Config `app`, or none
/// when the plan leaves it out.
fn planned_restarts(store: &ConfigStore, who: &Actor, services: &[&str]) -> Option<Vec<String>> {
    store
        .read(
            who,
            &crate::PlanQuery {
                environment: EnvironmentRef::default(),
                services: services.iter().map(|service| name(service)).collect(),
            },
        )
        .unwrap()
        .changes
        .into_iter()
        .find(|change| change.name == "app")
        .map(|change| change.restarts.iter().map(ToString::to_string).collect())
}

/// Deploy `client` and `web`, `web` mounting Config `app` with `mode: slow`, then
/// change the file and have `client` reference `web`.
fn change_a_file_web_mounts(store: &ConfigStore, who: &Actor, also: &[&str]) {
    create(store, who, 3, "client");
    create(store, who, 4, "web");
    for (n, service) in also.iter().enumerate() {
        create(store, who, 6 + u8::try_from(n).unwrap(), service);
    }
    store
        .write(
            who,
            &CreateConfig {
                id: ConfigId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ConfigName::parse("app").unwrap(),
                mounts: ["web"]
                    .iter()
                    .chain(also)
                    .map(|service| ConfigMountAt {
                        service: name(service),
                        dir: "/etc/app".into(),
                    })
                    .collect(),
            },
        )
        .unwrap();
    put_file(store, who, "mode: slow\n");
    let first = deploy(store, who, 1, &[]);
    let ran: Vec<&str> = ["client", "web"].iter().chain(also).copied().collect();
    assert_eq!(run(store, &first, &ran), DeploymentStatus::Applied);
    put_file(store, who, "mode: fast\n");
    set(
        store,
        who,
        &[("client.env.WEB", json!("${{ web.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
}

#[test]
fn a_narrowed_plan_shows_the_restart_of_a_service_it_references() {
    let (store, who) = store();
    change_a_file_web_mounts(&store, &who, &[]);
    assert_eq!(
        planned_restarts(&store, &who, &["client"]),
        Some(vec!["web".to_owned()])
    );
}

#[test]
fn a_narrowed_plan_shows_a_shared_file_change_a_service_it_references_restarts_for() {
    let (store, who) = store();
    change_a_file_web_mounts(&store, &who, &["api"]);
    assert_eq!(
        planned_restarts(&store, &who, &["client"]),
        Some(vec!["web".to_owned()]),
        "api, which this Deploy doesn't ship, keeps the old file"
    );
}
