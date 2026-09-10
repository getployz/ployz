//! Façade tests for Cloud session Cluster destroy with named Data Loss.

use std::{collections::BTreeMap, time::Duration};

use ployz::sdk;
use ployz_core::{
    ContractDescription, DataLoss, DockerVolumeId, MachineName, MachineObservation,
    MembershipObservation, RpcErrorCode, UnconfirmedDataLoss,
};
use tokio::time::timeout;

use super::support::{DiscoveryService, confirmation, machine};
use super::support::{docker_volume, owned_volume, volume_id};
use super::unix_session::{self, UnixSession};

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
    let (client, loss, _worker, _down, service, _session, _machine) = cluster_session().await;
    let confirmation = confirmation(Vec::<DataLoss>::new());

    let error = client.destroy_cluster(&confirmation).await.unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    let missing: UnconfirmedDataLoss = serde_json::from_value(error.details).unwrap();
    assert_eq!(missing.missing, loss.all());
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());
}

#[tokio::test]
async fn destroy_cluster_resets_machines_destroys_projects() {
    let (client, loss, worker, down, service, _session, _machine) = cluster_session().await;
    let confirmation = confirmation(loss.all());

    let teardown = client.destroy_cluster(&confirmation).await.unwrap();
    assert_eq!(
        teardown.destroyed_projects,
        [ployz_core::ProjectName::parse("shop").unwrap()]
    );

    assert!(!teardown.pairing_revoked);
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

    let again = client.destroy_cluster(&confirmation).await.unwrap();
    assert!(!again.pairing_revoked, "{again:?}");
}

#[tokio::test]
async fn node_destroy_cluster_covers_teardown_and_unconfirmed_missing_names() {
    let (description, loss, worker, _down, service) = cluster_fixture();
    let session = UnixSession::start().await;
    let _machine = session.spawn_machine(description.machine_id, service).await;
    session
        .assert_sdk_script(
            "node_destroy_cluster.js",
            description.machine_id,
            &[
                ("PLOYZ_WORKER_MACHINE", worker.machine.id.as_str()),
                ("PLOYZ_SCRATCH_MACHINE_ID", loss.scratch.machine_id.as_str()),
                ("PLOYZ_SHOP_MACHINE_ID", loss.shop_data.machine_id.as_str()),
            ],
        )
        .await;
}

async fn cluster_session() -> (
    sdk::Session,
    ClusterLoss,
    ployz_core::Machine,
    ployz_core::Machine,
    DiscoveryService,
    UnixSession,
    super::unix_session::FakeMachine,
) {
    let (description, loss, worker, down, service) = cluster_fixture();
    let session = UnixSession::start().await;
    let spawned = session
        .spawn_machine(description.machine_id, service.clone())
        .await;
    let client = timeout(
        Duration::from_secs(5),
        unix_session::connect(&session.directory, description.machine_id.as_str()),
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
