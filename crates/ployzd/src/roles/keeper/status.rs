//! Machine-owned status observations and structurally local SQL writes.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use ployz_core::corrosion::{
    CorrosionDocumentVersion, CorrosionTimestamp, MachineLoadBand, MachineStatusDocument,
    MeshConvergenceTestimony, SqliteParameter, Statement, WireGuardHandshakeEvidence,
};
use ployz_core::ids::{ClusterId, MachineRowId};
use rustix::fs::statvfs;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// The only constructor for Keeper's machine_status UPSERT.
///
/// The row key and JSON `machine_id` are derived from the same stored value;
/// callers cannot supply a second identity that could disagree.
pub(super) struct LocalMachineStatusWriter {
    cluster_id: ClusterId,
    local_machine_id: MachineRowId,
    corrosion_version: String,
}

impl LocalMachineStatusWriter {
    #[must_use]
    pub(super) const fn new(
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
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
        mesh: MeshConvergenceTestimony,
        wireguard_handshakes: Option<BTreeMap<MachineRowId, WireGuardHandshakeEvidence>>,
    ) -> Result<Statement, MachineStatusWriteError> {
        self.statement_with_observation(
            mesh,
            wireguard_handshakes,
            SystemObservation::read()?,
            now()?,
        )
    }

    fn statement_with_observation(
        &self,
        mesh: MeshConvergenceTestimony,
        wireguard_handshakes: Option<BTreeMap<MachineRowId, WireGuardHandshakeEvidence>>,
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
            mesh: Some(mesh),
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

#[derive(Debug, Clone, Copy)]
struct SystemObservation {
    free_disk_bytes: u64,
    free_memory_bytes: u64,
    load: MachineLoadBand,
}

impl SystemObservation {
    fn read() -> Result<Self, MachineStatusWriteError> {
        let stat =
            statvfs(Path::new("/")).map_err(|source| MachineStatusWriteError::Observation {
                resource: "root filesystem",
                detail: source.to_string(),
            })?;
        let free_disk_bytes = bytes_from_blocks(stat.f_bavail, stat.f_frsize);
        let meminfo = fs::read_to_string("/proc/meminfo").map_err(|source| {
            MachineStatusWriteError::Observation {
                resource: "available memory",
                detail: source.to_string(),
            }
        })?;
        let free_memory_bytes = mem_available_bytes(&meminfo)?;
        let loadavg = fs::read_to_string("/proc/loadavg").map_err(|source| {
            MachineStatusWriteError::Observation {
                resource: "load average",
                detail: source.to_string(),
            }
        })?;
        let one_minute = loadavg
            .split_ascii_whitespace()
            .next()
            .ok_or_else(|| MachineStatusWriteError::Observation {
                resource: "load average",
                detail: "missing one-minute value".to_owned(),
            })?
            .parse::<f64>()
            .map_err(|source| MachineStatusWriteError::Observation {
                resource: "load average",
                detail: source.to_string(),
            })?;
        let cpu_count = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1) as f64;
        let normalized = one_minute / cpu_count;
        let load = if normalized < 0.5 {
            MachineLoadBand::Idle
        } else if normalized < 1.0 {
            MachineLoadBand::Normal
        } else {
            MachineLoadBand::Hot
        };
        Ok(Self {
            free_disk_bytes,
            free_memory_bytes,
            load,
        })
    }
}

fn mem_available_bytes(meminfo: &str) -> Result<u64, MachineStatusWriteError> {
    let Some(line) = meminfo
        .lines()
        .find(|line| line.starts_with("MemAvailable:"))
    else {
        return Err(MachineStatusWriteError::Observation {
            resource: "available memory",
            detail: "MemAvailable is absent from /proc/meminfo".to_owned(),
        });
    };
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    let [_, kibibytes, "kB"] = fields.as_slice() else {
        return Err(MachineStatusWriteError::Observation {
            resource: "available memory",
            detail: "MemAvailable has an unexpected shape".to_owned(),
        });
    };
    kibibytes
        .parse::<u64>()
        .map(|value| value.saturating_mul(1_024))
        .map_err(|source| MachineStatusWriteError::Observation {
            resource: "available memory",
            detail: source.to_string(),
        })
}

fn bytes_from_blocks(blocks: u64, block_size: u64) -> u64 {
    u64::try_from(u128::from(blocks).saturating_mul(u128::from(block_size))).unwrap_or(u64::MAX)
}

pub(super) fn now() -> Result<CorrosionTimestamp, MachineStatusWriteError> {
    let value = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|source| MachineStatusWriteError::Timestamp {
            detail: source.to_string(),
        })?;
    CorrosionTimestamp::try_new(value).map_err(|source| MachineStatusWriteError::Timestamp {
        detail: source.to_string(),
    })
}

#[derive(Debug, thiserror::Error)]
pub(super) enum MachineStatusWriteError {
    #[error("could not observe {resource}: {detail}")]
    Observation {
        resource: &'static str,
        detail: String,
    },
    #[error("could not construct machine status timestamp: {detail}")]
    Timestamp { detail: String },
    #[error("could not encode machine status document: {detail}")]
    Encode { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::corrosion::BuiltinWireguardKeyMismatch;
    use ployz_core::network::WireGuardPublicKey;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    fn timestamp() -> CorrosionTimestamp {
        CorrosionTimestamp::try_new("2026-08-04T12:00:00Z").expect("timestamp")
    }

    #[test]
    fn local_writer_encodes_key_mismatch_upsert_with_one_identity() {
        let writer = LocalMachineStatusWriter::new(
            ClusterId::try_new(CLUSTER).expect("cluster"),
            MachineRowId::try_new(MACHINE).expect("machine"),
            "0.2.0-beta.0".to_owned(),
        );
        let testimony = MeshConvergenceTestimony::KeyMismatch {
            attempted_at: timestamp(),
            mismatches: vec![BuiltinWireguardKeyMismatch::LocalPublicKey {
                machine_id: MachineRowId::try_new(MACHINE).expect("machine"),
                stored: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                    .expect("stored key"),
                local: WireGuardPublicKey::try_new("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=")
                    .expect("local key"),
            }],
        };
        let statement = writer
            .statement_with_observation(
                testimony.clone(),
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
        assert_eq!(decoded.wireguard_handshakes, None);
    }

    #[test]
    fn local_writer_serializes_an_observed_empty_handshake_map_as_an_object() {
        let writer = LocalMachineStatusWriter::new(
            ClusterId::try_new(CLUSTER).expect("cluster"),
            MachineRowId::try_new(MACHINE).expect("machine"),
            "0.2.0-beta.0".to_owned(),
        );
        let statement = writer
            .statement_with_observation(
                MeshConvergenceTestimony::NoRoster {
                    attempted_at: timestamp(),
                },
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

    #[test]
    fn reads_mem_available_as_bytes() {
        assert_eq!(
            mem_available_bytes("MemTotal: 20 kB\nMemAvailable: 12 kB\n").expect("memory"),
            12 * 1_024
        );
    }
}
