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
    ServiceLineageId, SettingPath,
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
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(n)).unwrap(),
                environment: EnvironmentRef::default(),
                name: name(service),
                image: Some("nginx:1".into()),
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
    store
        .write(
            who,
            &Admit::Deploy(Deploy {
                id: deployment(n),
                environment: EnvironmentRef::default(),
                services: services.iter().map(|service| name(service)).collect(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
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
