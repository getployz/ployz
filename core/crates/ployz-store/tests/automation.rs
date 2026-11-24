#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Git automation through `system`, on SQLite and on Postgres (see `backend`): pushes
//! and check suites admit auto-deploys of Saved State, idempotently.

use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Automated, BranchHead, Change, Command, ConfigStore, Edit, EnvironmentRef, Publish,
    SettingPath, SystemEvent, Trusted, Written,
};
use serde_json::json;

use ployz_store::testing::backend;

use ployz_store::testing::shop::*;

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
                target: Some(ployz_store::DiscardTarget::Head),
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
        failure: None,
    }
}

/// Deploy Volume `data` on `web`, then remove it and Save, accepting its loss
/// while Server `a` holds it.
fn saved_volume_loss() -> (ConfigStore, Actor) {
    saved_loss_of(false)
}

/// [`saved_volume_loss`], where `orphaned` unmounts `data` from `web` and
/// deploys that before the removal, so no Applied Service mounts it.
fn saved_loss_of(orphaned: bool) -> (ConfigStore, Actor) {
    let (store, who) = shop();
    let data = || ployz_store::VolumeName::parse("data").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
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
    if orphaned {
        store
            .write(
                &who,
                &Command::Edit(Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Unset {
                        path: SettingPath::parse("web.mounts.data").unwrap(),
                    }],
                }),
            )
            .unwrap();
        backend::deploy(&store, &who, "production", 2);
    }
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
                id: None,
                environment: EnvironmentRef::default(),
                version,
                message: None,
                accept_volume_loss: vec![data()],
                rule: None,
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
fn a_push_that_builds_another_service_still_deletes_the_accepted_data() {
    let (store, who) = saved_volume_loss();
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    system(&store, &push(None, Some(H1), Some(&["web/main.rs"])));
    let narrow = push(Some(H1), Some(H2), Some(&["api/main.rs"]));
    assert_eq!(
        deletes_of(&store, &narrow, Some(held_on_a())),
        [format!("{}:{DOCKER_VOLUME}", "a".repeat(32))]
    );
}

