//! Façade tests for Cloud session Cluster destroy with named Data Loss.

use std::{collections::BTreeMap, path::PathBuf, process::Command, time::Duration};

use ployz::sdk;
use ployz_core::{
    ContractDescription, DataLoss, DockerVolume, DockerVolumeId, DockerVolumeName, MANAGED_LABEL,
    MachineId, MachineName, MachineObservation, MembershipObservation, PROJECT_NAME_LABEL,
    RpcErrorCode, UnconfirmedDataLoss,
};
use tokio::time::timeout;

use super::relay::{self, RelaySession};
use super::support::{DiscoveryService, confirmation, machine, native_addon};

struct ClusterLoss {
    shop_data: DockerVolumeId,
    shop_logs: DockerVolumeId,
    scratch: DockerVolumeId,
}

impl ClusterLoss {
    fn all(&self) -> Vec<DataLoss> {
        vec![
            DataLoss::DockerVolume {
                id: self.shop_data.clone(),
            },
            DataLoss::DockerVolume {
                id: self.shop_logs.clone(),
            },
            DataLoss::DockerVolume {
                id: self.scratch.clone(),
            },
        ]
    }
}

#[tokio::test]
async fn data_loss_if_cluster_destroyed_unions_project_and_machine_volumes() {
    let (client, loss, _worker, _down, _service, _session, _machine) = cluster_session().await;
    let observed = client.data_loss_if_cluster_destroyed().await.unwrap();
    assert_eq!(observed.data_loss, loss.all());
}

#[tokio::test]
async fn destroy_cluster_refuses_unconfirmed_data_loss_and_names_what_was_missing() {
    let (client, loss, _worker, _down, service, session, _machine) = cluster_session().await;
    let confirmation = confirmation(Vec::<DataLoss>::new());

    let error = client.destroy_cluster(&confirmation).await.unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    let missing: UnconfirmedDataLoss = serde_json::from_value(error.details).unwrap();
    assert_eq!(missing.missing, loss.all());
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());
    relay::register_with_pairing(&session.url, relay::PAIRING, MachineId::random())
        .await
        .expect("pairing must still authenticate before a confirmed teardown");
}

#[tokio::test]
async fn destroy_cluster_resets_machines_destroys_projects_and_revokes_pairing() {
    let (client, loss, worker, down, service, session, _machine) = cluster_session().await;
    let confirmation = confirmation(loss.all());

    let teardown = client.destroy_cluster(&confirmation).await.unwrap();
    assert_eq!(
        teardown.destroyed_projects,
        [ployz_core::ProjectName::parse("shop").unwrap()]
    );
    assert!(teardown.pairing_revoked);
    let reset = service.reset_machines.lock().unwrap().clone();
    assert!(reset.contains(&worker.id), "{reset:?}");
    assert!(
        !reset.contains(&down.id),
        "unreachable Machine must not look like a silent skip: {reset:?}"
    );
    assert!(
        teardown
            .machines
            .failures
            .iter()
            .any(|failure| failure.machine_id == down.id),
        "{teardown:?}"
    );
    assert!(
        service
            .removed_machines
            .lock()
            .unwrap()
            .contains(&worker.id)
    );
    assert!(
        service
            .removed_volumes
            .lock()
            .unwrap()
            .contains(&loss.shop_data)
    );
    assert!(
        service
            .removed_volumes
            .lock()
            .unwrap()
            .contains(&loss.shop_logs)
    );

    let error = relay::register_with_pairing(&session.url, relay::PAIRING, MachineId::random())
        .await
        .expect_err("revoked pairing must not Register");
    assert_eq!(error.status(), Some(http::StatusCode::UNAUTHORIZED));

    let again = client.destroy_cluster(&confirmation).await.unwrap();
    assert!(again.pairing_revoked, "{again:?}");
}

