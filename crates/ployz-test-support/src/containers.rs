//! Builders for managed container identities and observations.
//!
//! Test fixtures construct these two shapes constantly; the builders default
//! every field a test does not care about (namespace `default`, `op_test` /
//! `step_test` provenance, service kind, exited state) so an identity-shape
//! change lands here once instead of in every hand-written literal.

use std::net::IpAddr;

use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerIdentity,
    ManagedContainerKind, ManagedContainerObservation,
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
    }
}

#[derive(Debug, Clone)]
pub struct ManagedContainerObservationBuilder {
    machine_id: String,
    container_id: String,
    identity: ManagedContainerIdentityBuilder,
    state: ContainerRuntimeState,
}

impl ManagedContainerObservationBuilder {
    #[must_use]
    pub fn namespace(mut self, namespace: &str) -> Self {
        self.identity = self.identity.namespace(namespace);
        self
    }

    #[must_use]
    pub fn service(mut self, service: &str) -> Self {
        let namespace = self.identity.identity.namespace_id.clone();
        let entry = self
            .identity
            .identity
            .namespace_revision_entry_id
            .clone();
        let operation = self.identity.identity.operation_id.clone();
        let step = self.identity.identity.step_id.clone();
        let kind = self.identity.identity.kind;
        self.identity = identity(service)
            .entry(entry.as_str())
            .operation(operation.as_str())
            .step(step.as_str())
            .kind(kind)
            .namespace(namespace.as_str());
        self
    }

    #[must_use]
    pub fn entry(mut self, entry: &str) -> Self {
        self.identity = self.identity.entry(entry);
        self
    }

    #[must_use]
    pub fn operation(mut self, operation: &str) -> Self {
        self.identity = self.identity.operation(operation);
        self
    }

    #[must_use]
    pub fn step(mut self, step: &str) -> Self {
        self.identity = self.identity.step(step);
        self
    }

    #[must_use]
    pub fn kind(mut self, kind: ManagedContainerKind) -> Self {
        self.identity = self.identity.kind(kind);
        self
    }

    #[must_use]
    pub fn identity(mut self, identity: ManagedContainerIdentity) -> Self {
        self.identity = ManagedContainerIdentityBuilder { identity };
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
    pub fn build(self) -> ManagedContainerObservation {
        ManagedContainerObservation {
            machine_id: machine_id(&self.machine_id),
            container_id: container_id(&self.container_id),
            identity: self.identity.build(),
            state: self.state,
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