/// A push that builds only `api` deploys beside `web`'s Volume, saved with shared
/// writes on before the pause: only a Volume a Service it ships mounts can refuse it.
#[test]
fn a_push_deploys_beside_a_shared_volume_only_another_service_mounts() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_on(ConfigStore::open(&url, backend::key()).unwrap());
    store
        .write(
            &who,
            &ployz_store::CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: ployz_store::VolumeId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_store::VolumeName::parse("data").unwrap(),
                mounts: vec![ployz_store::Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    set(&store, &who, "api.watchPaths", json!(["api/**"]));
    publish(&store, &who);
    let first = system(&store, &push(None, Some(H1), Some(&["web/main.rs"])));
    // How a Volume saved and deployed before the pause looks.
    backend::execute(
        &url,
        "UPDATE config_saved SET intent = \
         replace(intent, '\"name\":\"data\"', '\"name\":\"data\",\"sharedWrites\":true')",
    );
    backend::run(&store, &first.admitted[0].deployment.id);
    let narrow = system(&store, &push(Some(H1), Some(H2), Some(&["api/main.rs"])));
    let shipped: Vec<Vec<String>> = deployed(&store, &who, &narrow)
        .into_iter()
        .map(|(services, _)| services)
        .collect();
    assert_eq!(shipped, [["api"]]);
}

#[test]
fn a_push_never_deletes_data_no_save_accepted() {
    let (store, who) = shop();
    let data = || ployz_store::VolumeName::parse("data").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateVolume {
                restored_from: None,
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
    // No Server held it at Save, so the Save asked nothing and accepted no loss.
    let mut empty = held_on_a();
    empty.held.clear();
    store
        .write_trusted(
            &who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
                rule: None,
            },
            &Trusted {
                volumes: Some(empty),
                ..Trusted::default()
            },
        )
        .unwrap();
    assert!(pushed_deletes(&store, Some(held_on_a())).is_empty());
}

#[test]
fn a_push_removes_a_volume_the_servers_still_show_empty() {
    let (store, who) = shop();
    let data = || ployz_store::VolumeName::parse("data").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateVolume {
                restored_from: None,
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
    let mut empty = held_on_a();
    empty.held.clear();
    store
        .write_trusted(
            &who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
                rule: None,
            },
            &Trusted {
                volumes: Some(empty.clone()),
                ..Trusted::default()
            },
        )
        .unwrap();

    let Written::Automated(automated) = store
        .system(
            &org(),
            &push(None, Some(H1), None),
            &Trusted {
                volumes: Some(empty),
                ..Trusted::default()
            },
        )
        .unwrap()
    else {
        panic!("a system event writes Automated")
    };
    let nodes = store
        .read(
            &who,
            &ployz_store::DeploymentQuery {
                id: automated.admitted[0].deployment.id.clone(),
            },
        )
        .unwrap()
        .nodes;
    assert!(
        nodes.iter().any(
            |node| matches!(node.node, ployz_store::DeployedNode::Volume { .. })
                && node.node.name() == "data"
        ),
        "{nodes:?}"
    );
}

/// Deploy production, then add Volume `data` on `web` and admit a Deploy that
/// creates it, left in flight, then remove `data` from Working State.
fn volume_in_flight() -> (ConfigStore, Actor) {
    let (store, who) = shop();
    backend::deploy(&store, &who, "production", 1);
    store
        .write(
            &who,
            &ployz_store::CreateVolume {
                restored_from: None,
                shared_writes: false,
                storage_locked: false,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: ployz_store::VolumeId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_store::VolumeName::parse("data").unwrap(),
                mounts: vec![ployz_store::Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
    backend::admit(&store, &who, "production", 2);
    store
        .write(
            &who,
            &ployz_store::RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: ployz_store::VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    (store, who)
}

fn save_dropping_data(
    store: &ConfigStore,
    who: &Actor,
    version: Option<String>,
) -> Result<ployz_store::Published, ployz_core::RpcError> {
    store.write_trusted(
        who,
        &Publish {
            id: None,
            environment: EnvironmentRef::default(),
            version,
            message: None,
            accept_volume_loss: vec![ployz_store::VolumeName::parse("data").unwrap()],
            rule: None,
        },
        &Trusted {
            volumes: Some(held_on_a()),
            ..Trusted::default()
        },
    )
}

#[test]
fn a_save_asks_to_delete_a_volume_a_deploy_in_flight_creates() {
    let (store, who) = volume_in_flight();
    let removals = store
        .read(&who, &ployz_store::RemovalsQuery::default())
        .unwrap();
    assert_eq!(
        texts(
            &removals
                .volumes
                .iter()
                .map(|volume| &volume.name)
                .collect::<Vec<_>>()
        ),
        ["data"]
    );
    let refused = save_dropping_data(&store, &who, None).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
    assert!(pushed_deletes(&store, Some(held_on_a())).is_empty());

    let (store, who) = volume_in_flight();
    let refused = save_dropping_data(&store, &who, None).unwrap_err();
    save_dropping_data(
        &store,
        &who,
        Some(refused.details["version"].as_str().unwrap().to_owned()),
    )
    .unwrap();
    assert_eq!(
        pushed_deletes(&store, Some(held_on_a())),
        [format!("{}:{DOCKER_VOLUME}", "a".repeat(32))]
    );
}

#[test]
fn the_diff_marks_deleted_a_volume_a_deploy_in_flight_creates() {
    let (store, who) = volume_in_flight();
    let diff = store
        .read(&who, &ployz_store::DiffQuery::default())
        .unwrap();
    let data = diff
        .changes
        .iter()
        .find(|change| change.name == "data")
        .unwrap();
    assert_eq!(
        data.lifecycle,
        ployz_core::config::ReviewLifecycleKind::Delete
    );
    assert_eq!(data.data, Some(ployz_store::DataEffect::Deleted));
}

#[test]
fn the_diff_does_not_say_a_volume_it_deletes_keeps_its_data() {
    let (store, who) = volume_in_flight();
    store
        .write(
            &who,
            &ployz_store::RemoveService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    let diff = store
        .read(&who, &ployz_store::DiffQuery::default())
        .unwrap();
    let data = |name: &str| {
        diff.changes
            .iter()
            .find(|change| change.name == name)
            .unwrap()
            .data
    };
    assert_eq!(data("data"), Some(ployz_store::DataEffect::Deleted));
    assert_eq!(data("web"), None);
}

#[test]
fn the_diff_does_not_say_a_volume_replaced_by_one_of_its_name_is_kept() {
    let (store, who) = volume_in_flight();
    store
        .write(
            &who,
            &ployz_store::CreateVolume {
                restored_from: None,
                shared_writes: false,
                storage_locked: false,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: ployz_store::VolumeId::parse("00000000-0000-4000-8000-000000000006").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_store::VolumeName::parse("data").unwrap(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
    let diff = store
        .read(&who, &ployz_store::DiffQuery::default())
        .unwrap();
    let web = diff
        .changes
        .iter()
        .find(|change| change.name == "web")
        .unwrap();
    assert_eq!(web.data, None);
}

#[test]
fn the_diff_restarts_a_service_a_deploy_in_flight_creates() {
    let (store, who) = shop();
    let sentry = || ployz_core::ConfigName::parse("sentry").unwrap();
    let put = |content: &str| {
        store
            .write(
                &who,
                &ployz_store::PutConfigFile {
                    environment: EnvironmentRef::default(),
                    config: sentry(),
                    file: ployz_core::ConfigFileName::parse("config.yml").unwrap(),
                    content: content.into(),
                    mode: None,
                    uid: None,
                    gid: None,
                },
            )
            .unwrap();
    };
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: sentry(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    put("value: old");
    backend::deploy(&store, &who, "production", 1);
    store
        .write(
            &who,
            &ployz_store::CreateService {
                id: ployz_store::ServiceLineageId::parse("00000000-0000-4000-8000-000000000007")
                    .unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("worker").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::AttachConfig {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("worker").unwrap(),
                config: sentry(),
                dir: "/etc/sentry".into(),
            },
        )
        .unwrap();
    backend::admit(&store, &who, "production", 2);
    put("value: new");
    let diff = store
        .read(&who, &ployz_store::DiffQuery::default())
        .unwrap();
    let restarts = &diff
        .changes
        .iter()
        .find(|change| change.name == "sentry")
        .unwrap()
        .restarts;
    assert_eq!(
        restarts.iter().map(ServiceName::as_str).collect::<Vec<_>>(),
        ["web", "worker"]
    );
}

#[test]
fn a_deploy_asks_to_delete_a_volume_a_deploy_in_flight_creates() {
    let (store, who) = volume_in_flight();
    let deploy = |version: Option<String>| {
        store.write_trusted(
            &who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: ployz_store::DeploymentId::parse("00000000-0000-4000-8000-000000000103")
                    .unwrap(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version,
                upload: None,
                accept_volume_loss: vec![ployz_store::VolumeName::parse("data").unwrap()],
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                copies: Vec::new(),
                recreate: Vec::new(),
                rule: None,
            }),
            &Trusted {
                volumes: Some(held_on_a()),
                ..Trusted::default()
            },
        )
    };
    let refused = deploy(None).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
    deploy(Some(
        refused.details["version"].as_str().unwrap().to_owned(),
    ))
    .unwrap();
    let deletes: Vec<String> = store
        .claim(
            &ployz_store::DeploymentId::parse("00000000-0000-4000-8000-000000000103").unwrap(),
            &ployz_store::RunnerId::parse("runner").unwrap(),
        )
        .unwrap()
        .deletes
        .iter()
        .map(|volume| format!("{}:{}", volume.machine_id, volume.name))
        .collect();
    assert_eq!(deletes, [format!("{}:{DOCKER_VOLUME}", "a".repeat(32))]);
}

#[test]
fn a_narrow_push_deletes_saved_loss_data_no_service_mounts() {
    let (store, who) = saved_loss_of(true);
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    system(&store, &push(None, Some(H1), None));
    let narrow = push(Some(H1), Some(H2), Some(&["api/main.rs"]));
    assert_eq!(
        deletes_of(&store, &narrow, Some(held_on_a())),
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
fn open_pull_request(store: &ConfigStore, who: &Actor) -> Automated {
    open_as(store, who, &pull_request(true))
}

/// Turn on pull request environments from `production`, and report `event`.
fn open_as(store: &ConfigStore, who: &Actor, event: &SystemEvent) -> Automated {
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
    system(store, event)
}

/// Pull request #5 onto `main`: open, or merged.
fn pull_request(open: bool) -> SystemEvent {
    SystemEvent::PullRequest(ployz_store::PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        title: "Add search".into(),
        author: "ada".into(),
        author_id: Some(backend::ada()),
        bot: false,
        head_branch: backend::git_branch("search"),
        head_repository_id: Some(backend::repo_id(11)),
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

/// Pull request #5 open at `head`, by someone outside the org, from `acme/web` itself.
fn by_stranger(head: &str, minute: u8) -> SystemEvent {
    let SystemEvent::PullRequest(open) = pull_request(true) else {
        unreachable!("pull_request is a pull request")
    };
    SystemEvent::PullRequest(ployz_store::PullRequest {
        author: "eve".into(),
        author_id: Some(ployz_store::GithubUserId::parse(42).unwrap()),
        head: backend::sha(head),
        updated: ployz_store::GithubTimestamp::parse(format!("2026-09-29T10:{minute:02}:00Z"))
            .unwrap(),
        ..open
    })
}

/// A push to pull request #5's branch, from `base`.
fn push_search(base: Option<&str>, head: &str) -> SystemEvent {
    SystemEvent::BranchHead(BranchHead {
        repository_id: backend::repo_id(11),
        branch: backend::git_branch("search"),
        base: base.map(backend::sha),
        head: Some(backend::sha(head)),
        changed: None,
    })
}

fn approve(store: &ConfigStore, who: &Actor, head: &str) -> Automated {
    store
        .write(
            who,
            &ployz_store::ApprovePullRequest {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
                head: backend::sha(head),
            },
        )
        .unwrap()
}

fn reasons(automated: &Automated) -> Vec<&str> {
    automated
        .skipped
        .iter()
        .map(|skipped| skipped.reason.as_str())
        .collect()
}

#[test]
fn a_push_to_a_members_pull_request_deploys_its_head() {
    let (store, who) = shop();
    publish(&store, &who);
    let opened = open_pull_request(&store, &who);
    backend::run(&store, &opened.admitted[0].deployment.id);
    let pushed = system(&store, &push_search(None, H3));
    assert_eq!(pushed.admitted.len(), 1, "{pushed:?}");
    assert_eq!(pinned(&store, &pushed, "web").as_deref(), Some(H3));
}

#[test]
fn a_push_to_a_non_members_pull_request_waits_for_approval_of_its_head() {
    let (store, who) = shop();
    publish(&store, &who);
    let opened = open_as(&store, &who, &by_stranger(H2, 1));
    assert!(opened.admitted.is_empty(), "{opened:?}");
    assert!(system(&store, &push_search(None, H2)).admitted.is_empty());
    let approved = approve(&store, &who, H2);
    assert_eq!(pinned(&store, &approved, "web").as_deref(), Some(H2));
    backend::run(&store, &approved.admitted[0].deployment.id);

    // The push can arrive before the facts: the approval was for another head.
    let early = system(&store, &push_search(Some(H2), H3));
    assert!(early.admitted.is_empty(), "{early:?}");
    assert_eq!(reasons(&early), ["Needs approval"]);
    let reported = system(&store, &by_stranger(H3, 2));
    assert!(reported.admitted.is_empty(), "{reported:?}");
    assert!(
        system(&store, &push_search(Some(H2), H3))
            .admitted
            .is_empty()
    );

    let approved = approve(&store, &who, H3);
    assert_eq!(approved.admitted.len(), 1, "{approved:?}");
    assert_eq!(pinned(&store, &approved, "web").as_deref(), Some(H3));
}

#[test]
fn an_approval_that_made_no_pr_environment_stays_listed_to_approve_again() {
    let (store, who) = shop();
    publish(&store, &who);
    assert!(
        open_as(&store, &who, &by_stranger(H2, 1))
            .admitted
            .is_empty()
    );
    let approval = ployz_store::ApprovePullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        head: backend::sha(H2),
    };
    let serverless = Trusted {
        servers: Some(0),
        ..Trusted::default()
    };
    let stalled = store.write_trusted(&who, &approval, &serverless).unwrap();
    assert!(stalled.admitted.is_empty(), "{stalled:?}");

    let plans = store
        .read(&who, &ployz_store::PrPlansQuery::default())
        .unwrap();
    let listed: Vec<_> = plans.plans[0]
        .open
        .iter()
        .map(|open| {
            (
                open.number.get(),
                open.standing.clone(),
                open.environment.clone(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [(5, ployz_store::Standing::Approved(backend::sha(H2)), None)]
    );

    let approved = approve(&store, &who, H2);
    assert_eq!(approved.admitted.len(), 1, "{approved:?}");
    assert_eq!(pinned(&store, &approved, "web").as_deref(), Some(H2));
}

#[test]
fn a_push_refused_before_the_author_proved_a_member_deploys_once_they_do() {
    let (store, who) = shop();
    publish(&store, &who);
    let SystemEvent::PullRequest(open) = pull_request(true) else {
        unreachable!("pull_request is a pull request")
    };
    let unknown = SystemEvent::PullRequest(ployz_store::PullRequest {
        author_id: None,
        ..open.clone()
    });
    assert!(open_as(&store, &who, &unknown).admitted.is_empty());
    assert!(system(&store, &push_search(None, H2)).admitted.is_empty());
    let approved = approve(&store, &who, H2);
    backend::run(&store, &approved.admitted[0].deployment.id);

    let early = system(&store, &push_search(Some(H2), H3));
    assert_eq!(reasons(&early), ["Needs approval"]);
    let member = SystemEvent::PullRequest(ployz_store::PullRequest {
        head: backend::sha(H3),
        updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:05:00Z").unwrap(),
        ..open
    });
    let reported = system(&store, &member);
    assert_eq!(reported.admitted.len(), 1, "{reported:?}");
    assert_eq!(pinned(&store, &reported, "web").as_deref(), Some(H3));
}

/// What Cloud reports once `ada`, who wrote pull request #5, left the Organization.
fn ada_left() -> Trusted {
    Trusted {
        members: Some(Vec::new()),
        ..Trusted::default()
    }
}

/// Each pull request the PR plans list, with its standing.
fn standings(store: &ConfigStore, who: &Actor) -> Vec<(u64, ployz_store::Standing)> {
    store
        .read(who, &ployz_store::PrPlansQuery::default())
        .unwrap()
        .plans
        .into_iter()
        .flat_map(|plan| plan.open)
        .map(|open| (open.number.get(), open.standing))
        .collect()
}

fn system_with(store: &ConfigStore, event: &SystemEvent, trusted: &Trusted) -> Automated {
    let Written::Automated(automated) = store.system(&org(), event, trusted).unwrap() else {
        panic!("a system event writes Automated")
    };
    automated
}

#[test]
fn a_push_after_its_author_left_the_organization_waits_for_approval() {
    let (store, who) = shop();
    publish(&store, &who);
    let opened = open_pull_request(&store, &who);
    backend::run(&store, &opened.admitted[0].deployment.id);

    // The push lands before GitHub's pull_request event tells of it.
    let pushed = system_with(&store, &push_search(None, H3), &ada_left());
    assert!(pushed.admitted.is_empty(), "{pushed:?}");
    assert_eq!(reasons(&pushed), ["Needs approval"]);
    assert_eq!(
        standings(&store, &who),
        [(5, ployz_store::Standing::NeedsApproval)]
    );
}

#[test]
fn a_push_refused_after_its_author_left_deploys_once_a_report_finds_them_a_member_again() {
    let (store, who) = shop();
    publish(&store, &who);
    let opened = open_pull_request(&store, &who);
    backend::run(&store, &opened.admitted[0].deployment.id);
    let pushed = system_with(&store, &push_search(None, H3), &ada_left());
    assert_eq!(reasons(&pushed), ["Needs approval"]);

    let SystemEvent::PullRequest(open) = pull_request(true) else {
        unreachable!("pull_request is a pull request")
    };
    let rejoined = system(
        &store,
        &SystemEvent::PullRequest(ployz_store::PullRequest {
            head: backend::sha(H3),
            updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:05:00Z").unwrap(),
            ..open
        }),
    );
    assert_eq!(rejoined.admitted.len(), 1, "{rejoined:?}");
    assert_eq!(pinned(&store, &rejoined, "web").as_deref(), Some(H3));
    assert_eq!(
        standings(&store, &who),
        [(5, ployz_store::Standing::Trusted)]
    );
}

#[test]
fn a_push_held_for_ci_whose_author_left_before_it_passed_waits_for_approval() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    let opened = open_pull_request(&store, &who);
    backend::run(&store, &opened.admitted[0].deployment.id);
    let pushed = system(&store, &push_search(None, H3));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");
    assert_eq!(
        standings(&store, &who),
        [(5, ployz_store::Standing::Trusted)]
    );

    let passed = system_with(
        &store,
        &suite(1, H3, "completed", Some("success"), 30),
        &ada_left(),
    );
    assert!(passed.admitted.is_empty(), "{passed:?}");
    assert_eq!(reasons(&passed), ["Needs approval"]);
    assert_eq!(
        standings(&store, &who),
        [(5, ployz_store::Standing::NeedsApproval)]
    );
}

#[test]
fn another_repositorys_push_never_ships_a_build_of_a_pull_request_whose_author_left() {
    let (store, who) = shop();
    let worker = Trusted {
        repositories: vec![ployz_store::AuthorizedRepository {
            repository: backend::repo_name("acme/worker"),
            repository_id: backend::repo_id(12),
            access: ployz_core::config::ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: backend::git_branch("main"),
            branches: Vec::new(),
        }],
        ..Trusted::default()
    };
    store
        .write_trusted(
            &who,
            &ployz_store::CreateGitService {
                id: ployz_store::ServiceLineageId::parse("00000000-0000-4000-8000-000000000005")
                    .unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("worker").unwrap(),
                repository: backend::repo_name("acme/worker"),
                branch: None,
            },
            &worker,
        )
        .unwrap();
    publish(&store, &who);
    store
        .write(
            &who,
            &ployz_store::SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(ployz_store::EnvironmentName::parse("production").unwrap()),
                copy: Some(vec![ployz_store::NodeName::parse("worker").unwrap()]),
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    let opened = system(&store, &pull_request(true));
    let preview = opened.admitted[0].environment.clone();
    let first = &opened.admitted[0].deployment.id;
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let claimed = store.claim(first, &runner).unwrap();
    for service in ["web", "api"] {
        store
            .record(
                first,
                &runner,
                ployz_store::RunEvidence::Build(ployz_store::BuildReport {
                    service: ServiceName::parse(service).unwrap(),
                    status: ployz_store::BuildStatus::Built,
                    message: None,
                    log: String::new(),
                    server: None,
                }),
            )
            .unwrap();
    }
    backend::finish(&store, first, claimed);
    let pr_5 = EnvironmentRef {
        project: None,
        environment: Some(ployz_store::EnvironmentName::parse("pr-5").unwrap()),
    };
    store
        .write(
            &who,
            &Command::Edit(Edit {
                environment: pr_5.clone(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("api.replicas").unwrap(),
                    value: json!(2),
                }],
            }),
        )
        .unwrap();
    store
        .write(
            &who,
            &Command::Publish(Publish {
                id: None,
                environment: pr_5,
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
                rule: None,
            }),
        )
        .unwrap();

    let pushed = system_with(
        &store,
        &SystemEvent::BranchHead(BranchHead {
            repository_id: backend::repo_id(12),
            branch: backend::git_branch("main"),
            base: None,
            head: Some(backend::sha(H1)),
            changed: None,
        }),
        &ada_left(),
    );
    let deployed = pushed
        .admitted
        .iter()
        .find(|admitted| admitted.environment == preview)
        .expect("the push deploys pr-5's worker");
    let api = store
        .sources(&deployed.deployment.id)
        .unwrap()
        .into_iter()
        .find(|source| source.service.as_str() == "api")
        .expect("api ships its Saved change");
    assert_eq!(api.commit, None, "{api:?}");
    assert_eq!(
        standings(&store, &who),
        [(5, ployz_store::Standing::NeedsApproval)]
    );
}

/// Admit a Deploy of `pr-5` as Deployment `n`, which pins nothing yet.
fn deploy_pr_5(store: &ConfigStore, who: &Actor, n: u8) -> ployz_store::DeploymentId {
    let id = ployz_store::DeploymentId::parse(format!("00000000-0000-4000-8000-0000000002{n:02}"))
        .unwrap();
    store
        .write_trusted(
            who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: id.clone(),
                environment: EnvironmentRef {
                    project: None,
                    environment: Some(ployz_store::EnvironmentName::parse("pr-5").unwrap()),
                },
                services: vec![ServiceName::parse("web").unwrap()],
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
    id
}

fn pin_web(
    store: &ConfigStore,
    id: &ployz_store::DeploymentId,
    head: &str,
    trusted: &Trusted,
) -> Result<Vec<ployz_store::GitSource>, ployz_core::RpcError> {
    store.pin(
        id,
        &std::collections::BTreeMap::from([(
            ServiceName::parse("web").unwrap(),
            backend::sha(head),
        )]),
        trusted,
    )
}

#[test]
fn a_build_of_a_trusted_pull_request_pins_only_while_its_author_is_a_member() {
    let (store, who) = shop();
    publish(&store, &who);
    let opened = open_pull_request(&store, &who);
    backend::run(&store, &opened.admitted[0].deployment.id);

    // Without the members Cloud read, no member is trusted, and none is distrusted.
    let unknown = deploy_pr_5(&store, &who, 1);
    let refused = pin_web(&store, &unknown, H3, &Trusted::default()).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        standings(&store, &who),
        [(5, ployz_store::Standing::Trusted)]
    );
    let pinned = pin_web(&store, &unknown, H3, &backend::members()).unwrap();
    assert_eq!(pinned[0].commit, Some(backend::sha(H3)));

    backend::run(&store, &unknown);
    let left = deploy_pr_5(&store, &who, 2);
    let refused = pin_web(&store, &left, H3, &ada_left()).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        standings(&store, &who),
        [(5, ployz_store::Standing::NeedsApproval)]
    );
}

#[test]
fn a_bots_pull_request_stays_trusted_whoever_the_members_are() {
    let (store, who) = shop();
    publish(&store, &who);
    let SystemEvent::PullRequest(open) = pull_request(true) else {
        unreachable!("pull_request is a pull request")
    };
    let bot = SystemEvent::PullRequest(ployz_store::PullRequest {
        author: "renovate[bot]".into(),
        author_id: None,
        bot: true,
        ..open
    });
    store
        .write(
            &who,
            &ployz_store::SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(ployz_store::EnvironmentName::parse("production").unwrap()),
                copy: None,
                setup: None,
                remove_on_close: None,
                include_bots: Some(true),
            },
        )
        .unwrap();
    let opened = system_with(&store, &bot, &ada_left());
    backend::run(&store, &opened.admitted[0].deployment.id);
    let pushed = system_with(&store, &push_search(None, H3), &Trusted::default());
    assert_eq!(pushed.admitted.len(), 1, "{pushed:?}");
}

#[test]
fn a_pull_request_whose_author_left_deploys_the_head_a_member_approves() {
    let (store, who) = shop();
    publish(&store, &who);
    let opened = open_pull_request(&store, &who);
    backend::run(&store, &opened.admitted[0].deployment.id);
    let SystemEvent::PullRequest(open) = pull_request(true) else {
        unreachable!("pull_request is a pull request")
    };
    let moved = SystemEvent::PullRequest(ployz_store::PullRequest {
        head: backend::sha(H3),
        updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:05:00Z").unwrap(),
        ..open
    });
    let reported = system_with(&store, &moved, &ada_left());
    assert!(reported.admitted.is_empty(), "{reported:?}");

    let approved = approve(&store, &who, H3);
    assert_eq!(approved.admitted.len(), 1, "{approved:?}");
    assert_eq!(pinned(&store, &approved, "web").as_deref(), Some(H3));
    let early = system_with(&store, &push_search(None, H1), &ada_left());
    assert_eq!(reasons(&early), ["Needs approval"]);
}

#[test]
fn a_manual_deploy_never_pins_a_head_the_pull_request_has_not_approved() {
    let (store, who) = shop();
    publish(&store, &who);
    open_as(&store, &who, &by_stranger(H2, 1));
    assert!(system(&store, &push_search(None, H2)).admitted.is_empty());
    let approved = approve(&store, &who, H2);
    backend::run(&store, &approved.admitted[0].deployment.id);
    assert!(system(&store, &by_stranger(H3, 2)).admitted.is_empty());
    assert!(
        system(&store, &push_search(Some(H2), H3))
            .admitted
            .is_empty()
    );

    let id = ployz_store::DeploymentId::parse("00000000-0000-4000-8000-000000000201").unwrap();
    store
        .write_trusted(
            &who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: id.clone(),
                environment: EnvironmentRef {
                    project: None,
                    environment: Some(ployz_store::EnvironmentName::parse("pr-5").unwrap()),
                },
                services: vec![ServiceName::parse("web").unwrap()],
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
    let refused = store
        .pin(
            &id,
            &std::collections::BTreeMap::from([(
                ServiceName::parse("web").unwrap(),
                backend::sha(H3),
            )]),
            &backend::members(),
        )
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
}

#[test]
fn an_approval_racing_news_of_a_push_refuses_the_head_it_moved_from() {
    // SQLite writers never overlap.
    if std::env::var_os("PLOYZ_STORE_TEST_POSTGRES").is_none() {
        return;
    }
    let url = backend::fresh_url(&tempfile::tempdir().unwrap());
    let (store, who) = shop_on(ConfigStore::open(&url, backend::key()).unwrap());
    publish(&store, &who);
    open_as(&store, &who, &by_stranger(H2, 1));
    // News of a push to H3, not yet committed when the approval of H2 arrives.
    let mut news = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let mut held = news.transaction().unwrap();
    held.execute(
        "UPDATE config_pull_request SET facts = replace(facts, $1, $2)",
        &[&H2, &H3],
    )
    .unwrap();
    let approval = std::thread::scope(|threads| {
        let approval = threads.spawn(|| {
            store.write(
                &who,
                &ployz_store::ApprovePullRequest {
                    repository_id: backend::repo_id(11),
                    number: backend::pr_number(5),
                    head: backend::sha(H2),
                },
            )
        });
        // real-wait: the approval blocks on a Postgres row lock in another connection, which reports nothing
        std::thread::sleep(std::time::Duration::from_millis(300));
        held.commit().unwrap();
        approval.join().unwrap()
    });
    assert_eq!(approval.unwrap_err().code, RpcErrorCode::Conflict);
    let approved = approve(&store, &who, H3);
    assert_eq!(pinned(&store, &approved, "web").as_deref(), Some(H3));
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
fn a_push_ships_every_saved_change_and_builds_only_what_it_selects() {
    let (store, who) = shop();
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    set(&store, &who, "api.watchPaths", json!(["api/**"]));
    publish(&store, &who);
    let first = system(&store, &push(None, Some(H1), None));
    let first = &first.admitted[0].deployment.id;
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    store.claim(first, &runner).unwrap();
    store
        .record(
            first,
            &runner,
            ployz_store::RunEvidence::Build(ployz_store::BuildReport {
                service: ServiceName::parse("api").unwrap(),
                status: ployz_store::BuildStatus::Built,
                message: None,
                log: String::new(),
                server: None,
            }),
        )
        .unwrap();
    set(&store, &who, "api.replicas", json!(3));
    publish(&store, &who);

    let automated = system(
        &store,
        &push(Some(H1), Some(H2), Some(&["web/src/main.rs"])),
    );
    // api's Saved change ships too, on the commit it last built.
    assert_eq!(
        deployed(&store, &who, &automated),
        vec![(vec!["web".to_owned(), "api".to_owned()], 2)]
    );
    assert_eq!(pinned(&store, &automated, "web").as_deref(), Some(H2));
    assert_eq!(pinned(&store, &automated, "api").as_deref(), Some(H1));
}

#[test]
fn a_push_tears_down_a_service_a_save_removed() {
    let (store, who) = shop();
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    publish(&store, &who);
    system(&store, &push(None, Some(H1), None));
    backend::deploy(&store, &who, "production", 1);
    store
        .write(
            &who,
            &ployz_store::RemoveService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("api").unwrap(),
            },
        )
        .unwrap();
    publish(&store, &who);

    let automated = system(
        &store,
        &push(Some(H1), Some(H2), Some(&["web/src/main.rs"])),
    );
    assert_eq!(
        deployed(&store, &who, &automated),
        vec![(vec!["web".to_owned(), "api".to_owned()], 2)]
    );
}

#[test]
fn a_push_tears_down_a_service_a_save_removed_while_its_first_deploy_ran() {
    let (store, who) = shop();
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    publish(&store, &who);
    let first = system(&store, &push(None, Some(H1), None));
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    store
        .claim(&first.admitted[0].deployment.id, &runner)
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::RemoveService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("api").unwrap(),
            },
        )
        .unwrap();
    publish(&store, &who);

    let automated = system(
        &store,
        &push(Some(H1), Some(H2), Some(&["web/src/main.rs"])),
    );
    assert_eq!(
        deployed(&store, &who, &automated),
        vec![(vec!["web".to_owned(), "api".to_owned()], 2)]
    );
}

#[test]
fn a_push_ships_a_saved_config_nothing_mounts() {
    let (store, who) = shop();
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    backend::deploy(&store, &who, "production", 1);
    system(&store, &push(None, Some(H1), None));
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse("00000000-0000-4000-8000-000000000025").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ConfigName::parse("extra").unwrap(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
    publish(&store, &who);

    let automated = system(
        &store,
        &push(Some(H1), Some(H2), Some(&["web/src/main.rs"])),
    );
    let nodes: Vec<String> = store
        .read(
            &who,
            &ployz_store::DeploymentQuery {
                id: automated.admitted[0].deployment.id.clone(),
            },
        )
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| node.node.name().to_owned())
        .collect();
    assert!(nodes.contains(&"extra".to_owned()), "{nodes:?}");
}

#[test]
fn a_push_removes_a_config_a_save_deleted_while_a_deploy_shipped_it() {
    let (store, who) = shop();
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    backend::deploy(&store, &who, "production", 1);
    system(&store, &push(None, Some(H1), None));
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse("00000000-0000-4000-8000-000000000025").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ConfigName::parse("extra").unwrap(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
    publish(&store, &who);
    let shipping = system(
        &store,
        &push(Some(H1), Some(H2), Some(&["web/src/main.rs"])),
    );
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    store
        .claim(&shipping.admitted[0].deployment.id, &runner)
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::DeleteConfig {
                environment: EnvironmentRef::default(),
                config: ployz_core::ConfigName::parse("extra").unwrap(),
            },
        )
        .unwrap();
    publish(&store, &who);

    let automated = system(
        &store,
        &push(Some(H2), Some(H3), Some(&["web/src/main.rs"])),
    );
    let nodes: Vec<String> = store
        .read(
            &who,
            &ployz_store::DeploymentQuery {
                id: automated.admitted[0].deployment.id.clone(),
            },
        )
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| node.node.name().to_owned())
        .collect();
    assert!(nodes.contains(&"extra".to_owned()), "{nodes:?}");
}

#[test]
fn a_push_ships_a_config_a_save_restored_while_a_deploy_removed_it() {
    let (store, who) = shop();
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    let extra = || ployz_core::ConfigName::parse("extra").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse("00000000-0000-4000-8000-000000000025").unwrap(),
                environment: EnvironmentRef::default(),
                name: extra(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
    backend::deploy(&store, &who, "production", 1);
    let with_extra = store
        .read(
            &who,
            &ployz_store::DeploymentQuery {
                id: ployz_store::DeploymentId::parse("00000000-0000-4000-8000-000000000101")
                    .unwrap(),
            },
        )
        .unwrap()
        .deployment
        .saved;
    for admitted in system(&store, &push(None, Some(H1), None)).admitted {
        backend::run(&store, &admitted.deployment.id);
    }
    store
        .write(
            &who,
            &ployz_store::DeleteConfig {
                environment: EnvironmentRef::default(),
                config: extra(),
            },
        )
        .unwrap();
    publish(&store, &who);
    let removing = system(
        &store,
        &push(Some(H1), Some(H2), Some(&["web/src/main.rs"])),
    );
    let removing = removing.admitted[0].deployment.id.clone();
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&removing, &runner).unwrap();
    // While the removal runs, a Save brings the Config back.
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: with_extra,
                action: ployz_store::HistoryAction::Restore,
                change: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                environment: EnvironmentRef::default(),
                revision: with_extra,
                action: ployz_store::HistoryAction::Restore,
                change: None,
                version: preview.version,
                accept_overwrite: true,
            },
        )
        .unwrap();
    publish(&store, &who);
    let restoring = system(
        &store,
        &push(Some(H2), Some(H3), Some(&["web/src/main.rs"])),
    );
    let restoring = restoring.admitted[0].deployment.id.clone();
    backend::finish(&store, &removing, claimed);
    backend::run(&store, &restoring);

    let nodes: Vec<String> = store
        .read(&who, &ployz_store::DeploymentQuery { id: restoring })
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| node.node.name().to_owned())
        .collect();
    assert!(nodes.contains(&"extra".to_owned()), "{nodes:?}");
    let configs = store
        .read(&who, &ployz_store::ConfigsQuery::default())
        .unwrap()
        .configs;
    let listed = configs
        .iter()
        .find(|listed| listed.config.name == extra())
        .expect("Saved holds extra");
    assert!(listed.deployed, "{listed:?}");
    assert_eq!(listed.change, None, "{listed:?}");
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
    backend::deploy(&store, &who, "production", 1);

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
    assert_eq!(held(&store, &who).len(), 1);

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

/// Sign `generation` of an automatic deploy rule over web's and api's Saved settings, as
/// `ployz github rule approve web api` does, and keep it.
fn approve_rule(
    store: &ConfigStore,
    who: &Actor,
    generation: u64,
) -> Result<ployz_store::RuleView, ployz_core::RpcError> {
    store.write(
        who,
        &ployz_store::ApproveRule {
            environment: EnvironmentRef::default(),
            rule: signed_rule(store, who, (9, generation, "main"), false),
            births: Default::default(),
        },
    )
}

type SignedRule = Box<ployz_core::authority::Signed<ployz_core::authority::AutoDeployRule>>;

/// `generation` of rule `id` on `branch` over web's and api's settings: the latest Saved
/// revision's, or with `staged` what a Save would save.
fn signed_rule(
    store: &ConfigStore,
    who: &Actor,
    (id, generation, branch): (u8, u64, &str),
    staged: bool,
) -> SignedRule {
    use ployz_core::authority::rule::{
        BuildPolicy, Builder, PolicyCache, RuleEvent, RuleService, RuleSource, Runner, template,
    };
    use ployz_core::authority::{AutoDeployRule, RuleId, Signed, Signer, SignerKey, Timestamp};
    let view = store
        .read(
            who,
            &ployz_store::RuleQuery {
                environment: EnvironmentRef::default(),
                staged,
            },
        )
        .unwrap();
    let signer = Signer::from_seed(&[7; 32]);
    let services = ["api", "web"]
        .into_iter()
        .map(|name| {
            let spec = view
                .target
                .iter()
                .find(|spec| spec.name.as_str() == name)
                .unwrap();
            RuleService {
                name: spec.name.clone(),
                template: template(spec, &view.namespace).unwrap(),
                policy: {
                    let source = view
                        .sources
                        .iter()
                        .find(|source| source.service == spec.name)
                        .unwrap();
                    BuildPolicy::derive(
                        &source.root_dir,
                        &source.build,
                        spec,
                        ["linux/amd64".to_owned()].into(),
                        PolicyCache::Off,
                    )
                },
                secrets: None,
            }
        })
        .collect();
    let rule = AutoDeployRule {
        proposal: None,
        cluster: ployz_core::SealingPublicKey::from_bytes([3; 32]),
        rule: RuleId::from_bytes([id; 16]),
        generation,
        namespace: view.namespace,
        machines: Vec::new(),
        source: RuleSource {
            repository_id: 11,
            owner_id: 1,
            repository: "acme/web".into(),
            branch: branch.into(),
            workflow: ".github/workflows/ployz-build.yml".into(),
            events: vec![RuleEvent::Push],
            builder: Builder::pinned(),
            runner: Runner::GithubHosted,
        },
        services,
        issued_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(ployz_core::DeviceKey::from_bytes(
            *signer.public().as_bytes(),
        )),
    };
    Box::new(Signed::sign(&rule, &signer).unwrap())
}

/// Save what is staged at review `version` with `rule`, which the Save's tap signed
/// when `prior` was kept.
fn save_with_rule(
    store: &ConfigStore,
    who: &Actor,
    version: &str,
    rule: (SignedRule, Option<(u8, u64)>),
) -> Result<Written, ployz_core::RpcError> {
    save_with_rule_as(store, who, Some(version), rule, &Trusted::default())
}

/// As [`save_with_rule`], at review `version` if any, by a caller `trusted` describes.
fn save_with_rule_as(
    store: &ConfigStore,
    who: &Actor,
    version: Option<&str>,
    (rule, prior): (SignedRule, Option<(u8, u64)>),
    trusted: &Trusted,
) -> Result<Written, ployz_core::RpcError> {
    store.write_trusted(
        who,
        &Command::Publish(Publish {
            id: None,
            environment: EnvironmentRef::default(),
            version: version.map(str::to_owned),
            message: None,
            accept_volume_loss: Vec::new(),
            rule: Some(ployz_store::SavedRule {
                rule,
                births: Default::default(),
                prior: prior.map(|(id, generation)| ployz_store::RulePrior {
                    rule: ployz_core::authority::RuleId::from_bytes([id; 16]),
                    generation,
                }),
            }),
        }),
        trusted,
    )
}

fn staged_version(store: &ConfigStore, who: &Actor) -> String {
    let view: ployz_store::RuleView = store
        .read(
            who,
            &ployz_store::RuleQuery {
                environment: EnvironmentRef::default(),
                staged: true,
            },
        )
        .unwrap();
    view.version.unwrap()
}

/// The rule the Environment keeps: its ID's first byte, generation and branch.
fn kept_rule(store: &ConfigStore, who: &Actor) -> Option<(u8, u64, String)> {
    let view: ployz_store::RuleView = store
        .read(
            who,
            &ployz_store::RuleQuery {
                environment: EnvironmentRef::default(),
                staged: false,
            },
        )
        .unwrap();
    let rule = view.rule?.open().unwrap();
    Some((rule.rule.as_bytes()[0], rule.generation, rule.source.branch))
}

#[test]
fn of_two_saves_from_one_review_only_the_first_keeps_its_rule_and_its_push_deploys() {
    let (store, who) = shop();
    set(&store, &who, "web.env.GREETING", json!("hello"));
    let version = staged_version(&store, &who);
    let main = signed_rule(&store, &who, (9, 1, "main"), true);
    let dev = signed_rule(&store, &who, (8, 1, "dev"), true);

    save_with_rule(&store, &who, &version, (main, None)).unwrap();
    let late = save_with_rule(&store, &who, &version, (dev, None)).unwrap_err();

    assert_eq!(late.code, RpcErrorCode::Conflict, "{late:?}");
    assert_eq!(kept_rule(&store, &who), Some((9, 1, "main".to_owned())));
    let pushed = system(&store, &push(None, Some(H1), None));
    assert_eq!(pushed.admitted.len(), 1, "{pushed:?}");
}

#[test]
fn a_save_whose_rule_moved_since_its_tap_saves_nothing() {
    let (store, who) = shop();
    publish(&store, &who);
    set(&store, &who, "web.env.GREETING", json!("hello"));
    let version = staged_version(&store, &who);
    let tapped = signed_rule(&store, &who, (8, 1, "main"), true);
    approve_rule(&store, &who, 1).unwrap();

    let refused = save_with_rule(&store, &who, &version, (tapped, None)).unwrap_err();

    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(kept_rule(&store, &who), Some((9, 1, "main".to_owned())));
    assert_eq!(
        staged_version(&store, &who),
        version,
        "the staged change is still waiting to be saved"
    );
}

#[test]
fn a_saver_who_cant_deploy_keeps_no_rule_and_saves_nothing() {
    let (store, who) = shop();
    set(&store, &who, "web.env.GREETING", json!("hello"));
    let version = staged_version(&store, &who);
    let rule = signed_rule(&store, &who, (9, 1, "main"), true);
    let no_deploy = Trusted {
        permissions: Some(
            (ployz_store::Permission::ALL.iter().copied())
                .filter(|held| *held != ployz_store::Permission::ChangesDeploy)
                .collect(),
        ),
        ..Trusted::default()
    };

    let refused =
        save_with_rule_as(&store, &who, Some(&version), (rule, None), &no_deploy).unwrap_err();

    assert_eq!(
        refused.code,
        RpcErrorCode::from(ployz_core::FORBIDDEN_CODE),
        "{refused:?}"
    );
    assert_eq!(kept_rule(&store, &who), None);
    assert_eq!(staged_version(&store, &who), version, "nothing was saved");
}

#[test]
fn a_save_that_keeps_a_rule_without_its_review_saves_nothing_staged_after_the_tap() {
    let (store, who) = shop();
    set(&store, &who, "web.env.GREETING", json!("hello"));
    let rule = signed_rule(&store, &who, (9, 1, "main"), true);
    set(&store, &who, "web.env.GREETING", json!("goodbye"));
    let moved = staged_version(&store, &who);

    let refused =
        save_with_rule_as(&store, &who, None, (rule, None), &Trusted::default()).unwrap_err();

    assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{refused:?}");
    assert_eq!(kept_rule(&store, &who), None);
    assert_eq!(staged_version(&store, &who), moved, "nothing was saved");
}

/// End rule 9 at `generation`, as turning off Deploy every push does.
fn end_rule(store: &ConfigStore, who: &Actor, generation: u64) {
    use ployz_core::authority::{Signed, Signer};
    let mut rule = signed_rule(store, who, (9, generation, "main"), false)
        .open()
        .unwrap();
    rule.ended = true;
    store
        .write(
            who,
            &ployz_store::ApproveRule {
                environment: EnvironmentRef::default(),
                rule: Box::new(Signed::sign(&rule, &Signer::from_seed(&[7; 32])).unwrap()),
                births: Default::default(),
            },
        )
        .unwrap();
}

/// Deploy what is staged at review `version` with `rule`, which the Deploy's tap signed
/// when `prior` was kept.
fn deploy_with_rule(
    store: &ConfigStore,
    who: &Actor,
    version: &str,
    (rule, prior): (SignedRule, Option<(u8, u64)>),
) -> Result<ployz_store::DeploymentSummary, ployz_core::RpcError> {
    store.write_trusted(
        who,
        &ployz_store::Admit::Deploy(ployz_store::Deploy {
            id: ployz_store::DeploymentId::parse("00000000-0000-4000-8000-000000000141").unwrap(),
            environment: EnvironmentRef::default(),
            services: Vec::new(),
            version: Some(version.to_owned()),
            upload: None,
            accept_volume_loss: Vec::new(),
            message: None,
            prune: false,
            no_prune: false,
            versions: Vec::new(),
            copies: Vec::new(),
            recreate: Vec::new(),
            rule: Some(Box::new(ployz_store::SavedRule {
                rule,
                births: Default::default(),
                prior: prior.map(|(id, generation)| ployz_store::RulePrior {
                    rule: ployz_core::authority::RuleId::from_bytes([id; 16]),
                    generation,
                }),
            })),
        }),
        &permitted(),
    )
}

#[test]
fn a_deploy_keeps_the_rule_its_tap_signed_and_the_next_push_deploys() {
    let (store, who) = shop();
    set(&store, &who, "web.env.GREETING", json!("hello"));
    let version = staged_version(&store, &who);
    let rule = signed_rule(&store, &who, (9, 1, "main"), true);

    deploy_with_rule(&store, &who, &version, (rule, None)).unwrap();

    assert_eq!(kept_rule(&store, &who), Some((9, 1, "main".to_owned())));
    let pushed = system(&store, &push(None, Some(H1), None));
    assert_eq!(pushed.admitted.len(), 1, "{pushed:?}");
}

#[test]
fn a_persons_deploy_builds_under_the_rule_on_a_run_cloud_dispatches_until_the_rule_ends() {
    use ployz_store::GithubRule;
    let (store, who) = shop();
    set(&store, &who, "web.env.GREETING", json!("hello"));
    let version = staged_version(&store, &who);
    let rule = signed_rule(&store, &who, (9, 1, "main"), true);
    let deployed = deploy_with_rule(&store, &person(), &version, (rule, None)).unwrap();
    let pushed = system(&store, &push(None, Some(H1), None));
    let (deployed, pushed) = (
        build_id(&deployed.id, "web"),
        build_id(&pushed.admitted[0].deployment.id, "web"),
    );

    assert!(
        matches!(
            store.github_rule(&deployed).unwrap(),
            Some(GithubRule::Dispatch(_))
        ),
        "a person's Deploy builds under the rule it signed"
    );
    assert!(matches!(
        store.github_rule(&pushed).unwrap(),
        Some(GithubRule::Push(_))
    ));

    end_rule(&store, &who, 2);
    assert_eq!(store.github_rule(&deployed).unwrap(), None);
    assert!(
        matches!(
            store.github_rule(&pushed).unwrap(),
            Some(GithubRule::Push(_))
        ),
        "a push's build still names the rule, which the grant Machine refuses once ended"
    );
}

#[test]
fn a_deploy_whose_rule_was_ended_since_its_tap_deploys_nothing_and_leaves_it_ended() {
    let (store, who) = shop();
    publish(&store, &who);
    approve_rule(&store, &who, 1).unwrap();
    set(&store, &who, "web.env.GREETING", json!("hello"));
    let version = staged_version(&store, &who);
    let tapped = signed_rule(&store, &who, (9, 2, "main"), true);
    end_rule(&store, &who, 2);

    let refused = deploy_with_rule(&store, &who, &version, (tapped, Some((9, 1)))).unwrap_err();

    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(
        refused.message,
        "Automatic deploys are off for api, web. Turn on Deploy every push to deploy them."
    );
    assert_eq!(staged_version(&store, &who), version, "nothing was saved");
    let kept: ployz_store::RuleView = store
        .read(
            &who,
            &ployz_store::RuleQuery {
                environment: EnvironmentRef::default(),
                staged: false,
            },
        )
        .unwrap();
    let kept = kept.rule.unwrap().open().unwrap();
    assert!(kept.ended && kept.generation == 2, "{kept:?}");
}

/// What the plan says a person's Deploy needs for each built Service that reaches data.
fn plan(store: &ConfigStore, who: &Actor) -> ployz_store::PlanView {
    store
        .read(
            who,
            &ployz_store::PlanQuery {
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                target: true,
                deployment: None,
            },
        )
        .unwrap()
}

fn unsigned(store: &ConfigStore, who: &Actor) -> Vec<(String, ployz_store::Needs)> {
    plan(store, who)
        .unsigned
        .into_iter()
        .map(|entry| (entry.service.as_str().to_owned(), entry.needs))
        .collect()
}

#[test]
fn a_built_service_that_reaches_data_needs_a_build_its_rule_vouches_for() {
    use ployz_store::Needs;
    let (store, who) = shop();
    store
        .write(
            &who,
            &ployz_store::CreateVolume {
                restored_from: None,
                shared_writes: false,
                storage_locked: false,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: ployz_store::VolumeId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_store::VolumeName::parse("data").unwrap(),
                mounts: vec![ployz_store::Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
    assert_eq!(unsigned(&store, &who), [("web".to_owned(), Needs::Rule)]);
    publish(&store, &who);
    approve_rule(&store, &who, 1).unwrap();
    // The rule approves web as it stands, but a person's Deploy builds nothing under it.
    assert_eq!(unsigned(&store, &who), [("web".to_owned(), Needs::Rebuild)]);
    // Once a push built and deployed it, a person's Deploy of the same settings reuses
    // that build, and its Servers take the build's proof.
    let pushed = system(&store, &push(None, Some(H1), None)).admitted[0]
        .deployment
        .id
        .clone();
    // A receipt names its image by its bare content digest.
    let image = format!("sha256:{}", "d".repeat(64));
    built_on_github(&store, &build_id(&pushed, "web"), 41, "token-1", &image);
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&pushed, &runner).unwrap();
    let web = ServiceName::parse("web").unwrap();
    store
        .keep_verified(&pushed, &runner, &[(web, verified_web(1))].into())
        .unwrap();
    backend::finish(&store, &pushed, claimed);
    assert_eq!(unsigned(&store, &who), []);
    // A new generation of the rule refuses the proof the build kept under the last one.
    approve_rule(&store, &who, 2).unwrap();
    assert_eq!(unsigned(&store, &who), [("web".to_owned(), Needs::Rebuild)]);
    set(&store, &who, "web.env.GREETING", json!("hello"));
    assert_eq!(unsigned(&store, &who), [("web".to_owned(), Needs::Rule)]);
    // Once the person turns automatic deploys off, only the switch turns them on again.
    end_rule(&store, &who, 3);
    assert_eq!(unsigned(&store, &who), [("web".to_owned(), Needs::Rule)]);
    assert_eq!(
        serde_json::to_value(&plan(&store, &who).unsigned).unwrap(),
        json!([{ "service": "web", "needs": "rule", "ended": true }]),
        "a client that predates `ended` still reads the need"
    );
}

#[test]
fn a_push_under_a_rule_deploys_when_only_the_commit_changed() {
    let (store, who) = shop();
    publish(&store, &who);
    approve_rule(&store, &who, 1).unwrap();
    let automated = system(&store, &push(None, Some(H1), None));
    assert_eq!(automated.skipped, Vec::new());
    assert_eq!(automated.admitted.len(), 1);
    let claimed = store
        .claim(
            &automated.admitted[0].deployment.id,
            &ployz_store::RunnerId::parse("runner").unwrap(),
        )
        .unwrap();
    assert!(claimed.rule.is_some(), "the runner relays the rule");
    // A policy setting no Server reads changes nothing the rule approves.
    set(&store, &who, "web.waitForCi", json!(false));
    publish(&store, &who);
    let automated = system(&store, &push(Some(H1), Some(H2), None));
    assert_eq!(automated.skipped, Vec::new());
}

#[test]
fn a_push_under_a_rule_waits_for_approval_again_once_a_setting_changed() {
    let (store, who) = shop();
    publish(&store, &who);
    approve_rule(&store, &who, 1).unwrap();
    // A replica count isn't part of any one Container's settings, so it changes nothing.
    set(&store, &who, "web.replicas", json!(3));
    publish(&store, &who);
    assert_eq!(
        system(&store, &push(None, Some(H3), None)).admitted.len(),
        1
    );
    set(&store, &who, "web.env.NEW", json!("1"));
    publish(&store, &who);
    let automated = system(&store, &push(Some(H3), Some(H1), None));
    assert!(automated.admitted.is_empty());
    assert_eq!(
        reasons(&automated),
        vec![
            "web changed since the automatic deploy rule was approved. Approve it again in \
             the environment's settings, or run ployz github rule approve web."
        ]
    );
    // Approved again over the new settings, the next push deploys.
    approve_rule(&store, &who, 2).unwrap();
    let automated = system(&store, &push(Some(H1), Some(H2), None));
    assert_eq!(automated.admitted.len(), 1);
    // A build setting is in the rule's policy rather than its template, and holds too.
    set(&store, &who, "web.buildMethod", json!("dockerfile"));
    publish(&store, &who);
    let automated = system(&store, &push(Some(H2), Some(H3), None));
    assert!(automated.admitted.is_empty());
    assert_eq!(
        reasons(&automated),
        vec![
            "web changed since the automatic deploy rule was approved. Approve it again in \
             the environment's settings, or run ployz github rule approve web."
        ]
    );
}

#[test]
fn a_new_service_on_the_rules_branch_holds_the_push_and_shows_in_the_rule_view() {
    let (store, who) = shop();
    publish(&store, &who);
    approve_rule(&store, &who, 1).unwrap();
    add_git_service(&store, &who, 5, "worker");
    publish(&store, &who);
    let automated = system(&store, &push(None, Some(H1), None));
    assert!(automated.admitted.is_empty());
    assert_eq!(
        reasons(&automated),
        vec![
            "worker changed since the automatic deploy rule was approved. Approve it again in \
             the environment's settings, or run ployz github rule approve worker."
        ]
    );
    // The view the dashboard reads names what holds the push, so it can be approved.
    let view = store
        .read(
            &who,
            &ployz_store::RuleQuery {
                environment: EnvironmentRef::default(),
                staged: false,
            },
        )
        .unwrap();
    assert_eq!(view.changed, [ServiceName::parse("worker").unwrap()]);
}

#[test]
fn a_rule_that_does_not_supersede_the_kept_one_is_refused() {
    let (store, who) = shop();
    publish(&store, &who);
    approve_rule(&store, &who, 2).unwrap();
    let refused = approve_rule(&store, &who, 2).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
}

/// Run `run` of GitHub build `id` to its end, reporting `token` and the image `image`.
fn built_on_github(
    store: &ConfigStore,
    id: &ployz_store::GithubBuildId,
    run: u64,
    token: &str,
    image: &str,
) {
    store
        .github_dispatched(
            id,
            &ployz_store::GithubRun {
                run_id: run,
                run_url: format!("https://github.com/acme/web/actions/runs/{run}"),
                workflow_ref: "acme/build/.github/workflows/build.yml@refs/heads/main".into(),
                repository: backend::repo_name("acme/web"),
                installation_id: 7,
            },
        )
        .unwrap();
    let grant = ployz_store::GithubGrant {
        id: "grant".into(),
        machine: ployz_core::MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        fingerprint: "f".repeat(64),
    };
    store.github_check_in(id, run, &grant).unwrap();
    let ended = ployz_store::RunEnd::Built {
        platforms: vec!["linux/amd64".into()],
        image_token: Some(token.into()),
    };
    store
        .github_report(
            id,
            run,
            &ployz_store::GithubReport {
                from: 0,
                lines: Vec::new(),
                ended: Some(ended),
            },
        )
        .unwrap();
    let receipt = json!({"fingerprint": "f".repeat(64), "image": {"reference": image}});
    store
        .github_end(id, Some(run), &ployz_store::GithubEnd::Built { receipt })
        .unwrap();
}

/// End GitHub build `id` without a run, on the image `image` a Machine still holds.
fn reused(store: &ConfigStore, id: &ployz_store::GithubBuildId, image: &str) {
    let receipt = json!({"fingerprint": "f".repeat(64), "image": {"reference": image}});
    store
        .github_end(id, None, &ployz_store::GithubEnd::Built { receipt })
        .unwrap();
}

/// What Cloud trusts a person with every permission to do.
fn permitted() -> Trusted {
    Trusted {
        permissions: Some(ployz_store::Permission::ALL.iter().copied().collect()),
        ..Trusted::default()
    }
}

fn person() -> Actor {
    Actor {
        principal: Some(ployz_store::Principal::parse("ada").unwrap()),
        ..Actor::system(org())
    }
}

fn build_id(deployment: &ployz_store::DeploymentId, service: &str) -> ployz_store::GithubBuildId {
    ployz_store::GithubBuildId::parse(&format!("{deployment}.{service}")).unwrap()
}

/// A push of [`H1`] under rule generation 1, built on GitHub with `token` and `image`.
fn pushed_and_built(
    store: &ConfigStore,
    who: &Actor,
    token: &str,
    image: &str,
) -> ployz_store::DeploymentId {
    publish(store, who);
    approve_rule(store, who, 1).unwrap();
    let pushed = system(store, &push(None, Some(H1), None)).admitted[0]
        .deployment
        .id
        .clone();
    built_on_github(store, &build_id(&pushed, "web"), 41, token, image);
    backend::run(store, &pushed);
    pushed
}

fn redeploy(n: u8, source: &ployz_store::DeploymentId) -> ployz_store::Retry {
    ployz_store::Retry {
        id: ployz_store::DeploymentId::parse(format!("00000000-0000-4000-8000-0000000009{n:02}"))
            .unwrap(),
        deployment: source.clone(),
        rolls_back: None,
        version: None,
        accept_volume_loss: Vec::new(),
        versions: Vec::new(),
        copies: Vec::new(),
    }
}

/// A Server admits a rule-built image only with the rule and the build's token, so the
/// runner of a redeploy a person asked for gets both.
#[test]
fn a_persons_redeploy_of_a_rule_built_image_carries_the_rule_and_its_token() {
    let (store, system_actor) = shop();
    let first = format!("ployz-build/web@sha256:{}", "d".repeat(64));
    let pushed = pushed_and_built(&store, &system_actor, "token-1", &first);
    let again = redeploy(3, &pushed);
    store
        .write_trusted(
            &person(),
            &ployz_store::Admit::Retry(again.clone()),
            &permitted(),
        )
        .unwrap();
    reused(&store, &build_id(&again.id, "web"), &first);
    let claimed = store
        .claim(&again.id, &ployz_store::RunnerId::parse("runner").unwrap())
        .unwrap();
    assert!(claimed.rule.is_some());
    assert_eq!(
        claimed
            .image_tokens
            .values()
            .map(|token| token.token.as_str())
            .collect::<Vec<_>>(),
        ["token-1"]
    );
}

/// What a Server issued for a rule-built image outlives the build's token: a rollback
/// carries its source's, and a drain finds the newest for a copy's own image under the
/// Environment's rule, and none for another image.
#[test]
fn a_verified_build_rides_a_rollback_and_a_drain_of_its_image() {
    use ployz_core::authority::{BuiltImageDigest, PayloadDigest, RuleEvidence};
    let (store, who) = shop();
    let image = format!("ployz-build/web@sha256:{}", "d".repeat(64));
    publish(&store, &who);
    approve_rule(&store, &who, 1).unwrap();
    let pushed = system(&store, &push(None, Some(H1), None)).admitted[0]
        .deployment
        .id
        .clone();
    built_on_github(&store, &build_id(&pushed, "web"), 41, "token-1", &image);
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&pushed, &runner).unwrap();
    let namespace = claimed.rule.as_ref().unwrap().open().unwrap().namespace;
    let web = ServiceName::parse("web").unwrap();
    let kept = verified_web(1);
    store
        .keep_verified(&pushed, &runner, &[(web.clone(), kept.clone())].into())
        .unwrap();
    backend::finish(&store, &pushed, claimed);

    let back = redeploy(3, &pushed);
    store
        .write_trusted(
            &person(),
            &ployz_store::Admit::Retry(back.clone()),
            &permitted(),
        )
        .unwrap();
    reused(&store, &build_id(&back.id, "web"), &image);
    let claimed = store.claim(&back.id, &runner).unwrap();
    assert_eq!(claimed.verified.get(&web), Some(&kept));

    let drained = |image: &str| {
        store
            .read(
                &who,
                &ployz_store::VersionsQuery {
                    namespace: namespace.clone(),
                    service: web.clone(),
                    digest: PayloadDigest::from_bytes([0; 32]),
                    image: Some(BuiltImageDigest::parse(&format!("sha256:{image}")).unwrap()),
                },
            )
            .unwrap()
            .rule
    };
    let proof = drained(&"d".repeat(64)).expect("the copy's image has a verified build");
    assert_eq!(proof.evidence, RuleEvidence::Verified(kept.verified));
    assert_eq!(proof.claim, kept.claim);
    assert_eq!(drained(&"c".repeat(64)), None);
    // Under the rule's next generation the kept proof proves nothing, so a build's
    // fresh token for the same image goes in its place.
    approve_rule(&store, &who, 2).unwrap();
    assert_eq!(drained(&"d".repeat(64)), None);
}

/// What a Server issued for web's image `sha256:dd…` under generation `generation` of
/// [`approve_rule`]'s rule.
fn verified_web(generation: u64) -> ployz_store::VerifiedClaim {
    use ployz_core::authority::{
        AttestKey, BuildClaim, BuildStage, BuiltImageDigest, CommitAuthority, CommitSha,
        PolicyDigest, RuleId, VerifiedBuild,
    };
    ployz_store::VerifiedClaim {
        claim: BuildClaim {
            cluster: ployz_core::SealingPublicKey::from_bytes([3; 32]),
            rule: RuleId::from_bytes([9; 16]),
            generation,
            service: ServiceName::parse("web").unwrap(),
            policy: PolicyDigest::from_bytes([1; 32]),
            commit: CommitSha::parse(H1).unwrap(),
            authority: CommitAuthority::Head,
            stage: BuildStage::Built {
                image: BuiltImageDigest::parse(&format!("sha256:{}", "d".repeat(64))).unwrap(),
                attest: AttestKey::from_bytes([5; 32]),
            },
        },
        verified: VerifiedBuild::from_bytes([0x5a; 32]),
    }
}

#[test]
fn a_redeploy_reusing_its_sources_image_keeps_that_images_token() {
    use ployz_core::authority::CommitAuthority;
    let (store, system_actor) = shop();
    let first = format!("ployz-build/web@sha256:{}", "d".repeat(64));
    let pushed = pushed_and_built(&store, &system_actor, "token-1", &first);
    let again = redeploy(3, &pushed);
    store
        .write_trusted(
            &person(),
            &ployz_store::Admit::Retry(again.clone()),
            &permitted(),
        )
        .unwrap();
    let web = build_id(&again.id, "web");
    reused(&store, &web, &first);
    let token = store.github_input(&web).unwrap().image_token.unwrap();
    assert_eq!(
        (token.token.as_str(), token.authority),
        ("token-1", CommitAuthority::Head)
    );
    // Another image keeps no token: none names it.
    let other = redeploy(4, &pushed);
    store
        .write_trusted(
            &person(),
            &ployz_store::Admit::Retry(other.clone()),
            &permitted(),
        )
        .unwrap();
    let web = build_id(&other.id, "web");
    reused(
        &store,
        &web,
        &format!("ployz-build/web@sha256:{}", "c".repeat(64)),
    );
    assert_eq!(store.github_input(&web).unwrap().image_token, None);
}
