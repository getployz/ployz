//! Runtime and intent state records.

use serde::{Deserialize, Serialize};

use crate::dataplane::{DataplaneProjection, MachineEndpointSubnet, WireGuardPublicKey};
use crate::deploy::{ImageReference, ReplicaCount, VolumeName};
use crate::ids::{
    MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, RouteBindingId, ServiceId,
};
use crate::ingress::{
    ActiveCertificateMetadata, AutomaticHostnameConfiguration, PloyzDnsTargetIntent,
    RouteBindingOrigin,
};
use crate::machine::{IssuedJoinToken, MachineName};
use crate::nats_config::NatsAuthorizationGrant;
use crate::ops::{RoutePort, RouteTarget};
use crate::roles::InstallRolePolicy;
use std::net::{IpAddr, SocketAddr};

/// Core-owned serving-target intent value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServingTargetEntry {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub image: ImageReference,
    pub desired_replicas: ReplicaCount,
    #[serde(default)]
    pub volume_names: Vec<VolumeName>,
}

/// Core-owned route-binding intent value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RouteBindingState {
    pub id: RouteBindingId,
    pub namespace_id: NamespaceId,
    pub target: RouteTarget,
    pub endpoint_port: RoutePort,
    pub service_id: ServiceId,
    pub origin: RouteBindingOrigin,
}

/// Core-owned named-volume placement intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct VolumePinState {
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
    pub machine_id: MachineId,
}

/// Core-owned active-machine roster value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveMachineState {
    pub machine_id: MachineId,
    pub name: MachineName,
    pub activated_by: OperationId,
    #[serde(default = "InstallRolePolicy::install_all")]
    pub roles: InstallRolePolicy,
    /// Durable operator intent for this machine (Machine Lifecycle in the
    /// glossary). Absent in records written before lifecycle existed, so the
    /// default is active.
    #[serde(default)]
    pub lifecycle: MachineLifecycle,
    /// Public NATS/operator endpoints recorded from machine testimony. Promotion
    /// requires at least one of these; mesh-private addresses never become
    /// promotion authority.
    pub control_endpoints: Vec<IpAddr>,
    /// WireGuard dial candidates recorded from machine testimony. The first is
    /// programmed initially; later candidates are for endpoint rotation.
    pub mesh_endpoints: Vec<SocketAddr>,
    /// Core-owned overlay endpoint subnet allocated from cluster intent.
    pub endpoint_subnet: MachineEndpointSubnet,
    pub wireguard_public_key: WireGuardPublicKey,
}

/// Operation-owned machine identity admitted into the target dataplane projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedMachineDataplaneState {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub endpoint_subnet: MachineEndpointSubnet,
    pub mesh_endpoints: Vec<SocketAddr>,
    pub wireguard_public_key: WireGuardPublicKey,
}

/// Epoch-stamped non-secret pending machine-add recovery hints mirrored for
/// core promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingMachineJoinRecoverySnapshot {
    pub epoch: ControlPlaneEpoch,
    pub pending: Vec<PendingMachineJoinRecovery>,
}

/// Non-secret pending machine-add recovery hint mirrored for core promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingMachineJoinRecovery {
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    #[serde(default = "crate::install::HostPortAssurance::keeper")]
    pub host_port_assurance: crate::install::HostPortAssurance,
    pub endpoint_subnet: crate::dataplane::MachineEndpointSubnet,
    pub join_token: IssuedJoinToken,
}

/// Monotonic control-plane generation, advertised with intent. A machine tells a
/// promoted core (higher epoch) from a healed old one (lower epoch) by comparing
/// it; the old core is repaired by an explicit operator core replacement command
/// after a partition. Owned by the core and bumped only on operator promotion (ADR
/// 0030/0031) — NATS carries the value, it does not define it (core NATS has no
/// epoch primitive; the ones that exist live in the JetStream we exited).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ControlPlaneEpoch(u64);

impl ControlPlaneEpoch {
    /// The epoch a core mints before it has ever been promoted.
    #[must_use]
    pub const fn initial() -> Self {
        Self(1)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next generation, minted by a promotion to fence the core it succeeds
    /// (ADR 0031). `#[must_use]` because a bump only matters once persisted as the
    /// new epoch.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Full operator intent visible to readers, stamped with the epoch it reflects.
/// The NATS authorization grant set rides here too (ADR 0031): a promoted core
/// reuses it verbatim rather than re-deriving authority from the roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IntentSnapshot {
    pub epoch: ControlPlaneEpoch,
    pub core_machine_id: MachineId,
    pub active_machines: Vec<ActiveMachineState>,
    pub dataplane_projection: DataplaneProjection,
    pub route_bindings: Vec<RouteBindingState>,
    pub serving_target_entries: Vec<ServingTargetEntry>,
    #[serde(default)]
    pub volume_pins: Vec<VolumePinState>,
    pub nats_authorizations: Vec<NatsAuthorizationGrant>,
    pub automatic_hostname_configuration: AutomaticHostnameConfiguration,
    pub ployz_dns_target: PloyzDnsTargetIntent,
    pub active_certificates: Vec<ActiveCertificateMetadata>,
}

impl IntentSnapshot {
    #[must_use]
    pub fn services_referencing_volume(
        &self,
        namespace_id: &NamespaceId,
        volume_name: &VolumeName,
    ) -> Vec<ServiceId> {
        let mut services = self
            .serving_target_entries
            .iter()
            .filter(|entry| {
                &entry.namespace_id == namespace_id && entry.volume_names.contains(volume_name)
            })
            .map(|entry| entry.service_id.clone())
            .collect::<Vec<_>>();
        services.sort();
        services.dedup();
        services
    }

