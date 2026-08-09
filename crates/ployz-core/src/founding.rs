//! Pure founding contracts and policy for machine one.

use std::net::Ipv6Addr;

use serde::{Deserialize, Serialize};

use crate::corrosion::{
    ClusterDocument, CorrosionDocumentVersion, MachineDocument, MachineStorageIneligibleReason,
    MachineStorageSelection, MachineStorageSelectionReason, MachineTransport, MeshProvider,
    OperationInitiator, PeerDocument, PeerTransport, StorageMode, derive_builtin_wireguard_member,
};
use crate::ids::{ClusterName, MachineName, PeerName};
use crate::machine::MachineLifecycle;
use crate::network::MachineEndpointSubnet;

/// The exact repair primitive named when founding finds foreign host state.
pub const FOUNDING_RESET_COMMAND: &str = "ployz machine reset";

/// The plain-storage capability forfeit shown by init and diagnostics.
pub const PLAIN_STORAGE_FORFEIT: &str = "plain directories — not eligible for volume backups, instant migrations, or zero-downtime volume moves";

/// Minimum installed memory at which automatic init selection may choose ZFS.
pub const MINIMUM_ZFS_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Whether init should choose storage from host facts or honor an explicit flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InitStorageChoice {
    Automatic,
    Flag { mode: StorageMode },
}

/// Stable host facts used by the init storage eligibility gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitStorageFacts {
    pub imported_zfs_pool: bool,
    pub total_memory_bytes: u64,
}

/// Why an explicit ZFS choice is not eligible on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InitStorageSelectionError {
    #[error("ZFS storage requires an imported pool")]
    ZfsPoolNotImported,
    #[error("ZFS storage requires at least 2 GiB of RAM")]
    ZfsMemoryBelowMinimum,
}

/// Resolves machine one's durable storage selection.
pub const fn select_init_storage(
    choice: InitStorageChoice,
    facts: InitStorageFacts,
) -> Result<MachineStorageSelection, InitStorageSelectionError> {
    match choice {
        InitStorageChoice::Flag {
            mode: StorageMode::Zfs,
        } if !facts.imported_zfs_pool => Err(InitStorageSelectionError::ZfsPoolNotImported),
        InitStorageChoice::Flag {
            mode: StorageMode::Zfs,
        } if facts.total_memory_bytes < MINIMUM_ZFS_MEMORY_BYTES => {
            Err(InitStorageSelectionError::ZfsMemoryBelowMinimum)
        }
        InitStorageChoice::Flag { mode } => Ok(MachineStorageSelection {
            mode,
            reason: MachineStorageSelectionReason::Flag,
        }),
        InitStorageChoice::Automatic if !facts.imported_zfs_pool => Ok(MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Default,
        }),
        InitStorageChoice::Automatic if facts.total_memory_bytes < MINIMUM_ZFS_MEMORY_BYTES => {
            Ok(MachineStorageSelection {
                mode: StorageMode::Plain,
                reason: MachineStorageSelectionReason::Ineligible {
                    reason: MachineStorageIneligibleReason::LowRam,
                },
            })
        }
        InitStorageChoice::Automatic => Ok(MachineStorageSelection {
            mode: StorageMode::Zfs,
            reason: MachineStorageSelectionReason::Default,
        }),
    }
}

/// Driver enrollment included with the initial Corrosion rows.
///
/// The on-host form has no driver row. SSH and Cloud carry only the public
/// roster material that becomes the first peer row; callback credentials and
/// URLs are intentionally absent from this contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FoundingDriverEnrollment {
    OnHost,
    Ssh {
        peer_id: PeerName,
        document: PeerDocument,
    },
    Cloud {
        peer_id: PeerName,
        document: PeerDocument,
    },
}

impl FoundingDriverEnrollment {
    /// Returns the peer row carried by a driver-assisted founding request.
    #[must_use]
    pub const fn enrolled_peer(&self) -> Option<(&PeerName, &PeerDocument)> {
        match self {
            Self::OnHost => None,
            Self::Ssh { peer_id, document } | Self::Cloud { peer_id, document } => {
                Some((peer_id, document))
            }
        }
    }
}

