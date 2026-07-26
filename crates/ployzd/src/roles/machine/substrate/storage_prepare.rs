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
        #[cfg(unix)]
        command.process_group(0);
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
    cancel_ack_budget: Duration,
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

    fn new(evidence_directory: &Path, budget: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            evidence_directory: Arc::new(evidence_directory.to_path_buf()),
            budget,
            cancel_ack_budget: ployz_core::storage::MACHINE_STORAGE_PREPARE_CANCEL_ACK_BUDGET,
        }
    }

    #[cfg(test)]
    fn with_cancel_ack_budget(mut self, budget: Duration) -> Self {
        self.cancel_ack_budget = budget;
        self
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
                if process_group_is_live(process.pid) {
                    return Err(termination_failure(
                        "refused to terminate an unverified recovered storage preparation group",
                    ));
                }
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
    ) -> Result<MachineStoragePrepareReport, StorageEffectFailure> {
        let repository = StorageEvidenceRepository::new(&self.evidence_directory);
        match repository.read_report(operation_id)? {
            report @ (MachineStoragePrepareReport::Completed { .. }
            | MachineStoragePrepareReport::Failed { .. }
            | MachineStoragePrepareReport::Cancelled { .. }) => return Ok(report),
            MachineStoragePrepareReport::NotFound => {
                return Err(StorageEffectFailure::ProcessFailed {
                    message: "storage preparation has no machine evidence".to_owned(),
                });
            }
            MachineStoragePrepareReport::Running => {}
        }
        let mut state = self.state.lock().await;
        let Some(active) = state.as_mut() else {
            drop(state);
            recover_stale_running(&repository, operation_id, &reason).await?;
            return terminal_cancel_report(&repository, operation_id);
        };
        if &active.operation_id != operation_id {
            return Err(StorageEffectFailure::ProcessFailed {
                message: "another storage preparation is active".to_owned(),
            });
        }
        if let Some(cancel) = active.cancel.take() {
            let _ = cancel.send(reason);
        }
        drop(state);
        let deadline = tokio::time::Instant::now() + self.cancel_ack_budget;
        loop {
            match repository.read_report(operation_id)? {
                report @ (MachineStoragePrepareReport::Completed { .. }
                | MachineStoragePrepareReport::Failed { .. }
                | MachineStoragePrepareReport::Cancelled { .. }) => return Ok(report),
                MachineStoragePrepareReport::Running => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(StorageEffectFailure::ProcessFailed {
                            message: "storage preparation cancellation did not reach durable terminal evidence within the bounded acknowledgement deadline".to_owned(),
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                MachineStoragePrepareReport::NotFound => {
                    return Err(StorageEffectFailure::ProcessFailed {
                        message: "storage preparation evidence disappeared during cancellation"
                            .to_owned(),
                    });
                }
            }
        }
    }

    async fn report(
        &self,
        operation_id: &OperationId,
    ) -> Result<MachineStoragePrepareReport, StorageEffectFailure> {
        let repository = StorageEvidenceRepository::new(&self.evidence_directory);
        if self
            .state
            .lock()
            .await
            .as_ref()
            .is_some_and(|active| &active.operation_id == operation_id)
        {
            repository.read_report(operation_id)
        } else {
            repository.report(operation_id)
        }
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
            let _ =
                tokio::time::timeout(self.cancel_ack_budget + Duration::from_millis(100), async {
                    let _ = self.cancel(&operation_id, reason).await;
                    loop {
                        if self.state.lock().await.is_none() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await;
        }
    }
}

fn terminal_cancel_report(
    repository: &StorageEvidenceRepository<'_>,
    operation_id: &OperationId,
) -> Result<MachineStoragePrepareReport, StorageEffectFailure> {
    match repository.read_report(operation_id)? {
        report @ (MachineStoragePrepareReport::Completed { .. }
        | MachineStoragePrepareReport::Failed { .. }
        | MachineStoragePrepareReport::Cancelled { .. }) => Ok(report),
        MachineStoragePrepareReport::Running => Err(StorageEffectFailure::ProcessFailed {
            message: "storage preparation cancellation did not reach durable terminal evidence"
                .to_owned(),
        }),
        MachineStoragePrepareReport::NotFound => Err(StorageEffectFailure::ProcessFailed {
            message: "storage preparation has no machine evidence".to_owned(),
        }),
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
    match state.report(&request.operation_id).await {
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
        Ok(report) => machine_success(MachineStoragePrepareCancelRpcResponse::Ok(
            MachineStoragePrepareCancelRpcOk { machine_id, report },
        )),
        Err(failure) => machine_domain_error(MachineStoragePrepareCancelRpcResponse::DomainError {
            machine_id,
            error: storage_failure(failure),
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
        let mut evidence = self.read_optional(operation_id)?;
        if let Some(MachineStoragePreparationEvidence::Running { process, .. }) = &evidence
            && !process_identity_is_expected_live(process, operation_id)
        {
            if process_group_is_live(process.pid) {
                return Ok(MachineStoragePrepareReport::Running);
            }
            let failure = StorageEffectFailure::Interrupted {
                message: "the recorded storage preparation process is no longer live".to_owned(),
            };
            evidence = self.transition_running_to_failure(operation_id, &failure)?;
        }
        Ok(Self::project_report(evidence))
    }

    fn read_report(
        &self,
        operation_id: &OperationId,
    ) -> Result<MachineStoragePrepareReport, StorageEffectFailure> {
        self.read_optional(operation_id).map(Self::project_report)
    }

    fn project_report(
        evidence: Option<MachineStoragePreparationEvidence>,
    ) -> MachineStoragePrepareReport {
        match evidence {
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
        }
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
        self.transition_running_to_failure(operation_id, failure)
            .map(|_| ())
    }

    fn transition_running_to_failure(
        &self,
        operation_id: &OperationId,
        failure: &StorageEffectFailure,
    ) -> Result<Option<MachineStoragePreparationEvidence>, StorageEffectFailure> {
        if matches!(
            self.read_optional(operation_id)?,
            Some(MachineStoragePreparationEvidence::Running { .. })
        ) {
            self.persist_evidence(&MachineStoragePreparationEvidence::Failed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            })?;
        }
        self.read_optional(operation_id)
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
            } else if process_group_is_live(process.pid) {
                Err(StorageEffectFailure::ProcessFailed {
                    message: "the storage preparation leader stopped while its process group remains live"
                        .to_owned(),
                })
            } else {
                let failure = StorageEffectFailure::Interrupted {
                    message: "the recorded storage preparation process is no longer live"
                        .to_owned(),
                };
                match repository.transition_running_to_failure(&operation_id, &failure)? {
                    Some(MachineStoragePreparationEvidence::Completed { .. })
                    | Some(MachineStoragePreparationEvidence::Cancelled { .. }) => Ok(()),
                    Some(MachineStoragePreparationEvidence::Failed { failure, .. }) => Err(failure),
                    Some(MachineStoragePreparationEvidence::Running { .. }) | None => Err(failure),
                }
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
    let child_group_id = child.id().expect("spawned child has a process id");
    if let Err(failure) = wait_for_accepted_evidence(&repository, &operation_id, &mut child).await {
        if let Err(termination) =
            terminate_storage_prepare_child(&repository, &operation_id, child_group_id, &mut child)
                .await
        {
            let _ = accepted.send(Err(storage_failure(termination.clone())));
            retain_running_after_termination_failure(&operation_id, &termination).await;
        }
        let accepted_result = settle_acceptance_failure(&repository, &operation_id, failure);
        let _ = accepted.send(accepted_result.map_err(storage_failure));
        clear_active(&runtime, &operation_id).await;
        return;
    }
    let _ = accepted.send(Ok(()));
    let remaining =
        remaining_budget(&repository, &operation_id, runtime.budget).unwrap_or(Duration::ZERO);
    tokio::select! {
        wait = child.wait() => {
            if let Err(termination) = terminate_owned_process_group(child_group_id).await {
                retain_running_after_termination_failure(&operation_id, &termination).await;
            }
            finish_after_wait(&repository, &operation_id, wait);
        }
        _ = tokio::time::sleep(remaining) => {
            if let Err(termination) = terminate_storage_prepare_child(
                &repository,
                &operation_id,
                child_group_id,
                &mut child,
            ).await {
                retain_running_after_termination_failure(&operation_id, &termination).await;
            }
            let _ = repository.persist_failure_if_running(
                &operation_id,
                &StorageEffectFailure::OperationTimedOut,
            );
        }
        reason = &mut cancel => {
            if let Err(termination) = terminate_storage_prepare_child(
                &repository,
                &operation_id,
                child_group_id,
                &mut child,
            ).await {
                retain_running_after_termination_failure(&operation_id, &termination).await;
            }
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
            if let Err(termination) = terminate_owned_process_group(process.pid).await {
                retain_running_after_termination_failure(&operation_id, &termination).await;
            }
            let _ = repository.persist_failure_if_running(
                &operation_id,
                &StorageEffectFailure::Interrupted {
                    message: "storage preparation process exited after runtime recovery without terminal evidence".to_owned(),
                },
            );
        }
        RecoveredOutcome::TimedOut => {
            if let Err(termination) = terminate_exact_process(&process).await {
                retain_running_after_termination_failure(&operation_id, &termination).await;
            }
            let _ = repository.persist_failure_if_running(
                &operation_id,
                &StorageEffectFailure::OperationTimedOut,
            );
        }
        RecoveredOutcome::Cancelled(reason) => {
            if let Err(termination) = terminate_exact_process(&process).await {
                retain_running_after_termination_failure(&operation_id, &termination).await;
            }
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

async fn terminate_exact_process(
    process: &StoragePreparationProcessIdentity,
) -> Result<(), StorageEffectFailure> {
    if !process_identity_is_live(process) {
        return if process_group_is_live(process.pid) {
            Err(termination_failure(
                "storage preparation group leader identity changed before termination",
            ))
        } else {
            Ok(())
        };
    }
    if !process_is_group_leader(process.pid) {
        return Err(termination_failure(
            "storage preparation process is not its process-group leader",
        ));
    }
    signal_process_group(process.pid, "-TERM").await?;
    if tokio::time::timeout(
        ployz_core::storage::MACHINE_STORAGE_PREPARE_TERMINATION_GRACE,
        wait_for_process_group_exit(process.pid),
    )
    .await
    .is_ok()
    {
        return Ok(());
    }
    if process_group_is_live(process.pid) {
        signal_process_group(process.pid, "-KILL").await?;
        tokio::time::timeout(
            ployz_core::storage::MACHINE_STORAGE_PREPARE_TERMINATION_GRACE,
            wait_for_process_group_exit(process.pid),
        )
        .await
        .map_err(|_| {
            termination_failure("storage preparation process group survived the bounded KILL grace")
        })?;
    }
    Ok(())
}

async fn terminate_owned_process_group(group_id: u32) -> Result<(), StorageEffectFailure> {
    if !process_group_is_live(group_id) {
        return Ok(());
    }
    signal_process_group(group_id, "-TERM").await?;
    if tokio::time::timeout(
        ployz_core::storage::MACHINE_STORAGE_PREPARE_TERMINATION_GRACE,
        wait_for_process_group_exit(group_id),
    )
    .await
    .is_ok()
    {
        return Ok(());
    }
    if process_group_is_live(group_id) {
        signal_process_group(group_id, "-KILL").await?;
        tokio::time::timeout(
            ployz_core::storage::MACHINE_STORAGE_PREPARE_TERMINATION_GRACE,
            wait_for_process_group_exit(group_id),
        )
        .await
        .map_err(|_| {
            termination_failure("owned storage preparation descendants survived bounded KILL")
        })?;
    }
    Ok(())
}

async fn retain_running_after_termination_failure(
    operation_id: &OperationId,
    failure: &StorageEffectFailure,
) -> ! {
    tracing::error!(
        operation_id = operation_id.as_str(),
        error = %failure,
        "storage preparation terminating; retaining operation ownership and nonterminal evidence"
    );
    std::future::pending::<()>().await;
    unreachable!("pending future cannot complete")
}

async fn signal_process_group(pid: u32, signal: &str) -> Result<(), StorageEffectFailure> {
    let status = tokio::process::Command::new("kill")
        .arg(signal)
        .arg("--")
        .arg(format!("-{pid}"))
        .status()
        .await
        .map_err(|error| {
            termination_failure(format!(
                "failed to invoke {signal} for process group {pid}: {error}"
            ))
        })?;
    if status.success() || !process_group_is_live(pid) {
        Ok(())
    } else {
        Err(termination_failure(format!(
            "{signal} for process group {pid} exited with {status}"
        )))
    }
}

fn termination_failure(message: impl Into<String>) -> StorageEffectFailure {
    StorageEffectFailure::ProcessFailed {
        message: message.into(),
    }
}

async fn wait_for_process_group_exit(pid: u32) {
    while process_group_is_live(pid) {
        tokio::time::sleep(Duration::from_millis(20)).await;
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
            return Err(failure);
        }
        if tokio::time::Instant::now() >= deadline {
            let failure = StorageEffectFailure::ProcessFailed {
                message: "storage preparation did not establish running evidence".to_owned(),
            };
            return Err(failure);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn settle_acceptance_failure(
    repository: &StorageEvidenceRepository<'_>,
    operation_id: &OperationId,
    failure: StorageEffectFailure,
) -> Result<(), StorageEffectFailure> {
    let evidence = match repository.read_optional(operation_id) {
        Ok(Some(MachineStoragePreparationEvidence::Running { .. })) => {
            repository.transition_running_to_failure(operation_id, &failure)?
        }
        Ok(Some(evidence)) => Some(evidence),
        Ok(None) | Err(_) => {
            repository.persist_failure(operation_id, &failure)?;
            repository.read_optional(operation_id)?
        }
    };
    match evidence {
        Some(MachineStoragePreparationEvidence::Completed { .. })
        | Some(MachineStoragePreparationEvidence::Cancelled { .. }) => Ok(()),
        Some(MachineStoragePreparationEvidence::Failed { failure, .. }) => Err(failure),
        Some(MachineStoragePreparationEvidence::Running { .. }) | None => Err(failure),
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
    if now < launched_at_unix_millis {
        return Ok(Duration::ZERO);
    }
    Ok(budget.saturating_sub(Duration::from_millis(now - launched_at_unix_millis)))
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

async fn recover_stale_running(
    repository: &StorageEvidenceRepository<'_>,
    operation_id: &OperationId,
    reason: &CancellationReason,
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
    if process_group_is_live(process.pid) {
        return Err(termination_failure(
            "refused to cancel an unverified stale storage preparation group",
        ));
    }
    repository.persist_cancelled_if_running(operation_id, reason)
}

#[cfg(target_os = "linux")]
fn process_identity_is_live(expected: &StoragePreparationProcessIdentity) -> bool {
    read_process_identity(expected.pid).as_ref() == Some(expected)
}

#[cfg(target_os = "linux")]
fn process_is_group_leader(pid: u32) -> bool {
    read_process_group_id(pid) == Some(pid)
}

#[cfg(target_os = "linux")]
fn process_group_is_live(group_id: u32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            return false;
        };
        read_process_state_and_group(pid)
            .is_some_and(|(state, process_group)| state != 'Z' && process_group == group_id)
    })
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
    let (_, tail) = stat.rsplit_once(") ")?;
    let fields = tail.split_whitespace().collect::<Vec<_>>();
    if fields.first() == Some(&"Z") {
        return None;
    }
    let start_time = fields.get(19)?;
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

#[cfg(target_os = "linux")]
fn read_process_group_id(pid: u32) -> Option<u32> {
    read_process_state_and_group(pid).map(|(_, group_id)| group_id)
}

#[cfg(target_os = "linux")]
fn read_process_state_and_group(pid: u32) -> Option<(char, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, tail) = stat.rsplit_once(") ")?;
    let mut fields = tail.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let _parent_pid = fields.next()?;
    let group_id = fields.next()?.parse().ok()?;
    Some((state, group_id))
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

#[cfg(not(target_os = "linux"))]
fn process_is_group_leader(_pid: u32) -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
fn process_group_is_live(_group_id: u32) -> bool {
    false
}

async fn terminate_storage_prepare_child(
    repository: &StorageEvidenceRepository<'_>,
    operation_id: &OperationId,
    group_id: u32,
    child: &mut tokio::process::Child,
) -> Result<(), StorageEffectFailure> {
    let Some(pid) = child.id() else {
        terminate_owned_process_group(group_id).await?;
        return tokio::time::timeout(
            ployz_core::storage::MACHINE_STORAGE_PREPARE_TERMINATION_GRACE,
            child.wait(),
        )
        .await
        .map_err(|_| {
            termination_failure(
                "exited storage preparation child was not reaped within the bounded grace",
            )
        })?
        .map(|_| ())
        .map_err(|error| {
            termination_failure(format!(
                "failed to reap exited storage preparation: {error}"
            ))
        });
    };
    let persisted = repository.read_optional(operation_id).ok().flatten();
    let identity = match persisted {
        Some(MachineStoragePreparationEvidence::Running { process, .. })
            if process.pid == pid
                && process_identity_is_expected_live(&process, operation_id)
                && process_is_group_leader(pid) =>
        {
            process
        }
        _ => {
            let Some(process) = read_process_identity(pid) else {
                terminate_owned_process_group(group_id).await?;
                return tokio::time::timeout(
                    ployz_core::storage::MACHINE_STORAGE_PREPARE_TERMINATION_GRACE,
                    child.wait(),
                )
                .await
                .map_err(|_| termination_failure("storage preparation child disappeared but was not reaped within the bounded grace"))?
                .map(|_| ())
                .map_err(|error| termination_failure(format!("failed to reap disappeared storage preparation: {error}")));
            };
            if !process_identity_is_expected_live(&process, operation_id)
                || !process_is_group_leader(pid)
            {
                return Err(StorageEffectFailure::ProcessFailed {
                    message: "refused to terminate an unverified storage preparation process group"
                        .to_owned(),
                });
            }
            process
        }
    };
    terminate_exact_process(&identity).await?;
    tokio::time::timeout(
        ployz_core::storage::MACHINE_STORAGE_PREPARE_TERMINATION_GRACE,
        child.wait(),
    )
    .await
    .map_err(|_| {
        termination_failure("storage preparation child was not reaped within the bounded grace")
    })?
    .map(|_| ())
    .map_err(|error| termination_failure(format!("failed to reap storage preparation: {error}")))
}

#[cfg(test)]
mod tests;
