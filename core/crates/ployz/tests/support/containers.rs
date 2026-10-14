//! Run and change one Container on a Machine through EnsureContainer.

#![allow(dead_code, reason = "each cluster test uses only some of these")]

use ployz::connect::Client;
use ployz_core::{
    ContainerId, ContainerKind, ContainerLocator, ContainerNow, ContainerWant, CreationKey, Drain,
    MachineId, Namespace, NewContainer, ResolvedServiceSpec, RpcError, RunTarget, SlotKey,
    StopOptions,
};

/// Create and start a Container in a fresh slot.
pub async fn run(
    client: &Client,
    machine_id: MachineId,
    kind: ContainerKind,
    namespace: Namespace,
    resolved_spec: ResolvedServiceSpec,
) -> Result<ContainerId, RpcError> {
    let now = ensure(
        client,
        machine_id,
        ContainerWant::Running {
            container: RunTarget::New(NewContainer {
                slot: SlotKey {
                    namespace,
                    kind,
                    creation_key: CreationKey::try_from(uuid::Uuid::new_v4().to_string()).unwrap(),
                },
                resolved_spec,
                deployment_id: None,
                registry_auth: None,
                image_prepared: false,
                answer: ployz_core::AnswerWhen::Started,
            }),
        },
    )
    .await?;
    Ok(now
        .present()
        .expect("a running Container is present")
        .container_id)
}

pub async fn start(
    client: &Client,
    machine_id: MachineId,
    container_id: ContainerId,
) -> Result<ContainerNow, RpcError> {
    ensure(
        client,
        machine_id,
        ContainerWant::Running {
            container: RunTarget::Existing(ContainerLocator::Id(container_id)),
        },
    )
    .await
}

pub async fn stop(
    client: &Client,
    machine_id: MachineId,
    container_id: ContainerId,
) -> Result<ContainerNow, RpcError> {
    ensure(
        client,
        machine_id,
        ContainerWant::Stopped {
            container: ContainerLocator::Id(container_id),
            drain: Drain::DEFAULT,
            stop: StopOptions::default(),
        },
    )
    .await
}

pub async fn remove(
    client: &Client,
    machine_id: MachineId,
    container_id: ContainerId,
) -> Result<ContainerNow, RpcError> {
    ensure(
        client,
        machine_id,
        ContainerWant::Gone {
            container: ContainerLocator::Id(container_id),
            drain: Drain::NONE,
            anonymous_volumes: ployz_core::AnonymousVolumes::Remove,
        },
    )
    .await
}

pub async fn ensure(
    client: &Client,
    machine_id: MachineId,
    want: ContainerWant,
) -> Result<ContainerNow, RpcError> {
    client.ensure_container(&machine_id, want, None).await
}