/// The complete initial authority-row write submitted to the founding API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FoundingRequest {
    pub cluster_id: ClusterName,
    pub cluster: ClusterDocument,
    pub machine_id: MachineName,
    pub machine: MachineDocument,
    pub driver: FoundingDriverEnrollment,
}

impl FoundingRequest {
    /// Validates the exact machine-one row set before any row is written.
    pub fn try_validate(self) -> Result<ValidatedFoundingRequest, FoundingValidationError> {
        validate_founding_request(&self)?;
        Ok(ValidatedFoundingRequest(self))
    }
}

/// A founding request whose row identities and machine-one invariants agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedFoundingRequest(FoundingRequest);

impl ValidatedFoundingRequest {
    #[must_use]
    pub const fn request(&self) -> &FoundingRequest {
        &self.0
    }

    #[must_use]
    pub fn into_request(self) -> FoundingRequest {
        self.0
    }
}

/// One initial row named in founding validation evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum FoundingRow {
    Cluster,
    Machine,
    Peer,
}

/// Why an initial authority-row set is not safe to write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FoundingValidationError {
    #[error("cluster row key {key} disagrees with document cluster id {document_cluster_id}")]
    ClusterKeyMismatch {
        key: ClusterName,
        document_cluster_id: ClusterName,
    },
    #[error("{row:?} document belongs to cluster {found}, expected {expected}")]
    DocumentClusterMismatch {
        row: FoundingRow,
        expected: ClusterName,
        found: ClusterName,
    },
    #[error("{row:?} document version {found} is unsupported")]
    UnsupportedDocumentVersion { row: FoundingRow, found: u32 },
    #[error("founding supports builtin_wireguard, not {found:?}")]
    UnsupportedProvider { found: MeshProvider },
    #[error("machine one must use a WireGuard transport")]
    MachineTransportProviderMismatch,
    #[error("peer one must use a WireGuard transport")]
    PeerTransportProviderMismatch,
    #[error("machine one stored IPv6 address {stored} disagrees with derived address {derived}")]
    MachineAddressMismatch {
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        stored: Ipv6Addr,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        derived: Ipv6Addr,
    },
    #[error("peer one stored IPv6 address {stored} disagrees with derived address {derived}")]
    PeerAddressMismatch {
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        stored: Ipv6Addr,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        derived: Ipv6Addr,
    },
    #[error("machine one subnet {found:?} is not first subnet {expected:?}")]
    MachineSubnetNotFirst {
        expected: MachineEndpointSubnet,
        found: MachineEndpointSubnet,
    },
    #[error("machine one lifecycle must be active, not {found:?}")]
    MachineLifecycleNotActive { found: MachineLifecycle },
    #[error(
        "cluster storage default {cluster_default:?} disagrees with machine one selection {machine_mode:?}"
    )]
    StorageDefaultMismatch {
        cluster_default: StorageMode,
        machine_mode: StorageMode,
    },
    #[error("{row:?} founding provenance must be machine {expected_machine_id}, got {found:?}")]
    InvalidProvenance {
        row: FoundingRow,
        expected_machine_id: MachineName,
        found: OperationInitiator,
    },
}

