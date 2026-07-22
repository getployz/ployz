use super::super::response::{machine_domain_error, machine_success};
use super::{MACHINE_SUBSTRATE_LOCK_FILE, PrivilegedSubstrateGuard, try_privileged_substrate};
use crate::roles::machine::protocol::{
    MachineStoragePrepareCancelRpcOk, MachineStoragePrepareCancelRpcRequest,
    MachineStoragePrepareCancelRpcResponse, MachineStoragePrepareDomainError,
    MachineStoragePrepareReport, MachineStoragePrepareReportRpcOk,
    MachineStoragePrepareReportRpcRequest, MachineStoragePrepareReportRpcResponse,
    MachineStoragePrepareRpcOk, MachineStoragePrepareRpcRequest, MachineStoragePrepareRpcResponse,
};
use atomic_write_file::AtomicWriteFile;
#[cfg(unix)]
use atomic_write_file::unix::OpenOptionsExt as AtomicOpenOptionsExt;
use ployz_core::deploy::ZfsPoolName;
use ployz_core::ids::{MachineId, OperationId};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use std::io::ErrorKind;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as StdOpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, oneshot};

use ployz_core::operation::CancellationReason;
use ployz_core::storage::{
    MACHINE_STORAGE_PREPARE_BUDGET, MachineStoragePreparationEvidence, StorageEffectFailure,
    StorageOperationEvidenceFile, StoragePreparationProcessIdentity,
};

const STORAGE_OPERATION_DIRECTORY: &str = "/var/lib/ployz/storage-operations";

enum PrivilegedHostEffect<'a> {
    StoragePrepare {
        operation_id: &'a OperationId,
        pool: Option<&'a ZfsPoolName>,
    },
}

