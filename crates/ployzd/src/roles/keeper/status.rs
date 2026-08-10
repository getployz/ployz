//! Machine-owned status observations and structurally local SQL writes.

use std::collections::BTreeMap;

use crate::roles::system_observation::{SystemObservation, SystemObservationError};
use ployz_core::corrosion::{
    ContainerIsolationTestimony, CorrosionDocumentVersion, CorrosionTimestamp,
    MachineStatusDocument, MeshConvergenceTestimony, SqliteParameter, Statement,
    WireGuardHandshakeEvidence,
};
use ployz_core::ids::{ClusterName, MachineName};

/// The only constructor for Keeper's machine_status UPSERT.
///
/// The row key and JSON `machine_id` are derived from the same stored value;
/// callers cannot supply a second identity that could disagree.
pub(super) struct LocalMachineStatusWriter {
    cluster_id: ClusterName,
    local_machine_id: MachineName,
    corrosion_version: String,
}

impl LocalMachineStatusWriter {
    #[must_use]
    pub(super) const fn new(
        cluster_id: ClusterName,
        local_machine_id: MachineName,
        corrosion_version: String,
    ) -> Self {
        Self {
            cluster_id,
            local_machine_id,
            corrosion_version,
        }
    }

    pub(super) fn statement(
        &self,
        mesh: Option<MeshConvergenceTestimony>,
        container_isolation: Option<ContainerIsolationTestimony>,
        wireguard_handshakes: Option<BTreeMap<MachineName, WireGuardHandshakeEvidence>>,
    ) -> Result<Statement, MachineStatusWriteError> {
        self.statement_with_observation(
            mesh,
            container_isolation,
            wireguard_handshakes,
            SystemObservation::read()?,
            CorrosionTimestamp::now_utc(),
        )
    }

    fn statement_with_observation(
        &self,
        mesh: Option<MeshConvergenceTestimony>,
        container_isolation: Option<ContainerIsolationTestimony>,
        wireguard_handshakes: Option<BTreeMap<MachineName, WireGuardHandshakeEvidence>>,
        observation: SystemObservation,
        observed_at: CorrosionTimestamp,
    ) -> Result<Statement, MachineStatusWriteError> {
        let document = MachineStatusDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.cluster_id.clone(),
            machine_id: self.local_machine_id.clone(),
            ployz_version: env!("CARGO_PKG_VERSION").to_owned(),
            corrosion_version: self.corrosion_version.clone(),
            architecture: std::env::consts::ARCH.to_owned(),
            free_disk_bytes: observation.free_disk_bytes,
            free_memory_bytes: observation.free_memory_bytes,
            load: observation.load,
            observed_at,
            mesh,
            container_isolation,
            wireguard_handshakes,
        };
        let encoded =
            serde_json::to_string(&document).map_err(|source| MachineStatusWriteError::Encode {
                detail: source.to_string(),
            })?;
        Ok(Statement::with_params(
            "INSERT INTO machine_status (machine_id, document) VALUES (?, ?) \
             ON CONFLICT(machine_id) DO UPDATE SET document = excluded.document",
            vec![
                SqliteParameter::Text(self.local_machine_id.as_str().to_owned()),
                SqliteParameter::Text(encoded),
            ],
        ))
    }
}