fn validate_founding_request(request: &FoundingRequest) -> Result<(), FoundingValidationError> {
    if request.cluster_id != request.cluster.cluster_id {
        return Err(FoundingValidationError::ClusterKeyMismatch {
            key: request.cluster_id.clone(),
            document_cluster_id: request.cluster.cluster_id.clone(),
        });
    }
    validate_version(FoundingRow::Cluster, request.cluster.v)?;
    validate_version(FoundingRow::Machine, request.machine.v)?;
    validate_cluster_id(
        FoundingRow::Machine,
        &request.cluster_id,
        &request.machine.cluster_id,
    )?;
    if request.cluster.provider != MeshProvider::BuiltinWireguard {
        return Err(FoundingValidationError::UnsupportedProvider {
            found: request.cluster.provider,
        });
    }
    validate_provenance(
        FoundingRow::Cluster,
        &request.machine_id,
        &request.cluster.provenance.written_by,
    )?;
    validate_provenance(
        FoundingRow::Machine,
        &request.machine_id,
        &request.machine.provenance.written_by,
    )?;
    if request.machine.lifecycle != MachineLifecycle::Active {
        return Err(FoundingValidationError::MachineLifecycleNotActive {
            found: request.machine.lifecycle,
        });
    }
    if request.cluster.storage_default != request.machine.storage.mode {
        return Err(FoundingValidationError::StorageDefaultMismatch {
            cluster_default: request.cluster.storage_default,
            machine_mode: request.machine.storage.mode,
        });
    }

    let MachineTransport::Wireguard {
        pubkey,
        addr_v6,
        subnet_v4,
        ..
    } = &request.machine.transport
    else {
        return Err(FoundingValidationError::MachineTransportProviderMismatch);
    };
    let derived = derive_builtin_wireguard_member(&request.cluster_id, pubkey)
        .bind_address()
        .get();
    if *addr_v6 != derived {
        return Err(FoundingValidationError::MachineAddressMismatch {
            stored: *addr_v6,
            derived,
        });
    }
    let expected_subnet = request
        .cluster
        .prefix
        .allocate_next([])
        .expect("a valid /16 always has a first /24");
    if *subnet_v4 != expected_subnet {
        return Err(FoundingValidationError::MachineSubnetNotFirst {
            expected: expected_subnet,
            found: subnet_v4.clone(),
        });
    }

    let Some((_peer_id, peer)) = request.driver.enrolled_peer() else {
        return Ok(());
    };
    validate_version(FoundingRow::Peer, peer.v)?;
    validate_cluster_id(FoundingRow::Peer, &request.cluster_id, &peer.cluster_id)?;
    validate_provenance(
        FoundingRow::Peer,
        &request.machine_id,
        &peer.provenance.written_by,
    )?;
    let PeerTransport::Wireguard {
        pubkey, addr_v6, ..
    } = &peer.transport
    else {
        return Err(FoundingValidationError::PeerTransportProviderMismatch);
    };
    let derived = derive_builtin_wireguard_member(&request.cluster_id, pubkey)
        .bind_address()
        .get();
    if *addr_v6 != derived {
        return Err(FoundingValidationError::PeerAddressMismatch {
            stored: *addr_v6,
            derived,
        });
    }
    Ok(())
}

fn validate_version(
    row: FoundingRow,
    version: CorrosionDocumentVersion,
) -> Result<(), FoundingValidationError> {
    if version != CorrosionDocumentVersion::V1 {
        return Err(FoundingValidationError::UnsupportedDocumentVersion {
            row,
            found: version.get(),
        });
    }
    Ok(())
}

fn validate_cluster_id(
    row: FoundingRow,
    expected: &ClusterName,
    found: &ClusterName,
) -> Result<(), FoundingValidationError> {
    if found != expected {
        return Err(FoundingValidationError::DocumentClusterMismatch {
            row,
            expected: expected.clone(),
            found: found.clone(),
        });
    }
    Ok(())
}

fn validate_provenance(
    row: FoundingRow,
    expected_machine_id: &MachineName,
    found: &OperationInitiator,
) -> Result<(), FoundingValidationError> {
    if matches!(
        found,
        OperationInitiator::Machine { machine_id } if machine_id == expected_machine_id
    ) {
        return Ok(());
    }
    Err(FoundingValidationError::InvalidProvenance {
        row,
        expected_machine_id: expected_machine_id.clone(),
        found: found.clone(),
    })
}

/// Durable host state observed when the founding primitive arrives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FoundingArrival {
    Clean,
    Partial { persisted_cluster_id: ClusterName },
    Complete { persisted_cluster_id: ClusterName },
    Joined { persisted_cluster_id: ClusterName },
}

/// The successful meaning of a founding invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FoundingResult {
    Found,
    Resumed,
    NoOp,
}

/// A typed command whose only wire spelling is the exact repair primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum FoundingRepairCommand {
    #[serde(rename = "ployz machine reset")]
    ResetMachine,
}

impl FoundingRepairCommand {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResetMachine => FOUNDING_RESET_COMMAND,
        }
    }
}