impl PrivilegedHostEffect<'_> {
    fn into_command(self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("flock");
        command
            .arg("--no-fork")
            .arg("--nonblock")
            .arg(MACHINE_SUBSTRATE_LOCK_FILE)
            .arg("ployz")
            .arg("host");
        match self {
            Self::StoragePrepare { operation_id, pool } => {
                command
                    .arg("storage-prepare")
                    .arg("--operation-id")
                    .arg(operation_id.as_str());
                if let Some(pool) = pool {
                    command.arg("--pool").arg(pool.as_str());
                }
            }
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}

fn spawn_bounded_privileged_host_effect(
    mut command: tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    command.kill_on_drop(true).spawn()
}

#[derive(Clone)]
pub(crate) struct StoragePrepareRuntime {
    state: Arc<Mutex<Option<ActiveStoragePreparation>>>,
    evidence_directory: Arc<std::path::PathBuf>,
    budget: Duration,
}

struct ActiveStoragePreparation {
    operation_id: OperationId,
    cancel: Option<oneshot::Sender<CancellationReason>>,
}

impl StoragePrepareRuntime {
    pub(crate) fn host_default() -> Self {
        Self::new(
            Path::new(STORAGE_OPERATION_DIRECTORY),
            MACHINE_STORAGE_PREPARE_BUDGET,
        )
    }

    #[cfg(test)]
    fn new(evidence_directory: &Path, budget: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            evidence_directory: Arc::new(evidence_directory.to_path_buf()),
            budget,
        }
    }

    pub(crate) async fn recover(&self) -> Result<(), StorageEffectFailure> {
        let entries = match std::fs::read_dir(&*self.evidence_directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(StorageEffectFailure::ProcessFailed {
                    message: format!(
                        "failed to scan {}: {error}",
                        self.evidence_directory.display()
                    ),
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|error| StorageEffectFailure::ProcessFailed {
                message: format!("failed to scan storage preparation evidence: {error}"),
            })?;
            let bytes = std::fs::read(entry.path()).map_err(|error| {
                StorageEffectFailure::ProcessFailed {
                    message: format!("failed to read {}: {error}", entry.path().display()),
                }
            })?;
            let evidence: MachineStoragePreparationEvidence = serde_json::from_slice(&bytes)
                .map_err(|error| StorageEffectFailure::ProcessFailed {
                    message: format!("failed to decode {}: {error}", entry.path().display()),
                })?;
            let MachineStoragePreparationEvidence::Running {
                operation_id,
                process,
                ..
            } = evidence
            else {
                continue;
            };
            let repository = StorageEvidenceRepository::new(&self.evidence_directory);
            if !process_identity_is_expected_live(&process, &operation_id) {
                repository.persist_failure_if_running(
                    &operation_id,
                    &StorageEffectFailure::Interrupted {
                        message: "storage preparation did not survive machine runtime restart"
                            .to_owned(),
                    },
                )?;
                continue;
            }
            let mut state = self.state.lock().await;
            if let Some(active) = state.as_ref() {
                return Err(StorageEffectFailure::ProcessFailed {
                    message: format!(
                        "multiple live storage preparations: {} and {}",
                        active.operation_id.as_str(),
                        operation_id.as_str()
                    ),
                });
            }
            let guard = try_privileged_substrate(&operation_id).map_err(|owner| {
                StorageEffectFailure::ProcessFailed {
                    message: owner.map_or_else(
                        || "privileged substrate lock is busy during storage recovery".to_owned(),
                        |owner| {
                            format!(
                                "privileged substrate lock is owned by {} during storage recovery",
                                owner.as_str()
                            )
                        },
                    ),
                }
            })?;
            let (cancel, cancel_rx) = oneshot::channel();
            *state = Some(ActiveStoragePreparation {
                operation_id: operation_id.clone(),
                cancel: Some(cancel),
            });
            let runtime = self.clone();
            tokio::spawn(async move {
                supervise_recovered_storage_prepare(
                    runtime,
                    operation_id,
                    process,
                    guard,
                    cancel_rx,
                )
                .await;
            });
        }
        Ok(())
    }

    #[cfg(not(test))]
    fn new(evidence_directory: &Path, budget: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            evidence_directory: Arc::new(evidence_directory.to_path_buf()),
            budget,
        }
    }

    async fn start(
        &self,
        operation_id: OperationId,
        pool: Option<ZfsPoolName>,
    ) -> Result<(), MachineStoragePrepareDomainError> {
        let repository = StorageEvidenceRepository::new(&self.evidence_directory);
        if let Some(evidence) = repository
            .read_optional(&operation_id)
            .map_err(storage_failure)?
        {
            return acknowledge_existing(&repository, evidence).map_err(storage_failure);
        }
        let mut state = self.state.lock().await;
        if let Some(active) = state.as_ref() {
            if active.operation_id == operation_id {
                drop(state);
                return wait_for_existing_evidence(&repository, &operation_id)
                    .await
                    .map_err(storage_failure);
            }
            return Err(MachineStoragePrepareDomainError::Busy {
                owner_operation_id: active.operation_id.clone(),
            });
        }
        let guard = try_privileged_substrate(&operation_id).map_err(|owner| match owner {
            Some(owner_operation_id) => {
                MachineStoragePrepareDomainError::Busy { owner_operation_id }
            }
            None => storage_failure(StorageEffectFailure::ProcessFailed {
                message: "privileged substrate lock is busy without owner evidence".to_owned(),
            }),
        })?;
        let command = PrivilegedHostEffect::StoragePrepare {
            operation_id: &operation_id,
            pool: pool.as_ref(),
        }
        .into_command();
        let child = spawn_bounded_privileged_host_effect(command).map_err(|error| {
            storage_failure(StorageEffectFailure::ProcessFailed {
                message: format!("failed to launch storage preparation: {error}"),
            })
        })?;
        let (cancel, cancel_rx) = oneshot::channel();
        let (accepted, accepted_rx) = oneshot::channel();
        *state = Some(ActiveStoragePreparation {
            operation_id: operation_id.clone(),
            cancel: Some(cancel),
        });
        let runtime = self.clone();
        tokio::spawn(async move {
            supervise_storage_prepare_child(
                runtime,
                operation_id,
                child,
                guard,
                cancel_rx,
                accepted,
            )
            .await;
        });
        drop(state);
        accepted_rx.await.map_err(|_| {
            storage_failure(StorageEffectFailure::ProcessFailed {
                message: "storage preparation supervisor stopped before acceptance".to_owned(),
            })
        })?
    }

    async fn cancel(
        &self,
        operation_id: &OperationId,
        reason: CancellationReason,
    ) -> Result<(), StorageEffectFailure> {
        let repository = StorageEvidenceRepository::new(&self.evidence_directory);
        match repository.read_optional(operation_id)? {
            Some(
                MachineStoragePreparationEvidence::Completed { .. }
                | MachineStoragePreparationEvidence::Failed { .. }
                | MachineStoragePreparationEvidence::Cancelled { .. },
            ) => return Ok(()),
            None => return Ok(()),
            Some(MachineStoragePreparationEvidence::Running { .. }) => {}
        }
        let mut state = self.state.lock().await;
        let Some(active) = state.as_mut() else {
            return recover_stale_running(&repository, operation_id);
        };
        if &active.operation_id != operation_id {
            return Err(StorageEffectFailure::ProcessFailed {
                message: "another storage preparation is active".to_owned(),
            });
        }
        if let Some(cancel) = active.cancel.take() {
            let _ = cancel.send(reason);
        }
        Ok(())
    }

    pub(crate) async fn shutdown(&self) {
        let operation_id = self
            .state
            .lock()
            .await
            .as_ref()
            .map(|active| active.operation_id.clone());
        if let Some(operation_id) = operation_id {
            let reason = CancellationReason::try_new("machine runtime shutdown")
                .expect("shutdown cancellation reason is non-empty");
            let _ = self.cancel(&operation_id, reason).await;
            let _ = tokio::time::timeout(
                ployz_core::storage::MACHINE_STORAGE_PREPARE_TERMINATION_GRACE,
                async {
                    loop {
                        if self.state.lock().await.is_none() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                },
            )
            .await;
        }
    }
}

async fn wait_for_existing_evidence(
    repository: &StorageEvidenceRepository<'_>,
    operation_id: &OperationId,
) -> Result<(), StorageEffectFailure> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(evidence) = repository.read_optional(operation_id)? {
            return acknowledge_existing(repository, evidence);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(StorageEffectFailure::ProcessFailed {
                message: "active storage preparation has no queryable evidence".to_owned(),
            });
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn storage_failure(failure: StorageEffectFailure) -> MachineStoragePrepareDomainError {
    MachineStoragePrepareDomainError::PreparationFailed { failure }
}

pub(crate) async fn handle_storage_prepare(
    machine_id: MachineId,
    state: StoragePrepareRuntime,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineStoragePrepareRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match state.start(request.operation_id, request.pool).await {
        Ok(()) => machine_success(MachineStoragePrepareRpcResponse::Ok(
            MachineStoragePrepareRpcOk { machine_id },
        )),
        Err(error) => machine_domain_error(MachineStoragePrepareRpcResponse::DomainError {
            machine_id,
            error,
        }),
    }
}

pub(crate) async fn handle_storage_prepare_report(
    machine_id: MachineId,
    state: StoragePrepareRuntime,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineStoragePrepareReportRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match StorageEvidenceRepository::new(&state.evidence_directory).report(&request.operation_id) {
        Ok(report) => machine_success(MachineStoragePrepareReportRpcResponse::Ok(
            MachineStoragePrepareReportRpcOk { machine_id, report },
        )),
        Err(failure) => machine_domain_error(MachineStoragePrepareReportRpcResponse::DomainError {
            machine_id,
            error: MachineStoragePrepareDomainError::PreparationFailed { failure },
        }),
    }
}

pub(crate) async fn handle_storage_prepare_cancel(
    machine_id: MachineId,
    state: StoragePrepareRuntime,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineStoragePrepareCancelRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match state.cancel(&request.operation_id, request.reason).await {
        Ok(()) => machine_success(MachineStoragePrepareCancelRpcResponse::Ok(
            MachineStoragePrepareCancelRpcOk { machine_id },
        )),
        Err(failure) => machine_domain_error(MachineStoragePrepareCancelRpcResponse::DomainError {
            machine_id,
            error: storage_failure(failure),
        }),
    }
}

#[cfg(test)]
fn read_storage_prepare_evidence_at(
    directory: &Path,
    operation_id: &OperationId,
) -> Result<Option<ployz_core::deploy::ZfsPoolName>, StorageEffectFailure> {
    match StorageEvidenceRepository::new(directory).report(operation_id)? {
        MachineStoragePrepareReport::Completed { pool } => Ok(Some(pool)),
        MachineStoragePrepareReport::NotFound | MachineStoragePrepareReport::Running => Ok(None),
        MachineStoragePrepareReport::Failed { failure } => Err(failure),
        MachineStoragePrepareReport::Cancelled { .. } => Err(StorageEffectFailure::Interrupted {
            message: "storage preparation was cancelled".to_owned(),
        }),
    }
}

struct StorageEvidenceRepository<'a> {
    directory: &'a Path,
}

impl<'a> StorageEvidenceRepository<'a> {
    fn new(directory: &'a Path) -> Self {
        Self { directory }
    }

    fn file(&self, operation_id: &OperationId) -> StorageOperationEvidenceFile {
        StorageOperationEvidenceFile::in_evidence_directory(self.directory, operation_id.clone())
    }

    fn read_optional(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<MachineStoragePreparationEvidence>, StorageEffectFailure> {
        let file = self.file(operation_id);
        let bytes = match std::fs::read(file.path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StorageEffectFailure::ProcessFailed {
                    message: format!("failed to read {}: {error}", file.path().display()),
                });
            }
        };
        let evidence: MachineStoragePreparationEvidence =
            serde_json::from_slice(&bytes).map_err(|error| {
                StorageEffectFailure::ProcessFailed {
                    message: format!("failed to decode {}: {error}", file.path().display()),
                }
            })?;
        file.validate(&evidence)?;
        Ok(Some(evidence))
    }

    fn report(
        &self,
        operation_id: &OperationId,
    ) -> Result<MachineStoragePrepareReport, StorageEffectFailure> {
        let evidence = self.read_optional(operation_id)?;
        if let Some(MachineStoragePreparationEvidence::Running { process, .. }) = &evidence
            && !process_identity_is_expected_live(process, operation_id)
        {
            let failure = StorageEffectFailure::Interrupted {
                message: "the recorded storage preparation process is no longer live".to_owned(),
            };
            self.persist_failure_if_running(operation_id, &failure)?;
            return Ok(MachineStoragePrepareReport::Failed { failure });
        }
        Ok(match evidence {
            None => MachineStoragePrepareReport::NotFound,
            Some(MachineStoragePreparationEvidence::Running { .. }) => {
                MachineStoragePrepareReport::Running
            }
            Some(MachineStoragePreparationEvidence::Completed { prepared, .. }) => {
                MachineStoragePrepareReport::Completed {
                    pool: prepared.pool().clone(),
                }
            }
            Some(MachineStoragePreparationEvidence::Failed { failure, .. }) => {
                MachineStoragePrepareReport::Failed { failure }
            }
            Some(MachineStoragePreparationEvidence::Cancelled { reason, .. }) => {
                MachineStoragePrepareReport::Cancelled { reason }
            }
        })
    }

    fn persist_failure(
        &self,
        operation_id: &OperationId,
        failure: &StorageEffectFailure,
    ) -> Result<(), StorageEffectFailure> {
        std::fs::create_dir_all(self.directory).map_err(|error| {
            StorageEffectFailure::ProcessFailed {
                message: format!("failed to create {}: {error}", self.directory.display()),
            }
        })?;
        #[cfg(unix)]
        std::fs::set_permissions(self.directory, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| StorageEffectFailure::ProcessFailed {
                message: format!("failed to protect {}: {error}", self.directory.display()),
            },
        )?;
        let file = self.file(operation_id);
        let bytes = serde_json::to_vec_pretty(&MachineStoragePreparationEvidence::Failed {
            operation_id: operation_id.clone(),
            failure: failure.clone(),
        })
        .map_err(|error| StorageEffectFailure::ProcessFailed {
            message: format!("failed to encode terminal evidence: {error}"),
        })?;
        let mut atomic = open_secret_atomic(file.path()).map_err(|error| {
            StorageEffectFailure::ProcessFailed {
                message: format!("failed to create {}: {error}", file.path().display()),
            }
        })?;
        atomic
            .write_all(&bytes)
            .and_then(|()| atomic.commit())
            .map_err(|error| StorageEffectFailure::ProcessFailed {
                message: format!("failed to commit {}: {error}", file.path().display()),
            })?;
        Ok(())
    }

    fn persist_failure_if_running(
        &self,
        operation_id: &OperationId,
        failure: &StorageEffectFailure,
    ) -> Result<(), StorageEffectFailure> {
        if matches!(
            self.read_optional(operation_id)?,
            Some(MachineStoragePreparationEvidence::Running { .. })
        ) {
            self.persist_evidence(&MachineStoragePreparationEvidence::Failed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            })?;
        }
        Ok(())
    }

    fn persist_cancelled_if_running(
        &self,
        operation_id: &OperationId,
        reason: &CancellationReason,
    ) -> Result<(), StorageEffectFailure> {
        if matches!(
            self.read_optional(operation_id)?,
            Some(MachineStoragePreparationEvidence::Running { .. })
        ) {
            self.persist_evidence(&MachineStoragePreparationEvidence::Cancelled {
                operation_id: operation_id.clone(),
                reason: reason.clone(),
            })?;
        }
        Ok(())
    }

    fn persist_evidence(
        &self,
        evidence: &MachineStoragePreparationEvidence,
    ) -> Result<(), StorageEffectFailure> {
        std::fs::create_dir_all(self.directory).map_err(|error| {
            StorageEffectFailure::ProcessFailed {
                message: format!("failed to create {}: {error}", self.directory.display()),
            }
        })?;
        let file = self.file(evidence.operation_id());
        let bytes = serde_json::to_vec_pretty(evidence).map_err(|error| {
            StorageEffectFailure::ProcessFailed {
                message: format!("failed to encode terminal evidence: {error}"),
            }
        })?;
        let mut atomic = open_secret_atomic(file.path()).map_err(|error| {
            StorageEffectFailure::ProcessFailed {
                message: format!("failed to create {}: {error}", file.path().display()),
            }
        })?;
        atomic
            .write_all(&bytes)
            .and_then(|()| atomic.commit())
            .map_err(|error| StorageEffectFailure::ProcessFailed {
                message: format!("failed to commit {}: {error}", file.path().display()),
            })
    }
}

