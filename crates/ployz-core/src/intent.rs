//! Durable operator decisions and their complete intent projection.

pub mod recovery;

use serde::{Deserialize, Serialize};

use crate::deploy::{DatasetName, ImageReference, ServiceMode, VolumeMaxSizeBytes, VolumeName};
use crate::ids::{
    MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, RouteBindingId, ServiceId,
};
use crate::ingress::{
    ActiveCertificateMetadata, AutomaticHostnameConfiguration, PloyzDnsTargetIntent,
    RouteBindingOrigin,
};
use crate::machine::roles::InstallRolePolicy;
use crate::machine::{MachineLifecycle, MachineName};
use crate::nats_config::NatsAuthorizationGrant;
use crate::network::{DataplaneProjection, MachineEndpointSubnet, WireGuardPublicKey};
use crate::operation::{RoutePort, RouteTarget};
use std::net::{IpAddr, SocketAddr};

use recovery::ControlPlaneEpoch;

/// Core-owned serving-target intent value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServingTargetEntry {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub image: ImageReference,
    pub mode: ServiceMode,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct VolumePinState {
    namespace_id: NamespaceId,
    volume_name: VolumeName,
    machine_id: MachineId,
    kind: VolumeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeKind {
    Plain,
    Provisioned {
        dataset: DatasetName,
        max_size_bytes: VolumeMaxSizeBytes,
    },
}

fn plain_volume_kind() -> VolumeKind {
    VolumeKind::Plain
}

impl VolumePinState {
    pub fn try_new(
        namespace_id: NamespaceId,
        volume_name: VolumeName,
        machine_id: MachineId,
        kind: VolumeKind,
    ) -> Result<Self, VolumePinStateError> {
        if let VolumeKind::Provisioned { dataset, .. } = &kind
            && !dataset.matches_volume(&namespace_id, &volume_name)
        {
            return Err(VolumePinStateError::DatasetIdentityMismatch {
                namespace_id,
                volume_name,
                dataset: dataset.clone(),
            });
        }
        Ok(Self {
            namespace_id,
            volume_name,
            machine_id,
            kind,
        })
    }

    #[must_use]
    pub fn plain(
        namespace_id: NamespaceId,
        volume_name: VolumeName,
        machine_id: MachineId,
    ) -> Self {
        Self {
            namespace_id,
            volume_name,
            machine_id,
            kind: VolumeKind::Plain,
        }
    }

    #[must_use]
    pub fn namespace_id(&self) -> &NamespaceId {
        &self.namespace_id
    }

    #[must_use]
    pub fn volume_name(&self) -> &VolumeName {
        &self.volume_name
    }

    #[must_use]
    pub fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    #[must_use]
    pub fn kind(&self) -> &VolumeKind {
        &self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VolumePinStateError {
    #[error(
        "dataset {} does not belong to volume {}/{}",
        .dataset.as_str(),
        .namespace_id.as_str(),
        .volume_name.as_str()
    )]
    DatasetIdentityMismatch {
        namespace_id: NamespaceId,
        volume_name: VolumeName,
        dataset: DatasetName,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VolumePinStateWire {
    namespace_id: NamespaceId,
    volume_name: VolumeName,
    machine_id: MachineId,
    #[serde(default = "plain_volume_kind")]
    kind: VolumeKind,
}

impl<'de> Deserialize<'de> for VolumePinState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = VolumePinStateWire::deserialize(deserializer)?;
        Self::try_new(
            wire.namespace_id,
            wire.volume_name,
            wire.machine_id,
            wire.kind,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// A volume pin whose variant proves an exact provisioned dataset identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProvisionedVolumePinState(VolumePinState, #[serde(skip)] DatasetName);

impl ProvisionedVolumePinState {
    pub fn try_new(volume: VolumePinState) -> Result<Self, VolumePinState> {
        let dataset = match volume.kind() {
            VolumeKind::Plain => return Err(volume),
            VolumeKind::Provisioned { dataset, .. } => dataset.clone(),
        };
        Ok(Self(volume, dataset))
    }

    #[must_use]
    pub fn volume(&self) -> &VolumePinState {
        let Self(volume, _) = self;
        volume
    }

    #[must_use]
    pub fn dataset(&self) -> &DatasetName {
        let Self(_, dataset) = self;
        dataset
    }
}

impl<'de> Deserialize<'de> for ProvisionedVolumePinState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let volume = VolumePinState::deserialize(deserializer)?;
        Self::try_new(volume)
            .map_err(|_| serde::de::Error::custom("provisioned volume pin required"))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::ZfsPoolName;
    use crate::nats_config::{
        CredentialGrant, CredentialName, CredentialRole, NatsAuthorizationGrant, NatsUserPublicKey,
    };
    use serde_json::json;

    #[test]
    fn provisioned_volume_pin_rejects_plain_volume() {
        let volume = VolumePinState::plain(
            NamespaceId::try_new("prod").expect("namespace id"),
            VolumeName::try_new("data").expect("volume name"),
            MachineId::try_new("machine_a").expect("machine id"),
        );

        assert_eq!(
            ProvisionedVolumePinState::try_new(volume.clone()),
            Err(volume)
        );
    }

    #[test]
    fn provisioned_volume_pin_exposes_exact_validated_dataset() {
        let namespace_id = NamespaceId::try_new("prod").expect("namespace id");
        let volume_name = VolumeName::try_new("data").expect("volume name");
        let dataset = DatasetName::for_volume(
            &ZfsPoolName::try_new("tank").expect("pool name"),
            &namespace_id,
            &volume_name,
        )
        .expect("dataset name");
        let volume = VolumePinState::try_new(
            namespace_id,
            volume_name,
            MachineId::try_new("machine_a").expect("machine id"),
            VolumeKind::Provisioned {
                dataset: dataset.clone(),
                max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("volume size"),
            },
        )
        .expect("valid volume pin");

        let provisioned =
            ProvisionedVolumePinState::try_new(volume.clone()).expect("provisioned pin");

        assert_eq!(provisioned.volume(), &volume);
        assert_eq!(provisioned.dataset(), &dataset);
    }

    #[test]
    fn provisioned_volume_pin_round_trips_transparent_wire_shape() {
        let value = json!({
            "namespace_id": "prod",
            "volume_name": "data",
            "machine_id": "machine_a",
            "kind": {
                "kind": "provisioned",
                "dataset": "tank/ployz/volumes/ployz-n4-prod-v4-data",
                "max_size_bytes": 1024,
            },
        });

        let provisioned = serde_json::from_value::<ProvisionedVolumePinState>(value.clone())
            .expect("provisioned pin deserializes");

        assert_eq!(
            provisioned.dataset().as_str(),
            "tank/ployz/volumes/ployz-n4-prod-v4-data"
        );
        assert_eq!(
            serde_json::to_value(provisioned).expect("provisioned pin serializes"),
            value
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
