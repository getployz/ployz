//! Runtime for operation-owned machine lifecycle changes (drain/resume).
//!
//! The evidence file is the durable commit: lifecycle is operator intent
//! about a machine (which may be unreachable), so it is control-side
//! durable authority like the authorized-user set — written to disk first,
//! projected into the KV machine record after, and adopted back into KV on
//! control start so the intent survives JetStream loss. A failed KV
//! projection rolls the evidence back, so an operation that reported
//! `Failed` cannot take effect at a later adoption.

use crate::controllers::OperationControllers;
use crate::tasks::TaskRegistry;
use ployz_core::ids::MachineId;
use ployz_core::ops::{FailureMessage, MachineLifecycleFailure, MachineLifecycleTransition};
use ployz_core::state::MachineLifecycle;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::operations::AcceptedMachineLifecycleSubmission;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// All clones share one evidence-file lock: concurrent lifecycle operations
/// each read-modify-write the whole file, so the process must have exactly
/// one runtime instance per evidence file.
#[derive(Debug, Clone)]
pub struct MachineLifecycleOperationRuntime {
    controllers: OperationControllers,
    core_state: AsyncNatsCoreStateStore,
    evidence_file: PathBuf,
    evidence_lock: Arc<Mutex<()>>,
    task_registry: TaskRegistry,
}

impl MachineLifecycleOperationRuntime {
    #[must_use]
    pub fn new(
        controllers: OperationControllers,
        core_state: AsyncNatsCoreStateStore,
        evidence_file: PathBuf,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            controllers,
            core_state,
            evidence_file,
            evidence_lock: Arc::new(Mutex::new(())),
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedMachineLifecycleSubmission) {
        if !accepted.should_start_execution {
            return;
        }

        let runtime = self.clone();
        self.task_registry.spawn(async move {
            runtime.run(accepted).await;
        });
    }

    pub async fn run(self, accepted: AcceptedMachineLifecycleSubmission) {
        let operation_id = accepted.operation_id;
        let machine_id = accepted.machine_id;
        let target = accepted.target;

        let _evidence_guard = self.evidence_lock.lock().await;

        let active = match self.core_state.active_machine(&machine_id).await {
            Ok(Some(active)) => active,
            Ok(None) => {
                self.record_terminal(
                    &operation_id,
                    &machine_id,
                    MachineLifecycleTransition::Failed {
                        failure: MachineLifecycleFailure::NoSuchMachine {
                            machine_id: machine_id.clone(),
                        },
                    },
                )
                .await;
                return;
            }
            Err(error) => {
                self.record_state_commit_failed(&operation_id, &machine_id, &error.to_string())
                    .await;
                return;
            }
        };

        let evidence_changed =
            match record_lifecycle_evidence(&self.evidence_file, &machine_id, target) {
                Ok(changed) => changed,
                Err(message) => {
                    self.record_terminal(
                        &operation_id,
                        &machine_id,
                        MachineLifecycleTransition::Failed {
                            failure: MachineLifecycleFailure::EvidenceWriteFailed { message },
                        },
                    )
                    .await;
                    return;
                }
            };

        let mut active = active;
        active.lifecycle = target;
        if let Err(error) = self.core_state.replace_active_machine(&active).await {
            if evidence_changed {
                let revert = match target {
                    MachineLifecycle::Draining => MachineLifecycle::Active,
                    MachineLifecycle::Active => MachineLifecycle::Draining,
                };
                if let Err(rollback) =
                    record_lifecycle_evidence(&self.evidence_file, &machine_id, revert)
                {
                    eprintln!(
                        "failed to roll back lifecycle evidence after state commit failure: {rollback}"
                    );
                }
            }
            self.record_state_commit_failed(&operation_id, &machine_id, &error.to_string())
                .await;
            return;
        }

        self.record_terminal(
            &operation_id,
            &machine_id,
            MachineLifecycleTransition::Completed,
        )
        .await;
    }