#[tokio::test]
async fn node_destroy_cluster_covers_teardown_and_unconfirmed_missing_names() {
    let (description, loss, worker, _down, service) = cluster_fixture();
    let session = RelaySession::start().await;
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let addon = native_addon();
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ployz-sdk");
    let script = package.join("tests/node_destroy_cluster.js");
    let url = session.url.clone();
    let entry = description.machine_id.as_str().to_owned();
    let worker_id = worker.machine.id.as_str().to_owned();
    let scratch_machine = loss.scratch.machine_id.as_str().to_owned();
    let shop_machine = loss.shop_data.machine_id.as_str().to_owned();

    let output = timeout(
        Duration::from_secs(20),
        tokio::task::spawn_blocking(move || {
            Command::new("node")
                .arg(&script)
                .env("PLOYZ_SDK_ADDON", addon)
                .env("PLOYZ_SDK_PACKAGE", package)
                .env("PLOYZ_RELAY_URL", url)
                .env("PLOYZ_BEARER", relay::DIAL)
                .env("PLOYZ_PAIRING", relay::PAIRING)
                .env("PLOYZ_MACHINE_ID", entry)
                .env("PLOYZ_WORKER_MACHINE", worker_id)
                .env("PLOYZ_SCRATCH_MACHINE_ID", scratch_machine)
                .env("PLOYZ_SHOP_MACHINE_ID", shop_machine)
                .output()
        }),
    )
    .await
    .expect("Node Cluster destroy must not hang")
    .expect("Node Cluster destroy task joins")
    .expect("Node Cluster destroy spawns");

    assert!(
        output.status.success(),
        "Node Cluster destroy failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn cluster_session() -> (
    sdk::Session,
    ClusterLoss,
    ployz_core::Machine,
    ployz_core::Machine,
    DiscoveryService,
    RelaySession,
    super::relay::FakeMachine,
) {
    let (description, loss, worker, down, service) = cluster_fixture();
    let session = RelaySession::start().await;
    let spawned = session
        .spawn_machine(description.machine_id, service.clone())
        .await;
    let client = timeout(
        Duration::from_secs(5),
        sdk::connect(
            &session.url,
            relay::DIAL,
            relay::PAIRING,
            description.machine_id.as_str(),
        ),
    )
    .await
    .expect("connect must not hang")
    .unwrap();
    (
        client,
        loss,
        worker.machine,
        down.machine,
        service,
        session,
        spawned,
    )
}

fn cluster_fixture() -> (
    ContractDescription,
    ClusterLoss,
    MachineObservation,
    MachineObservation,
    DiscoveryService,
) {
    let description = super::sdk::advertised_description();
    let mut entry = machine('a', "entry");
    entry.machine.id = description.machine_id;
    entry.machine.name = MachineName::parse("entry").unwrap();
    let worker = machine('c', "worker");
    let mut down = machine('e', "down");
    down.membership = MembershipObservation::Down;
    let loss = ClusterLoss {
        shop_data: volume_id(description.machine_id, "shop_data"),
        shop_logs: volume_id(description.machine_id, "shop_logs"),
        scratch: volume_id(worker.machine.id, "scratch"),
    };
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![entry, worker.clone(), down.clone()];
    *service.listed_volumes.lock().unwrap() = BTreeMap::from([
        (
            description.machine_id,
            vec![
                owned_volume(description.machine_id, "shop_data", "shop"),
                owned_volume(description.machine_id, "shop_logs", "shop"),
            ],
        ),
        (
            worker.machine.id,
            vec![docker_volume(worker.machine.id, "scratch")],
        ),
        (down.machine.id, vec![]),
    ]);
    (description, loss, worker, down, service)
}

fn volume_id(machine_id: MachineId, name: &str) -> DockerVolumeId {
    DockerVolumeId {
        machine_id,
        name: DockerVolumeName::parse(name).unwrap(),
    }
}

fn owned_volume(machine_id: MachineId, name: &str, project: &str) -> DockerVolume {
    DockerVolume {
        id: volume_id(machine_id, name),
        options: Default::default(),
        labels: BTreeMap::from([
            (MANAGED_LABEL.to_owned(), String::new()),
            (PROJECT_NAME_LABEL.to_owned(), project.to_owned()),
        ]),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    }
}

fn docker_volume(machine_id: MachineId, name: &str) -> DockerVolume {
    DockerVolume {
        id: volume_id(machine_id, name),
        options: Default::default(),
        labels: Default::default(),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    }
}