impl From<SystemObservationError> for MachineStatusWriteError {
    fn from(error: SystemObservationError) -> Self {
        let SystemObservationError { resource, detail } = error;
        Self::Observation { resource, detail }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum MachineStatusWriteError {
    #[error("could not observe {resource}: {detail}")]
    Observation {
        resource: &'static str,
        detail: String,
    },
    #[error("could not encode machine status document: {detail}")]
    Encode { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::corrosion::{BuiltinWireguardKeyMismatch, MachineLoadBand};
    use ployz_core::network::WireGuardPublicKey;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    fn timestamp() -> CorrosionTimestamp {
        CorrosionTimestamp::try_new("2026-08-04T12:00:00Z").expect("timestamp")
    }

    #[test]
    fn local_writer_encodes_key_mismatch_upsert_with_one_identity() {
        let writer = LocalMachineStatusWriter::new(
            ClusterName::try_new(CLUSTER).expect("cluster"),
            MachineName::try_new(MACHINE).expect("machine"),
            "0.2.0-beta.0".to_owned(),
        );
        let testimony = MeshConvergenceTestimony::KeyMismatch {
            attempted_at: timestamp(),
            mismatches: vec![BuiltinWireguardKeyMismatch::LocalPublicKey {
                machine_id: MachineName::try_new(MACHINE).expect("machine"),
                stored: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                    .expect("stored key"),
                local: WireGuardPublicKey::try_new("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=")
                    .expect("local key"),
            }],
        };
        let statement = writer
            .statement_with_observation(
                Some(testimony.clone()),
                None,
                None,
                SystemObservation {
                    free_disk_bytes: 11,
                    free_memory_bytes: 22,
                    load: MachineLoadBand::Normal,
                },
                timestamp(),
            )
            .expect("status statement");
        let Statement::WithParams(sql, params) = statement else {
            panic!("status write must be parameterized");
        };
        assert_eq!(
            sql,
            "INSERT INTO machine_status (machine_id, document) VALUES (?, ?) \
             ON CONFLICT(machine_id) DO UPDATE SET document = excluded.document"
        );
        let [SqliteParameter::Text(key), SqliteParameter::Text(document)] = params.as_slice()
        else {
            panic!("status write has one key and one document parameter");
        };
        let decoded: MachineStatusDocument =
            serde_json::from_str(document).expect("status document");
        assert_eq!(key, MACHINE);
        assert_eq!(decoded.machine_id.as_str(), key);
        assert_eq!(decoded.cluster_id.as_str(), CLUSTER);
        assert_eq!(decoded.mesh, Some(testimony));
        assert_eq!(decoded.container_isolation, None);
        assert_eq!(decoded.wireguard_handshakes, None);
    }

    #[test]
    fn local_writer_composes_both_testimony_families_in_one_upsert() {
        let writer = LocalMachineStatusWriter::new(
            ClusterName::try_new(CLUSTER).expect("cluster"),
            MachineName::try_new(MACHINE).expect("machine"),
            "0.2.0-beta.0".to_owned(),
        );
        let isolation = ContainerIsolationTestimony::Converged {
            attempted_at: timestamp(),
            last_successful_converge: timestamp(),
            entries: 2,
        };
        let statement = writer
            .statement_with_observation(
                None,
                Some(isolation.clone()),
                None,
                SystemObservation {
                    free_disk_bytes: 11,
                    free_memory_bytes: 22,
                    load: MachineLoadBand::Normal,
                },
                timestamp(),
            )
            .expect("status statement");
        let Statement::WithParams(_, params) = statement else {
            panic!("status write must be parameterized");
        };
        let [_, SqliteParameter::Text(document)] = params.as_slice() else {
            panic!("status write has key and document parameters");
        };
        let decoded: MachineStatusDocument =
            serde_json::from_str(document).expect("status document");
        assert_eq!(decoded.mesh, None);
        assert_eq!(decoded.container_isolation, Some(isolation));
    }

    #[test]
    fn local_writer_serializes_an_observed_empty_handshake_map_as_an_object() {
        let writer = LocalMachineStatusWriter::new(
            ClusterName::try_new(CLUSTER).expect("cluster"),
            MachineName::try_new(MACHINE).expect("machine"),
            "0.2.0-beta.0".to_owned(),
        );
        let statement = writer
            .statement_with_observation(
                Some(MeshConvergenceTestimony::NoRoster {
                    attempted_at: timestamp(),
                }),
                None,
                Some(BTreeMap::new()),
                SystemObservation {
                    free_disk_bytes: 11,
                    free_memory_bytes: 22,
                    load: MachineLoadBand::Normal,
                },
                timestamp(),
            )
            .expect("status statement");
        let Statement::WithParams(_, params) = statement else {
            panic!("status write must be parameterized");
        };
        let [_, SqliteParameter::Text(document)] = params.as_slice() else {
            panic!("status write has one key and one document parameter");
        };
        let encoded: serde_json::Value = serde_json::from_str(document).expect("status JSON");

        assert_eq!(
            encoded.get("wireguard_handshakes"),
            Some(&serde_json::json!({}))
        );
    }
}
