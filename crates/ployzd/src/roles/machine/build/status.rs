use crate::adapters::atomic_file::write_secret_file_atomically;
use ployz_core::build::{
    BuildAdapter, BuildExecutorAcceptance, BuildExecutorAssignment, BuildExecutorStartRequest,
    BuildExecutorStatus, BuildSourceEvidence,
};
use ployz_core::ids::OperationId;
use ployz_core::image::OciPlatform;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

const RECORD_VERSION: u8 = 1;
const TERMINAL_RECORD_LIMIT: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BuildStatusRecord {
    version: u8,
    pub(super) commitment: String,
    pub(super) acceptance: BuildExecutorAcceptance,
    pub(super) status: BuildExecutorStatus,
}

impl BuildStatusRecord {
    pub(super) fn new(
        commitment: String,
        acceptance: BuildExecutorAcceptance,
        status: BuildExecutorStatus,
    ) -> Self {
        Self {
            version: RECORD_VERSION,
            commitment,
            acceptance,
            status,
        }
    }

    fn validate(&self, key: &OperationId) -> Result<(), String> {
        if self.version != RECORD_VERSION {
            return Err(format!("unsupported build status version {}", self.version));
        }
        if self.acceptance.operation_id != *key
            || status_acceptance(&self.status) != &self.acceptance
        {
            return Err("build status evidence identity does not match its file key".to_owned());
        }
        if self.commitment.len() != 64
            || !self.commitment.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("build status evidence has an invalid request commitment".to_owned());
        }
        Ok(())
    }
}

fn status_acceptance(status: &BuildExecutorStatus) -> &BuildExecutorAcceptance {
    match status {
        BuildExecutorStatus::Running { acceptance, .. }
        | BuildExecutorStatus::Failed { acceptance, .. }
        | BuildExecutorStatus::Cancelled { acceptance, .. } => acceptance,
        BuildExecutorStatus::Completed { result } => &result.acceptance,
    }
}

#[derive(Clone)]
pub(super) struct BuildStatusRepository {
    root: PathBuf,
}

impl BuildStatusRepository {
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn read(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<BuildStatusRecord>, String> {
        let path = self.path(operation_id);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("read {}: {error}", path.display())),
        };
        let record: BuildStatusRecord = serde_json::from_slice(&bytes)
            .map_err(|error| format!("decode {}: {error}", path.display()))?;
        record.validate(operation_id)?;
        Ok(Some(record))
    }

    pub(super) fn write(&self, record: &BuildStatusRecord) -> Result<(), String> {
        record.validate(&record.acceptance.operation_id)?;
        if !matches!(record.status, BuildExecutorStatus::Running { .. }) {
            self.reserve_terminal_capacity(&record.acceptance.operation_id)?;
        }
        let bytes = serde_json::to_vec(record)
            .map_err(|error| format!("encode build status evidence: {error}"))?;
        write_secret_file_atomically(&self.path(&record.acceptance.operation_id), &bytes)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(super) fn records(&self) -> Result<Vec<BuildStatusRecord>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("read {}: {error}", self.root.display())),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| format!("read build status entry: {error}"))?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let stem = entry
                .path()
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| "build status file has no UTF-8 key".to_owned())?
                .to_owned();
            let operation_id = OperationId::try_new(stem)
                .map_err(|error| format!("invalid build status file key: {error}"))?;
            let Some(record) = self.read(&operation_id)? else {
                return Err("build status evidence disappeared while recovering".to_owned());
            };
            records.push(record);
        }
        Ok(records)
    }

    fn path(&self, operation_id: &OperationId) -> PathBuf {
        self.root.join(format!("{}.json", operation_id.as_str()))
    }

    fn reserve_terminal_capacity(&self, replacing: &OperationId) -> Result<(), String> {
        let mut terminal = Vec::new();
        for record in self.records()? {
            if record.acceptance.operation_id != *replacing
                && !matches!(record.status, BuildExecutorStatus::Running { .. })
            {
                let path = self.path(&record.acceptance.operation_id);
                let modified = std::fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .map_err(|error| format!("inspect {}: {error}", path.display()))?;
                terminal.push((modified, path));
            }
        }
        terminal.sort_by_key(|(modified, _)| *modified);
        let remove = terminal
            .len()
            .saturating_add(1)
            .saturating_sub(TERMINAL_RECORD_LIMIT);
        for (_, path) in terminal.into_iter().take(remove) {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove {}: {error}", path.display()))?;
        }
        if remove > 0 {
            std::fs::File::open(&self.root)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("sync {}: {error}", self.root.display()))?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct Commitment<'a> {
    version: u8,
    operation_id: &'a OperationId,
    assignment: &'a BuildExecutorAssignment,
    platform: &'a OciPlatform,
    source: BuildSourceEvidence,
    adapter: &'a BuildAdapter,
    timeout_millis: u64,
}

pub(super) fn request_commitment(request: &BuildExecutorStartRequest) -> Result<String, String> {
    let canonical = serde_json::to_vec(&Commitment {
        version: RECORD_VERSION,
        operation_id: &request.operation_id,
        assignment: &request.assignment,
        platform: &request.platform,
        source: request.source.evidence(),
        adapter: &request.adapter,
        timeout_millis: request.timeout_millis,
    })
    .map_err(|error| format!("encode build request commitment: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}

#[cfg(test)]
pub(super) fn raw_path(root: &Path, operation_id: &OperationId) -> PathBuf {
    root.join(format!("{}.json", operation_id.as_str()))
}
