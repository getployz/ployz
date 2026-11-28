//! Run and change one Container on a Machine through EnsureContainer.

#![allow(dead_code, reason = "each cluster test uses only some of these")]

use ployz::connect::{Carried, Client};
use ployz_core::{
    ContainerId, ContainerKind, ContainerLocator, ContainerNow, ContainerWant, CreationKey,
    DeploymentLogId, Drain, MachineId, Namespace, NewContainer, ResolvedServiceSpec, RpcError,
    RunRotation, RunTarget, SlotKey, StopOptions, op,
};

/// Create and start a Container in a fresh slot. It stays Pending, so no proxy routes it.
pub async fn run(
    client: &Client,
    machine_id: MachineId,
    kind: ContainerKind,
    namespace: Namespace,
    resolved_spec: ResolvedServiceSpec,
) -> Result<ContainerId, RpcError> {
    create(
        client,
        machine_id,
        kind,
        namespace,
        resolved_spec,
        RunRotation::Pending,
    )
    .await
}

/// The run a routed copy is held for while it is stopped, as a Deploy holds its copies.
fn held_for() -> DeploymentLogId {
    DeploymentLogId::parse("layer3-test").unwrap()
}

/// [`run`], routed once it is healthy, as a Deploy routes the copies it starts.
pub async fn run_routed(
    client: &Client,
    machine_id: MachineId,
    kind: ContainerKind,
    namespace: Namespace,
    resolved_spec: ResolvedServiceSpec,
) -> Result<ContainerId, RpcError> {
    create(
        client,
        machine_id,
        kind,
        namespace,
        resolved_spec,
        RunRotation::In {
            run: None,
            expect: None,
        },
    )
    .await
}

/// Take a routed copy out of routing, as a Deploy holds it, then stop it.
pub async fn stop_routed(
    client: &Client,
    machine_id: MachineId,
    container_id: ContainerId,
    stop: StopOptions,
) -> Result<ContainerNow, RpcError> {
    ensure(
        client,
        machine_id,
        ContainerWant::Held {
            container: ContainerLocator::Id(container_id),
            run: held_for(),
            expect: None,
        },
    )
    .await?;
    ensure(
        client,
        machine_id,
        ContainerWant::Stopped {
            container: ContainerLocator::Id(container_id),
            drain: Drain::DEFAULT,
            stop,
        },
    )
    .await
}

/// Start a copy [`stop_routed`] stopped and route it again once it is healthy.
pub async fn start_routed(
    client: &Client,
    machine_id: MachineId,
    container_id: ContainerId,
) -> Result<ContainerNow, RpcError> {
    ensure(
        client,
        machine_id,
        ContainerWant::Running {
            container: RunTarget::Existing(ContainerLocator::Id(container_id)),
            rotation: RunRotation::In {
                run: Some(held_for()),
                expect: None,
            },
        },
    )
    .await
}

/// Take a routed copy out of routing for good, as a Deploy retires it, then remove it.
pub async fn remove_routed(
    client: &Client,
    machine_id: MachineId,
    container_id: ContainerId,
) -> Result<ContainerNow, RpcError> {
    ensure(
        client,
        machine_id,
        ContainerWant::Out {
            container: ContainerLocator::Id(container_id),
            allow_empty: true,
            expect: None,
        },
    )
    .await?;
    remove(client, machine_id, container_id).await
}

async fn create(
    client: &Client,
    machine_id: MachineId,
    kind: ContainerKind,
    namespace: Namespace,
    resolved_spec: ResolvedServiceSpec,
    rotation: RunRotation,
) -> Result<ContainerId, RpcError> {
    let now = ensure(
        client,
        machine_id,
        ContainerWant::Running {
            container: RunTarget::New(NewContainer {
                r#gen: ployz_core::Gen::FIRST,
                slot: SlotKey {
                    namespace,
                    service: resolved_spec.name.clone(),
                    kind,
                    creation_key: CreationKey::try_from(uuid::Uuid::new_v4().to_string()).unwrap(),
                },
                resolved_spec,
                deployment_id: None,
                registry_auth: None,
                registry_login: None,
                image_prepared: false,
                answer: ployz_core::AnswerWhen::Started,
                image_id: None,
                replaces: None,
                free_receipt: None,
            }),
            rotation,
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
            rotation: RunRotation::Pending,
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
            anonymous_volumes: ployz_core::AnonymousVolumes::keep(),
        },
    )
    .await
}

pub async fn ensure(
    client: &Client,
    machine_id: MachineId,
    want: ContainerWant,
) -> Result<ContainerNow, RpcError> {
    client
        .ensure::<op::EnsureContainer>(&machine_id, want, async |_| Carried::default(), None)
        .await?
        .into_settled()
        .map(|(now, _)| now)
}