fn open_secret_atomic(path: &Path) -> std::io::Result<AtomicWriteFile> {
    let mut options = AtomicWriteFile::options();
    #[cfg(unix)]
    {
        AtomicOpenOptionsExt::preserve_mode(&mut options, false);
        StdOpenOptionsExt::mode(&mut options, 0o600);
    }
    options.open(path)
}

fn acknowledge_existing(
    repository: &StorageEvidenceRepository<'_>,
    evidence: MachineStoragePreparationEvidence,
) -> Result<(), StorageEffectFailure> {
    match evidence {
        MachineStoragePreparationEvidence::Running {
            operation_id,
            process,
            ..
        } => {
            if process_identity_is_expected_live(&process, &operation_id) {
                Ok(())
            } else {
                let failure = StorageEffectFailure::Interrupted {
                    message: "the recorded storage preparation process is no longer live"
                        .to_owned(),
                };
                repository.persist_failure_if_running(&operation_id, &failure)?;
                Err(failure)
            }
        }
        MachineStoragePreparationEvidence::Completed { .. }
        | MachineStoragePreparationEvidence::Failed { .. }
        | MachineStoragePreparationEvidence::Cancelled { .. } => Ok(()),
    }
}

async fn supervise_storage_prepare_child(
    runtime: StoragePrepareRuntime,
    operation_id: OperationId,
    mut child: tokio::process::Child,
    _guard: PrivilegedSubstrateGuard,
    mut cancel: oneshot::Receiver<CancellationReason>,
    accepted: oneshot::Sender<Result<(), MachineStoragePrepareDomainError>>,
) {
    let repository = StorageEvidenceRepository::new(&runtime.evidence_directory);
    let accepted_result = wait_for_accepted_evidence(&repository, &operation_id, &mut child).await;
    let accepted_ok = accepted_result.is_ok();
    let _ = accepted.send(accepted_result.map_err(storage_failure));
    if !accepted_ok {
        let _ = terminate_storage_prepare_child(&mut child).await;
        clear_active(&runtime, &operation_id).await;
        return;
    }
    let remaining =
        remaining_budget(&repository, &operation_id, runtime.budget).unwrap_or(Duration::ZERO);
    tokio::select! {
        wait = child.wait() => {
            finish_after_wait(&repository, &operation_id, wait);
        }
        _ = tokio::time::sleep(remaining) => {
            let _ = terminate_storage_prepare_child(&mut child).await;
            let _ = repository.persist_failure_if_running(
                &operation_id,
                &StorageEffectFailure::OperationTimedOut,
            );
        }
        reason = &mut cancel => {
            let _ = terminate_storage_prepare_child(&mut child).await;
            let _ = child.wait().await;
            if let Ok(reason) = reason {
                let _ = repository.persist_cancelled_if_running(&operation_id, &reason);
            }
        }
    }
    clear_active(&runtime, &operation_id).await;
}