    async fn record_state_commit_failed(
        &self,
        operation_id: &ployz_core::ids::OperationId,
        machine_id: &MachineId,
        message: &str,
    ) {
        self.record_terminal(
            operation_id,
            machine_id,
            MachineLifecycleTransition::Failed {
                failure: MachineLifecycleFailure::StateCommitFailed {
                    message: FailureMessage::try_new(format!(
                        "failed to commit machine lifecycle: {message}"
                    ))
                    .expect("state commit failure message is non-empty"),
                },
            },
        )
        .await;
    }

    async fn record_terminal(
        &self,
        operation_id: &ployz_core::ids::OperationId,
        machine_id: &MachineId,
        transition: MachineLifecycleTransition,
    ) {
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_lifecycle_transition(operation_id, machine_id, transition)
            .await
        {
            eprintln!("failed to record machine-lifecycle terminal event: {error}");
        }
    }
}

/// The on-disk shape: only non-default intent is recorded; an absent machine
/// is active.
#[derive(Debug, Default, Serialize, Deserialize)]
struct MachineLifecycleEvidence {
    draining: BTreeSet<String>,
}

/// Returns whether the file changed: an idempotent re-drain or re-resume
/// leaves it untouched, so a failed KV commit after an unchanged write needs
/// no evidence rollback. Callers serialize through the runtime's evidence
/// lock — this read-modify-write is not safe concurrently.
fn record_lifecycle_evidence(
    path: &Path,
    machine_id: &MachineId,
    target: MachineLifecycle,
) -> Result<bool, FailureMessage> {
    let mut evidence = read_lifecycle_evidence(path)?;
    let changed = match target {
        MachineLifecycle::Draining => evidence.draining.insert(machine_id.as_str().to_owned()),
        MachineLifecycle::Active => evidence.draining.remove(machine_id.as_str()),
    };
    if !changed {
        return Ok(false);
    }

    let payload = serde_json::to_vec_pretty(&evidence)
        .map_err(|error| failure(format!("encode lifecycle evidence: {error}")))?;
    let temp_path = path.with_extension("tmp");
    let mut file = std::fs::File::create(&temp_path)
        .map_err(|error| failure(format!("create lifecycle evidence temp file: {error}")))?;
    file.write_all(&payload)
        .and_then(|()| file.sync_all())
        .map_err(|error| failure(format!("write lifecycle evidence: {error}")))?;
    std::fs::rename(&temp_path, path)
        .map_err(|error| failure(format!("commit lifecycle evidence: {error}")))?;
    Ok(true)
}

fn read_lifecycle_evidence(path: &Path) -> Result<MachineLifecycleEvidence, FailureMessage> {
    let payload = match std::fs::read(path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MachineLifecycleEvidence::default());
        }
        Err(error) => return Err(failure(format!("read lifecycle evidence: {error}"))),
    };
    serde_json::from_slice(&payload)
        .map_err(|error| failure(format!("decode lifecycle evidence: {error}")))
}

fn failure(message: String) -> FailureMessage {
    FailureMessage::try_new(message).expect("lifecycle evidence failure message is non-empty")
}

/// Adopt-on-start: re-applies the drained set from disk into the KV machine
/// records, so operator intent survives JetStream loss. Machines in the file
/// without a KV record stay pending until the record is rebuilt; adoption is
/// idempotent.
pub async fn adopt_machine_lifecycles_from_file(
    path: &Path,
    core_state: &AsyncNatsCoreStateStore,
) -> Result<(), FailureMessage> {
    let evidence = read_lifecycle_evidence(path)?;
    for machine in &evidence.draining {
        let machine_id = MachineId::try_new(machine.clone())
            .map_err(|error| failure(format!("lifecycle evidence machine id: {error}")))?;
        let Some(mut active) = core_state
            .active_machine(&machine_id)
            .await
            .map_err(|error| failure(format!("read machine for lifecycle adoption: {error}")))?
        else {
            continue;
        };
        if active.lifecycle == MachineLifecycle::Draining {
            continue;
        }
        active.lifecycle = MachineLifecycle::Draining;
        core_state
            .replace_active_machine(&active)
            .await
            .map_err(|error| failure(format!("adopt machine lifecycle: {error}")))?;
    }
    Ok(())
}