    /// A specific machine's advertised control endpoints, if the core recorded any.
    #[must_use]
    pub fn control_endpoints_of(&self, machine_id: &MachineId) -> Option<&[IpAddr]> {
        self.active_machines
            .iter()
            .find(|machine| &machine.machine_id == machine_id)
            .map(|machine| machine.control_endpoints.as_slice())
            .filter(|endpoints| !endpoints.is_empty())
    }
}

/// The durable operator-intent state of a current machine identity. Controls
/// placement policy; runtime readiness comes from observations, never from
/// lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineLifecycle {
    #[default]
    Active,
    Draining,
}

/// Why a machine is excluded from new workload placement for one operation.
/// Operator intent excludes durably; unavailable machine facts exclude only
/// the current operation runtime snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUsabilityReason {
    Draining,
    FactsUnavailable,
    DataplaneUnavailable { reason: DataplaneUnavailableReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DataplaneUnavailableReason {
    NotDeclared,
    TestimonyMissing,
    Admission {
        failure: crate::machine::DataplaneProjectionAdmissionFailure,
    },
}

#[must_use]
pub fn placement_rejection(lifecycle: MachineLifecycle) -> Option<MachineUsabilityReason> {
    match lifecycle {
        MachineLifecycle::Active => None,
        MachineLifecycle::Draining => Some(MachineUsabilityReason::Draining),
    }
}

/// Machine-owned endpoint facts reported with machine facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineEndpointObservation {
    pub machine_id: MachineId,
    pub control_endpoints: Vec<IpAddr>,
    pub mesh_endpoints: Vec<SocketAddr>,
}

/// Gateway role status fact reported by the gateway process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct GatewayStatusObservation {
    pub machine_id: MachineId,
    pub listen_addr: SocketAddr,
    pub serving: GatewayServingStatus,
    pub route_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum GatewayServingStatus {
    Current,
    LastKnownGood,
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nats_config::{
        CredentialGrant, CredentialName, CredentialRole, NatsAuthorizationGrant, NatsUserPublicKey,
    };

    #[test]
    fn only_draining_excludes_placement() {
        assert_eq!(placement_rejection(MachineLifecycle::Active), None);
        assert_eq!(
            placement_rejection(MachineLifecycle::Draining),
            Some(MachineUsabilityReason::Draining)
        );
    }

    #[test]
    fn intent_snapshot_round_trips_named_credential_grants() {
        let mut snapshot = ready_snapshot();
        snapshot
            .nats_authorizations
            .push(NatsAuthorizationGrant::Credential(CredentialGrant {
                public_key: NatsUserPublicKey::try_new(nkeys::KeyPair::new_user().public_key())
                    .expect("user public key"),
                name: CredentialName::try_new("Founder operator (core-1)")
                    .expect("credential name"),
                role: CredentialRole::Operator,
            }));

        let decoded = serde_json::from_value::<IntentSnapshot>(
            serde_json::to_value(&snapshot).expect("serialize intent snapshot"),
        )
        .expect("deserialize intent snapshot");

        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn intent_snapshot_requires_ingress_configuration() {
        let mut value = serde_json::to_value(ready_snapshot()).expect("serialize snapshot");
        value
            .as_object_mut()
            .expect("snapshot object")
            .remove("automatic_hostname_configuration");

        assert!(serde_json::from_value::<IntentSnapshot>(value).is_err());
    }

    #[test]
    fn intent_snapshot_round_trips_each_automatic_hostname_configuration() {
        for configuration in [
            AutomaticHostnameConfiguration::Disabled,
            AutomaticHostnameConfiguration::Ployz,
            AutomaticHostnameConfiguration::custom("apps.example.com").expect("custom suffix"),
        ] {
            let mut snapshot = ready_snapshot();
            snapshot.automatic_hostname_configuration = configuration.clone();

            let decoded = serde_json::from_value::<IntentSnapshot>(
                serde_json::to_value(snapshot).expect("serialize snapshot"),
            )
            .expect("deserialize snapshot");

            assert_eq!(decoded.automatic_hostname_configuration, configuration);
        }
    }

    fn ready_snapshot() -> IntentSnapshot {
        IntentSnapshot {
            epoch: ControlPlaneEpoch::initial(),
            core_machine_id: MachineId::try_new("core").expect("machine id"),
            active_machines: Vec::new(),
            dataplane_projection: DataplaneProjection::try_new(Vec::new(), None)
                .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            volume_pins: Vec::new(),
            nats_authorizations: Vec::new(),
            automatic_hostname_configuration: AutomaticHostnameConfiguration::Ployz,
            ployz_dns_target: PloyzDnsTargetIntent::Enabled,
            active_certificates: Vec::new(),
        }
    }
}
