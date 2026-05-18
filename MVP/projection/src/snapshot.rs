use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use mvp_bus::IslandId;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::error::{ProjectionError, ProjectionResult};
use crate::model::{
    AcmeHttp01ChallengeProjection, DnsRecordProjection, GatewayRouteProjection, ProjectionState,
};

const EMPTY_GATEWAY_COMMIT_ID: &str = "none";
const EMPTY_ROUTE_COMMIT_ID: &str = "none";
const EMPTY_DNS_COMMIT_ID: &str = "none";

const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotWriteReport {
    pub path: PathBuf,
    pub bytes_written: usize,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSnapshotWriteReport {
    pub gateway: Option<SnapshotWriteReport>,
    pub dns: Option<SnapshotWriteReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewaySnapshotFile {
    pub schema_version: u32,
    pub island: String,
    pub revision: String,
    pub gateway_commit_id: String,
    pub route_commit_id: String,
    pub routes: Vec<GatewayRouteProjection>,
    pub acme_http01: Vec<AcmeHttp01ChallengeProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsSnapshotFile {
    pub schema_version: u32,
    pub island: String,
    pub revision: String,
    pub dns_commit_id: String,
    pub records: Vec<DnsRecordProjection>,
}

pub fn write_projection_snapshots(
    state: &ProjectionState,
    gateway_path: impl AsRef<Path>,
    dns_path: impl AsRef<Path>,
) -> ProjectionResult<ProjectionSnapshotWriteReport> {
    let operations = [
        PlannedSnapshotOperation {
            kind: SnapshotKind::Gateway,
            operation: gateway_operation_for_state(state, gateway_path.as_ref())?,
        },
        PlannedSnapshotOperation {
            kind: SnapshotKind::Dns,
            operation: dns_operation_for_state(state, dns_path.as_ref())?,
        },
    ];
    apply_snapshot_batch(&operations)
}

pub fn load_gateway_snapshot(
    path: impl AsRef<Path>,
    expected_island: &IslandId,
) -> ProjectionResult<GatewaySnapshotFile> {
    reject_symlink_target(path.as_ref())?;
    let snapshot: GatewaySnapshotFile = serde_json::from_slice(&fs::read(path.as_ref())?)?;
    validate_snapshot_header(
        path.as_ref(),
        snapshot.schema_version,
        &snapshot.island,
        expected_island,
    )?;
    Ok(snapshot)
}

pub fn load_dns_snapshot(
    path: impl AsRef<Path>,
    expected_island: &IslandId,
) -> ProjectionResult<DnsSnapshotFile> {
    reject_symlink_target(path.as_ref())?;
    let snapshot: DnsSnapshotFile = serde_json::from_slice(&fs::read(path.as_ref())?)?;
    validate_snapshot_header(
        path.as_ref(),
        snapshot.schema_version,
        &snapshot.island,
        expected_island,
    )?;
    Ok(snapshot)
}

fn gateway_snapshot_for_state(
    state: &ProjectionState,
) -> ProjectionResult<Option<GatewaySnapshotFile>> {
    if state.gateway.is_none() && state.acme_http01.is_empty() && state.dns.is_none() {
        return Ok(None);
    }
    let island = island_string(state);
    let acme_http01 = state.acme_http01.values().cloned().collect::<Vec<_>>();
    let gateway_commit_id = state.gateway.as_ref().map_or_else(
        || EMPTY_GATEWAY_COMMIT_ID.to_string(),
        |gateway| gateway.gateway_commit_id.clone(),
    );
    let route_commit_id = state.gateway.as_ref().map_or_else(
        || EMPTY_ROUTE_COMMIT_ID.to_string(),
        |gateway| gateway.route_commit_id.clone(),
    );
    let routes = state
        .gateway
        .as_ref()
        .map_or_else(Vec::new, |gateway| gateway.routes.clone());
    let revision = format!(
        "gateway:{gateway_commit_id}:{route_commit_id}:acme:{}",
        acme_revision(&acme_http01)
    );
    Ok(Some(GatewaySnapshotFile {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        island,
        revision,
        gateway_commit_id,
        route_commit_id,
        routes,
        acme_http01,
    }))
}

fn dns_snapshot_for_state(state: &ProjectionState) -> ProjectionResult<Option<DnsSnapshotFile>> {
    if state.dns.is_none() && state.gateway.is_none() && state.acme_http01.is_empty() {
        return Ok(None);
    }
    let island = island_string(state);
    let dns_commit_id = state.dns.as_ref().map_or_else(
        || EMPTY_DNS_COMMIT_ID.to_string(),
        |dns| dns.dns_commit_id.clone(),
    );
    let records = state
        .dns
        .as_ref()
        .map_or_else(Vec::new, |dns| dns.records.clone());
    let revision = format!("dns:{dns_commit_id}");
    Ok(Some(DnsSnapshotFile {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        island,
        revision,
        dns_commit_id,
        records,
    }))
}

fn gateway_operation_for_state(
    state: &ProjectionState,
    path: &Path,
) -> ProjectionResult<SnapshotOperation> {
    match gateway_snapshot_for_state(state)? {
        Some(snapshot) => {
            SnapshotOperation::write(path, serde_json::to_vec(&snapshot)?, snapshot.revision)
        }
        None if state.statuses.is_empty() => Ok(SnapshotOperation::Remove {
            path: path.to_path_buf(),
        }),
        None => Ok(SnapshotOperation::Preserve),
    }
}

fn dns_operation_for_state(
    state: &ProjectionState,
    path: &Path,
) -> ProjectionResult<SnapshotOperation> {
    match dns_snapshot_for_state(state)? {
        Some(snapshot) => {
            SnapshotOperation::write(path, serde_json::to_vec(&snapshot)?, snapshot.revision)
        }
        None if state.statuses.is_empty() => Ok(SnapshotOperation::Remove {
            path: path.to_path_buf(),
        }),
        None => Ok(SnapshotOperation::Preserve),
    }
}

fn island_string(state: &ProjectionState) -> String {
    state.island.as_str().to_string()
}

fn acme_revision(challenges: &[AcmeHttp01ChallengeProjection]) -> String {
    if challenges.is_empty() {
        return "none".to_string();
    }
    challenges
        .iter()
        .map(|challenge| {
            format!(
                "{}:{}:{}:{}:{}:{}",
                challenge.hostname.as_str(),
                challenge.token.as_str(),
                challenge.lease_epoch.value(),
                challenge.claim_hash.as_hex(),
                challenge.key_authorization.as_str(),
                challenge.expires_at.value()
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[derive(Debug, Clone, Copy)]
enum SnapshotKind {
    Gateway,
    Dns,
}

#[derive(Debug, Clone)]
struct PlannedSnapshotOperation {
    kind: SnapshotKind,
    operation: SnapshotOperation,
}

#[derive(Debug, Clone)]
enum SnapshotOperation {
    Write {
        path: PathBuf,
        bytes: Vec<u8>,
        revision: String,
    },
    Remove {
        path: PathBuf,
    },
    Preserve,
}

impl SnapshotOperation {
    fn write(path: &Path, bytes: Vec<u8>, revision: String) -> ProjectionResult<Self> {
        Ok(Self::Write {
            path: path.to_path_buf(),
            bytes,
            revision,
        })
    }

    fn path(&self) -> Option<&Path> {
        match self {
            Self::Write { path, .. } | Self::Remove { path } => Some(path),
            Self::Preserve => None,
        }
    }
}

fn apply_snapshot_batch(
    operations: &[PlannedSnapshotOperation],
) -> ProjectionResult<ProjectionSnapshotWriteReport> {
    let mut report = ProjectionSnapshotWriteReport {
        gateway: None,
        dns: None,
    };
    let mut applied = Vec::new();

    for planned in operations {
        let Some(path) = planned.operation.path() else {
            continue;
        };
        let backup = match backup_snapshot(path) {
            Ok(backup) => backup,
            Err(error) => {
                rollback_snapshots(&applied)?;
                return Err(error);
            }
        };
        match apply_snapshot_operation(&planned.operation) {
            Ok(write_report) => {
                applied.push(backup);
                match planned.kind {
                    SnapshotKind::Gateway => report.gateway = write_report,
                    SnapshotKind::Dns => report.dns = write_report,
                }
            }
            Err(error) => {
                if let Err(rollback_error) = rollback_snapshots(&applied) {
                    return Err(ProjectionError::SnapshotPath {
                        path: path.to_path_buf(),
                        reason: format!(
                            "snapshot batch failed ({error}); rollback failed ({rollback_error})"
                        ),
                    });
                }
                return Err(error);
            }
        }
    }

    Ok(report)
}

fn apply_snapshot_operation(
    operation: &SnapshotOperation,
) -> ProjectionResult<Option<SnapshotWriteReport>> {
    match operation {
        SnapshotOperation::Write {
            path,
            bytes,
            revision,
        } => write_snapshot_bytes(path, bytes, revision.clone()).map(Some),
        SnapshotOperation::Remove { path } => {
            remove_snapshot_file(path)?;
            Ok(None)
        }
        SnapshotOperation::Preserve => Ok(None),
    }
}

#[derive(Debug, Clone)]
struct SnapshotBackup {
    path: PathBuf,
    previous: PreviousSnapshot,
}

#[derive(Debug, Clone)]
enum PreviousSnapshot {
    Present(Vec<u8>),
    Absent,
}

fn backup_snapshot(path: &Path) -> ProjectionResult<SnapshotBackup> {
    reject_symlink_target(path)?;
    let previous = match fs::read(path) {
        Ok(bytes) => PreviousSnapshot::Present(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PreviousSnapshot::Absent,
        Err(error) => return Err(error.into()),
    };
    Ok(SnapshotBackup {
        path: path.to_path_buf(),
        previous,
    })
}

fn rollback_snapshots(backups: &[SnapshotBackup]) -> ProjectionResult<()> {
    for backup in backups.iter().rev() {
        match &backup.previous {
            PreviousSnapshot::Present(bytes) => {
                write_snapshot_bytes(&backup.path, bytes, "rollback".to_string())?;
            }
            PreviousSnapshot::Absent => {
                remove_snapshot_file(&backup.path)?;
            }
        }
    }
    Ok(())
}

fn write_snapshot_bytes(
    path: &Path,
    bytes: &[u8],
    revision: String,
) -> ProjectionResult<SnapshotWriteReport> {
    reject_symlink_target(path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut file = NamedTempFile::new_in(parent)?;
    set_restrictive_file_mode(file.as_file())?;
    file.write_all(bytes)?;
    file.as_file_mut().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(SnapshotWriteReport {
        path: path.to_path_buf(),
        bytes_written: bytes.len(),
        revision,
    })
}

fn remove_snapshot_file(path: &Path) -> ProjectionResult<()> {
    reject_symlink_target(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn reject_symlink_target(path: &Path) -> ProjectionResult<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(ProjectionError::SnapshotPath {
            path: path.to_path_buf(),
            reason: "snapshot target is a symlink".to_string(),
        });
    }
    Ok(())
}

fn validate_snapshot_header(
    path: &Path,
    schema_version: u32,
    island: &str,
    expected_island: &IslandId,
) -> ProjectionResult<()> {
    if schema_version != SNAPSHOT_SCHEMA_VERSION {
        return Err(ProjectionError::SnapshotPath {
            path: path.to_path_buf(),
            reason: format!("unsupported schema version {schema_version}"),
        });
    }
    if island != expected_island.as_str() {
        return Err(ProjectionError::SnapshotPath {
            path: path.to_path_buf(),
            reason: format!("snapshot island {island} does not match {expected_island}"),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn set_restrictive_file_mode(file: &fs::File) -> ProjectionResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = file.metadata()?.permissions();
    permissions.set_mode(0o600);
    file.set_permissions(permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_restrictive_file_mode(_file: &fs::File) -> ProjectionResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use mvp_bus::IslandId;

    use crate::error::ProjectionError;
    use crate::facts::{BackendEndpoint, DnsRecordFact, RouteId};
    use crate::model::{
        DnsProjection, DnsRecordProjection, GatewayProjection, GatewayRouteProjection,
        ProjectionIgnoreReason, ProjectionState, ProjectionStatus,
    };
    use crate::snapshot::{load_dns_snapshot, load_gateway_snapshot, write_projection_snapshots};
    use mvp_identity::NodeId;

    fn sample_state() -> ProjectionState {
        let mut state = ProjectionState::for_island(IslandId::new("prod"));
        let backend = BackendEndpoint {
            node_id: NodeId::new("node-1"),
            address: "10.0.0.1:8080".to_string(),
        };
        state.gateway = Some(GatewayProjection {
            gateway_commit_id: "gateway-1".to_string(),
            route_commit_id: "route-1".to_string(),
            routes: vec![GatewayRouteProjection {
                route_id: RouteId::new("web-http"),
                hostnames: vec!["web.example.com".to_string()],
                backends: vec![backend],
                old_backends_to_drain: Vec::new(),
            }],
        });
        state.dns = Some(DnsProjection {
            dns_commit_id: "dns-1".to_string(),
            records: vec![DnsRecordProjection::from(DnsRecordFact {
                name: "web.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::1".to_string(),
                ttl_seconds: 30,
            })],
        });
        state
    }

    #[test]
    fn snapshot_writes_are_deterministic_and_loadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = sample_state();
        let gateway_path = dir.path().join("gateway.snapshot");
        let dns_path = dir.path().join("dns.snapshot");

        write_projection_snapshots(&state, &gateway_path, &dns_path).expect("write snapshots");
        let first_gateway = fs::read(&gateway_path).expect("read gateway snapshot");
        write_projection_snapshots(&state, &gateway_path, &dns_path).expect("rewrite snapshots");
        let second_gateway = fs::read(&gateway_path).expect("read gateway snapshot");

        assert_eq!(first_gateway, second_gateway);
        assert_eq!(
            load_gateway_snapshot(&gateway_path, &IslandId::new("prod"))
                .expect("load gateway")
                .routes
                .len(),
            1
        );
        assert_eq!(
            load_dns_snapshot(&dns_path, &IslandId::new("prod"))
                .expect("load dns")
                .records
                .len(),
            1
        );
    }

    #[test]
    fn loader_rejects_corrupt_and_wrong_island_snapshots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = sample_state();
        let path = dir.path().join("gateway.snapshot");
        write_projection_snapshots(&state, &path, dir.path().join("dns.snapshot"))
            .expect("write snapshots");
        assert!(load_gateway_snapshot(&path, &IslandId::new("laptop")).is_err());

        fs::write(&path, b"not-json").expect("write corrupt snapshot");
        assert!(load_gateway_snapshot(&path, &IslandId::new("prod")).is_err());
    }

    #[test]
    fn empty_projection_removes_stale_snapshots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = sample_state();
        let empty = ProjectionState::for_island(IslandId::new("prod"));
        let gateway_path = dir.path().join("gateway.snapshot");
        let dns_path = dir.path().join("dns.snapshot");

        write_projection_snapshots(&state, &gateway_path, &dns_path).expect("write snapshots");
        let report = write_projection_snapshots(&empty, &gateway_path, &dns_path)
            .expect("remove stale snapshots");

        assert!(report.gateway.is_none());
        assert!(report.dns.is_none());
        assert!(!gateway_path.exists());
        assert!(!dns_path.exists());
    }

    #[test]
    fn degraded_projection_preserves_last_good_snapshot_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = sample_state();
        let mut degraded = ProjectionState::for_island(IslandId::new("prod"));
        degraded.statuses.push(ProjectionStatus {
            reason: ProjectionIgnoreReason::MissingPayload,
            count: 1,
        });
        let gateway_path = dir.path().join("gateway.snapshot");
        let dns_path = dir.path().join("dns.snapshot");
        write_projection_snapshots(&state, &gateway_path, &dns_path).expect("write snapshots");
        let gateway_before = fs::read(&gateway_path).expect("read gateway");
        let dns_before = fs::read(&dns_path).expect("read dns");

        let report = write_projection_snapshots(&degraded, &gateway_path, &dns_path)
            .expect("preserve degraded snapshots");

        assert!(report.gateway.is_none());
        assert!(report.dns.is_none());
        assert_eq!(
            fs::read(&gateway_path).expect("read gateway"),
            gateway_before
        );
        assert_eq!(fs::read(&dns_path).expect("read dns"), dns_before);
    }

    #[test]
    fn projection_snapshot_batch_rolls_back_gateway_when_dns_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = sample_state();
        let mut next = sample_state();
        let Some(gateway) = next.gateway.as_mut() else {
            panic!("sample state has gateway");
        };
        gateway.gateway_commit_id = "gateway-2".to_string();
        gateway.route_commit_id = "route-2".to_string();
        let gateway_path = dir.path().join("gateway.snapshot");
        let dns_path = dir.path().join("dns.snapshot");
        write_projection_snapshots(&state, &gateway_path, &dns_path).expect("write snapshots");
        let gateway_before = fs::read(&gateway_path).expect("read gateway");
        let bad_parent = dir.path().join("not-a-directory");
        fs::write(&bad_parent, b"file").expect("write blocker");
        let bad_dns_path = bad_parent.join("dns.snapshot");

        let error = write_projection_snapshots(&next, &gateway_path, &bad_dns_path).unwrap_err();

        assert!(matches!(error, ProjectionError::Io(_)));
        assert_eq!(
            fs::read(&gateway_path).expect("read gateway"),
            gateway_before
        );
    }

    #[test]
    fn writer_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = sample_state();
        let root = dir.path().join("snapshots");
        let path = root.join("gateway.snapshot");

        write_projection_snapshots(&state, &path, root.join("dns.snapshot"))
            .expect("write snapshots");

        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn writer_rejects_symlink_targets() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let state = sample_state();
        let target = dir.path().join("target.snapshot");
        let link = dir.path().join("gateway.snapshot");
        fs::write(&target, b"old").expect("write target");
        symlink(&target, &link).expect("create symlink");

        assert!(
            write_projection_snapshots(&state, &link, dir.path().join("dns.snapshot")).is_err()
        );
        assert_eq!(fs::read(&target).expect("read target"), b"old");
    }

    #[cfg(unix)]
    #[test]
    fn loader_rejects_symlink_targets() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let state = sample_state();
        let target = dir.path().join("target.snapshot");
        let link = dir.path().join("gateway.snapshot");
        write_projection_snapshots(&state, &target, dir.path().join("dns.snapshot"))
            .expect("write snapshots");
        symlink(&target, &link).expect("create symlink");

        assert!(load_gateway_snapshot(&link, &IslandId::new("prod")).is_err());
    }
}
