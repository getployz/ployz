//! Builders for managed container identities and observations.
//!
//! Test fixtures construct these two shapes constantly; the builders default
//! every field a test does not care about (namespace `default`, `op_test` /
//! `step_test` provenance, service kind, exited state) so an identity-shape
//! change lands here once instead of in every hand-written literal.

use std::net::IpAddr;

use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerHealthStatus,
    ManagedContainerIdentity, ManagedContainerKind, ManagedContainerObservation,
};

use crate::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, operation_id, service_id,
    step_id,
};

/// Starts a [`ManagedContainerIdentity`] builder for one service.
#[must_use]
pub fn identity(service: &str) -> ManagedContainerIdentityBuilder {
    ManagedContainerIdentityBuilder {
        identity: ManagedContainerIdentity {
            namespace_id: namespace_id("default"),
            service_id: service_id(service),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_test"),
            operation_id: operation_id("op_test"),
            step_id: step_id("step_test"),
            kind: ManagedContainerKind::Service,
        },
    }
}

#[derive(Debug, Clone)]
pub struct ManagedContainerIdentityBuilder {
    identity: ManagedContainerIdentity,
}

impl ManagedContainerIdentityBuilder {
    #[must_use]
    pub fn namespace(mut self, namespace: &str) -> Self {
        self.identity.namespace_id = namespace_id(namespace);
        self
    }

    #[must_use]
    pub fn service(mut self, service: &str) -> Self {
        self.identity.service_id = service_id(service);
        self
    }

    #[must_use]
    pub fn entry(mut self, entry: &str) -> Self {
        self.identity.namespace_revision_entry_id = namespace_revision_entry_id(entry);
        self
    }

    #[must_use]
    pub fn operation(mut self, operation: &str) -> Self {
        self.identity.operation_id = operation_id(operation);
        self
    }

    #[must_use]
    pub fn step(mut self, step: &str) -> Self {
        self.identity.step_id = step_id(step);
        self
    }

    #[must_use]
    pub fn kind(mut self, kind: ManagedContainerKind) -> Self {
        self.identity.kind = kind;
        self
    }

    #[must_use]
    pub fn build(self) -> ManagedContainerIdentity {
        self.identity
    }
}

/// Starts a [`ManagedContainerObservation`] builder for one container on one
/// machine. Defaults: `svc_api` service identity, exited state.
#[must_use]
pub fn observation(machine: &str, container: &str) -> ManagedContainerObservationBuilder {
    ManagedContainerObservationBuilder {
        machine_id: machine.to_owned(),
        container_id: container.to_owned(),
        identity: identity("svc_api"),
        state: ContainerRuntimeState::Exited,
        health_status: None,
        resolved_image_identity: None,
        created_at_unix_seconds: None,
    }
}

#[derive(Debug, Clone)]
pub struct ManagedContainerObservationBuilder {
    machine_id: String,
    container_id: String,
    identity: ManagedContainerIdentityBuilder,
    state: ContainerRuntimeState,
    health_status: Option<ManagedContainerHealthStatus>,
    resolved_image_identity: Option<String>,
    created_at_unix_seconds: Option<i64>,
}

impl ManagedContainerObservationBuilder {
    /// Replaces the observation's identity with the given builder - the one
    /// identity surface, so a new identity field needs no lockstep setter
    /// here.
    #[must_use]
    pub fn with(mut self, identity: ManagedContainerIdentityBuilder) -> Self {
        self.identity = identity;
        self
    }

    #[must_use]
    pub fn state(mut self, state: ContainerRuntimeState) -> Self {
        self.state = state;
        self
    }

    #[must_use]
    pub fn running_unroutable(mut self) -> Self {
        self.state = ContainerRuntimeState::running_unroutable();
        self
    }

    #[must_use]
    pub fn running_at(mut self, ip: IpAddr) -> Self {
        self.state = ContainerRuntimeState::running_at(ip);
        self
    }

    #[must_use]
    pub fn exited(mut self) -> Self {
        self.state = ContainerRuntimeState::Exited;
        self
    }

    #[must_use]
    pub fn health_status(mut self, health_status: ManagedContainerHealthStatus) -> Self {
        self.health_status = Some(health_status);
        self
    }

    #[must_use]
    pub fn resolved_image_identity(mut self, resolved_image_identity: &str) -> Self {
        self.resolved_image_identity = Some(resolved_image_identity.to_owned());
        self
    }

    #[must_use]
    pub const fn created_at_unix_seconds(mut self, created_at_unix_seconds: i64) -> Self {
        self.created_at_unix_seconds = Some(created_at_unix_seconds);
        self
    }

    #[must_use]
    pub fn build(self) -> ManagedContainerObservation {
        ManagedContainerObservation {
            machine_id: machine_id(&self.machine_id),
            container_id: container_id(&self.container_id),
            identity: self.identity.build(),
            state: self.state,
            health_status: self.health_status,
            resolved_image_identity: self.resolved_image_identity,
            created_at_unix_seconds: self.created_at_unix_seconds,
        }
    }
}

/// Builds a machine snapshot from observation builders, panicking on the
/// machine-mismatch invariant so tests read as data.
#[must_use]
pub fn snapshot(
    machine: &str,
    observations: impl IntoIterator<Item = ManagedContainerObservationBuilder>,
) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(
        machine_id(machine),
        observations.into_iter().map(|builder| builder.build()),
    )
    .expect("snapshot fixture containers belong to the snapshot machine")
}