/// A founding refusal that names repair without authorizing or planning it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FoundingRefusal {
    InvalidRequest {
        reason: FoundingValidationError,
    },
    IncompleteDoorMaterial {
        repair_command: FoundingRepairCommand,
    },
    ForeignState {
        requested_cluster_id: ClusterName,
        found_cluster_id: ClusterName,
        repair_command: FoundingRepairCommand,
    },
}

impl From<FoundingValidationError> for FoundingRefusal {
    fn from(reason: FoundingValidationError) -> Self {
        Self::InvalidRequest { reason }
    }
}

/// Classifies a re-run without performing or planning any host work.
pub fn classify_founding_arrival(
    requested_cluster_id: &ClusterName,
    arrival: FoundingArrival,
) -> Result<FoundingResult, FoundingRefusal> {
    match arrival {
        FoundingArrival::Clean => Ok(FoundingResult::Found),
        FoundingArrival::Partial {
            persisted_cluster_id,
        } if persisted_cluster_id == *requested_cluster_id => Ok(FoundingResult::Resumed),
        FoundingArrival::Complete {
            persisted_cluster_id,
        } if persisted_cluster_id == *requested_cluster_id => Ok(FoundingResult::NoOp),
        FoundingArrival::Partial {
            persisted_cluster_id,
        }
        | FoundingArrival::Complete {
            persisted_cluster_id,
        }
        | FoundingArrival::Joined {
            persisted_cluster_id,
        } => Err(FoundingRefusal::ForeignState {
            requested_cluster_id: requested_cluster_id.clone(),
            found_cluster_id: persisted_cluster_id,
            repair_command: FoundingRepairCommand::ResetMachine,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_storage_uses_eligibility_without_inventing_a_no_pool_failure() {
        assert_eq!(
            select_init_storage(
                InitStorageChoice::Automatic,
                InitStorageFacts {
                    imported_zfs_pool: false,
                    total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES - 1,
                },
            ),
            Ok(MachineStorageSelection {
                mode: StorageMode::Plain,
                reason: MachineStorageSelectionReason::Default,
            })
        );
        assert_eq!(
            select_init_storage(
                InitStorageChoice::Automatic,
                InitStorageFacts {
                    imported_zfs_pool: true,
                    total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES,
                },
            ),
            Ok(MachineStorageSelection {
                mode: StorageMode::Zfs,
                reason: MachineStorageSelectionReason::Default,
            })
        );
    }

    #[test]
    fn explicit_storage_is_durable_flag_evidence() {
        assert_eq!(
            select_init_storage(
                InitStorageChoice::Flag {
                    mode: StorageMode::Plain,
                },
                InitStorageFacts {
                    imported_zfs_pool: true,
                    total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES * 2,
                },
            ),
            Ok(MachineStorageSelection {
                mode: StorageMode::Plain,
                reason: MachineStorageSelectionReason::Flag,
            })
        );
    }

    #[test]
    fn explicit_zfs_is_refused_when_the_host_is_ineligible() {
        assert_eq!(
            select_init_storage(
                InitStorageChoice::Flag {
                    mode: StorageMode::Zfs,
                },
                InitStorageFacts {
                    imported_zfs_pool: false,
                    total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES,
                },
            ),
            Err(InitStorageSelectionError::ZfsPoolNotImported)
        );
        assert_eq!(
            select_init_storage(
                InitStorageChoice::Flag {
                    mode: StorageMode::Zfs,
                },
                InitStorageFacts {
                    imported_zfs_pool: true,
                    total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES - 1,
                },
            ),
            Err(InitStorageSelectionError::ZfsMemoryBelowMinimum)
        );
    }

    #[test]
    fn incomplete_door_material_refusal_names_only_the_reset_primitive() {
        let refusal = FoundingRefusal::IncompleteDoorMaterial {
            repair_command: FoundingRepairCommand::ResetMachine,
        };

        assert_eq!(
            serde_json::to_value(refusal).expect("refusal serializes"),
            serde_json::json!({
                "kind": "incomplete_door_material",
                "repair_command": FOUNDING_RESET_COMMAND,
            })
        );
    }
}
