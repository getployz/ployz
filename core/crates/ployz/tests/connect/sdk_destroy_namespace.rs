//! Façade tests for Cloud session Namespace destroy with named Data Loss.

use std::{collections::BTreeMap, time::Duration};

use ployz::deploy::VolumeFate;
use ployz::sdk;
use ployz_core::{
    ContractDescription, DataLoss, DockerVolumeId, MachineName, RpcErrorCode, UnconfirmedDataLoss,
};
use tokio::time::timeout;

use super::support::{DiscoveryService, confirmation, machine};
use super::support::{owned_volume, volume_id};
use super::unix_session::{self, UnixSession};

struct NamespaceVolumes {
    shop_data: DockerVolumeId,
    shop_logs: DockerVolumeId,
    staging_data: DockerVolumeId,
}

impl NamespaceVolumes {
    fn shop_loss(&self) -> Vec<DataLoss> {
        vec![
            DataLoss::DockerVolume {
                id: self.shop_data.clone(),
            },
            DataLoss::DockerVolume {
                id: self.shop_logs.clone(),
            },
        ]
    }

    fn union_loss(&self) -> Vec<DataLoss> {
        let mut loss = self.shop_loss();
        loss.push(DataLoss::DockerVolume {
            id: self.staging_data.clone(),
        });
        loss
    }

    fn shop_ids(&self) -> Vec<DockerVolumeId> {
        vec![self.shop_data.clone(), self.shop_logs.clone()]
    }
}

#[tokio::test]
async fn data_loss_if_namespace_destroyed_is_empty_when_volumes_are_preserved() {
    let (client, volumes, _service, _session, _machine) = namespace_session().await;
    let observed = client
        .data_loss_if_namespace_destroyed("shop", VolumeFate::Preserve)
        .await
        .unwrap();
    assert!(observed.data_loss.is_empty(), "{observed:?}");
    let destroy = client
        .data_loss_if_namespace_destroyed("shop", VolumeFate::Destroy)
        .await
        .unwrap();
    assert_eq!(destroy.data_loss, volumes.shop_loss());
}

#[tokio::test]
async fn destroy_namespace_refuses_unconfirmed_data_loss_and_names_what_was_missing() {
    let (client, volumes, service, _session, _machine) = namespace_session().await;
    let confirmation = confirmation(Vec::<DataLoss>::new());

    let error = client
        .destroy_namespace("shop", &confirmation, VolumeFate::Destroy)
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    let missing: UnconfirmedDataLoss = serde_json::from_value(error.details).unwrap();
    assert_eq!(missing.missing, volumes.shop_loss());
    assert!(service.removed_volumes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn destroy_namespace_destroys_named_volumes_after_confirmation() {
    let (client, volumes, service, _session, _machine) = namespace_session().await;
    let confirmation = confirmation(volumes.shop_loss());

    let outcome = client
        .destroy_namespace("shop", &confirmation, VolumeFate::Destroy)
        .await
        .unwrap();
    assert!(
        matches!(outcome, ployz_core::DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(*service.removed_volumes.lock().unwrap(), volumes.shop_ids());

    let leftover = client
        .data_loss_if_namespace_destroyed("staging", VolumeFate::Destroy)
        .await
        .unwrap();
    assert_eq!(
        leftover.data_loss,
        [DataLoss::DockerVolume {
            id: volumes.staging_data.clone()
        }]
    );
}

#[tokio::test]
async fn one_confirmation_covers_several_namespaces() {
    let (client, volumes, service, _session, _machine) = namespace_session().await;
    let union = confirmation(volumes.union_loss());

    client
        .destroy_namespace("shop", &union, VolumeFate::Destroy)
        .await
        .unwrap();
    client
        .destroy_namespace("staging", &union, VolumeFate::Destroy)
        .await
        .unwrap();
    let mut removed = service.removed_volumes.lock().unwrap().clone();
    removed.sort_by(|left, right| left.name.cmp(&right.name));
    let mut expected = volumes.shop_ids();
    expected.push(volumes.staging_data.clone());
    expected.sort_by(|left, right| left.name.cmp(&right.name));
    assert_eq!(removed, expected);
}

#[tokio::test]
async fn destroy_namespace_preserves_volumes_with_an_empty_confirmation() {
    let (client, volumes, service, _session, _machine) = namespace_session().await;
    let confirmation = confirmation(Vec::<DataLoss>::new());
    client
        .destroy_namespace("shop", &confirmation, VolumeFate::Preserve)
        .await
        .unwrap();
    assert!(service.removed_volumes.lock().unwrap().is_empty());
    let observed = client
        .data_loss_if_namespace_destroyed("shop", VolumeFate::Destroy)
        .await
        .unwrap();
    assert_eq!(observed.data_loss, volumes.shop_loss());
}

#[tokio::test]
async fn data_loss_if_namespace_destroyed_refuses_the_reserved_namespace() {
    let (client, _volumes, _service, _session, _machine) = namespace_session().await;
    let error = client
        .data_loss_if_namespace_destroyed("ployz-system", VolumeFate::Preserve)
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        error.message,
        "Namespace 'ployz-system' is reserved for Ployz infrastructure"
    );
}

#[tokio::test]
async fn data_loss_if_namespace_destroyed_rejects_an_invalid_namespace() {
    let (client, _volumes, _service, _session, _machine) = namespace_session().await;
    let error = client
        .data_loss_if_namespace_destroyed("BAD NAME", VolumeFate::Preserve)
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
}

#[tokio::test]
async fn destroy_namespace_refuses_the_reserved_namespace() {
    let (client, _volumes, _service, _session, _machine) = namespace_session().await;
    let confirmation = confirmation(Vec::<DataLoss>::new());
    let error = client
        .destroy_namespace("ployz-system", &confirmation, VolumeFate::Preserve)
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        error.message,
        "Namespace 'ployz-system' is reserved for Ployz infrastructure"
    );
}

#[tokio::test]
async fn node_destroy_namespace_covers_volumes_and_unconfirmed_missing_names() {
    let (description, volumes, service) = namespace_cluster();
    let session = UnixSession::start().await;
    let _machine = session.spawn_machine(description.machine_id, service).await;
    session
        .assert_sdk_script(
            "node_destroy_namespace.js",
            description.machine_id,
            &[(
                "PLOYZ_VOLUME_MACHINE_ID",
                volumes.shop_data.machine_id.as_str(),
            )],
        )
        .await;
}

async fn namespace_session() -> (
    sdk::Session,
    NamespaceVolumes,
    DiscoveryService,
    UnixSession,
    super::unix_session::FakeMachine,
) {
    let (description, volumes, service) = namespace_cluster();
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
    (client, volumes, service, session, spawned)
}

fn namespace_cluster() -> (ContractDescription, NamespaceVolumes, DiscoveryService) {
    let description = super::sdk::advertised_description();
    let mut entry = machine('a', "entry");
    entry.machine.id = description.machine_id;
    entry.machine.name = MachineName::parse("entry").unwrap();
    let volumes = NamespaceVolumes {
        shop_data: volume_id(description.machine_id, "shop_data"),
        shop_logs: volume_id(description.machine_id, "shop_logs"),
        staging_data: volume_id(description.machine_id, "staging_data"),
    };
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![entry];
    *service.listed_volumes.lock().unwrap() = BTreeMap::from([(
        description.machine_id,
        vec![
            owned_volume(description.machine_id, "shop_data", "shop"),
            owned_volume(description.machine_id, "shop_logs", "shop"),
            owned_volume(description.machine_id, "staging_data", "staging"),
        ],
    )]);
    (description, volumes, service)
}