async fn supervise_recovered_storage_prepare(
    runtime: StoragePrepareRuntime,
    operation_id: OperationId,
    process: StoragePreparationProcessIdentity,
    _guard: PrivilegedSubstrateGuard,
    mut cancel: oneshot::Receiver<CancellationReason>,
) {
    let repository = StorageEvidenceRepository::new(&runtime.evidence_directory);
    let remaining =
        remaining_budget(&repository, &operation_id, runtime.budget).unwrap_or(Duration::ZERO);
    let outcome = tokio::select! {
        _ = tokio::time::sleep(remaining) => RecoveredOutcome::TimedOut,
        reason = &mut cancel => RecoveredOutcome::Cancelled(reason.ok()),
        () = wait_for_process_exit(&process) => RecoveredOutcome::Exited,
    };
    match outcome {
        RecoveredOutcome::Exited => {
            let _ = repository.persist_failure_if_running(
                &operation_id,
                &StorageEffectFailure::Interrupted {
                    message: "storage preparation process exited after runtime recovery without terminal evidence".to_owned(),
                },
            );
        }
        RecoveredOutcome::TimedOut => {
            terminate_exact_process(&process).await;
            let _ = repository.persist_failure_if_running(
                &operation_id,
                &StorageEffectFailure::OperationTimedOut,
            );
        }
        RecoveredOutcome::Cancelled(reason) => {
            terminate_exact_process(&process).await;
            if let Some(reason) = reason {
                let _ = repository.persist_cancelled_if_running(&operation_id, &reason);
            }
        }
    }
    clear_active(&runtime, &operation_id).await;
}

enum RecoveredOutcome {
    Exited,
    TimedOut,
    Cancelled(Option<CancellationReason>),
}

async fn wait_for_process_exit(process: &StoragePreparationProcessIdentity) {
    while process_identity_is_live(process) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn terminate_exact_process(process: &StoragePreparationProcessIdentity) {
    if !process_identity_is_live(process) {
        return;
    }
    let _ = tokio::process::Command::new("kill")
        .arg("-TERM")
        .arg(process.pid.to_string())
        .status()
        .await;
    if tokio::time::timeout(
        ployz_core::storage::MACHINE_STORAGE_PREPARE_TERMINATION_GRACE,
        wait_for_process_exit(process),
    )
    .await
    .is_ok()
    {
        return;
    }
    if process_identity_is_live(process) {
        let _ = tokio::process::Command::new("kill")
            .arg("-KILL")
            .arg(process.pid.to_string())
            .status()
            .await;
        wait_for_process_exit(process).await;
    }
}

async fn wait_for_accepted_evidence(
    repository: &StorageEvidenceRepository<'_>,
    operation_id: &OperationId,
    child: &mut tokio::process::Child,
) -> Result<(), StorageEffectFailure> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(evidence) = repository.read_optional(operation_id)? {
            return acknowledge_existing(repository, evidence);
        }
        if let Some(status) =
            child
                .try_wait()
                .map_err(|error| StorageEffectFailure::ProcessFailed {
                    message: format!("failed waiting for storage preparation acceptance: {error}"),
                })?
        {
            let failure = StorageEffectFailure::ProcessFailed {
                message: format!("storage preparation exited before acceptance with {status}"),
            };
            repository.persist_failure(operation_id, &failure)?;
            return Err(failure);
        }
        if tokio::time::Instant::now() >= deadline {
            let failure = StorageEffectFailure::ProcessFailed {
                message: "storage preparation did not establish running evidence".to_owned(),
            };
            repository.persist_failure(operation_id, &failure)?;
            return Err(failure);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn remaining_budget(
    repository: &StorageEvidenceRepository<'_>,
    operation_id: &OperationId,
    budget: Duration,
) -> Result<Duration, StorageEffectFailure> {
    let Some(MachineStoragePreparationEvidence::Running {
        launched_at_unix_millis,
        ..
    }) = repository.read_optional(operation_id)?
    else {
        return Ok(Duration::ZERO);
    };
    let now: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| StorageEffectFailure::ProcessFailed {
            message: format!("system clock precedes Unix epoch: {error}"),
        })?
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(budget.saturating_sub(Duration::from_millis(
        now.saturating_sub(launched_at_unix_millis),
    )))
}

fn finish_after_wait(
    repository: &StorageEvidenceRepository<'_>,
    operation_id: &OperationId,
    wait: std::io::Result<std::process::ExitStatus>,
) {
    let terminal = matches!(
        repository.read_optional(operation_id),
        Ok(Some(
            MachineStoragePreparationEvidence::Completed { .. }
                | MachineStoragePreparationEvidence::Failed { .. }
                | MachineStoragePreparationEvidence::Cancelled { .. }
        ))
    );
    if terminal {
        return;
    }
    let failure = StorageEffectFailure::ProcessFailed {
        message: match wait {
            Ok(status) => format!("storage preparation exited without terminal evidence: {status}"),
            Err(error) => format!("failed waiting for storage preparation: {error}"),
        },
    };
    let _ = repository.persist_failure_if_running(operation_id, &failure);
}

async fn clear_active(runtime: &StoragePrepareRuntime, operation_id: &OperationId) {
    let mut state = runtime.state.lock().await;
    if state
        .as_ref()
        .is_some_and(|active| &active.operation_id == operation_id)
    {
        *state = None;
    }
}

fn recover_stale_running(
    repository: &StorageEvidenceRepository<'_>,
    operation_id: &OperationId,
) -> Result<(), StorageEffectFailure> {
    let Some(MachineStoragePreparationEvidence::Running { process, .. }) =
        repository.read_optional(operation_id)?
    else {
        return Ok(());
    };
    if process_identity_is_expected_live(&process, operation_id) {
        return Err(StorageEffectFailure::ProcessFailed {
            message: "storage preparation is live but is not owned by this daemon".to_owned(),
        });
    }
    let failure = StorageEffectFailure::Interrupted {
        message: "the recorded storage preparation process stopped before cancellation".to_owned(),
    };
    repository.persist_failure_if_running(operation_id, &failure)
}

#[cfg(target_os = "linux")]
fn process_identity_is_live(expected: &StoragePreparationProcessIdentity) -> bool {
    read_process_identity(expected.pid).as_ref() == Some(expected)
}

#[cfg(target_os = "linux")]
fn process_identity_is_expected_live(
    expected: &StoragePreparationProcessIdentity,
    operation_id: &OperationId,
) -> bool {
    process_identity_is_live(expected)
        && expected
            .expected_command
            .split('\0')
            .any(|argument| argument == "storage-prepare")
        && expected
            .expected_command
            .split('\0')
            .any(|argument| argument == operation_id.as_str())
}

#[cfg(target_os = "linux")]
fn read_process_identity(pid: u32) -> Option<StoragePreparationProcessIdentity> {
    let Ok(boot_id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") else {
        return None;
    };
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return None;
    };
    let Some((_, tail)) = stat.rsplit_once(") ") else {
        return None;
    };
    let Some(start_time) = tail.split_whitespace().nth(19) else {
        return None;
    };
    let Ok(command) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return None;
    };
    Some(StoragePreparationProcessIdentity {
        boot_id: boot_id.trim().to_owned(),
        pid,
        start_time_ticks: start_time.parse().ok()?,
        expected_command: command
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| String::from_utf8_lossy(argument))
            .collect::<Vec<_>>()
            .join("\u{0}"),
    })
}

#[cfg(not(target_os = "linux"))]
fn process_identity_is_live(_expected: &StoragePreparationProcessIdentity) -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
fn process_identity_is_expected_live(
    _expected: &StoragePreparationProcessIdentity,
    _operation_id: &OperationId,
) -> bool {
    false
}

async fn terminate_storage_prepare_child(
    child: &mut tokio::process::Child,
) -> Result<(), StorageEffectFailure> {
    child
        .kill()
        .await
        .map_err(|error| StorageEffectFailure::ProcessFailed {
            message: format!("failed to terminate storage preparation: {error}"),
        })
}

#[cfg(test)]
mod tests;
