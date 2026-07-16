use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use ployz_core::build::build_control_request_timeout;
use ployz_core::deploy::PushedImageReceipt;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{
    BuildCleanupEvidence, BuildEvidence, BuildOperationFailure, BuildPlatformFailure,
    BuildTimeoutFailure, BuildTransition, CancellationReason, EventSequence, FailureMessage,
};
use ployz_nats::subjects::{MachineServiceEndpoint, machine_build_log};
use tokio::sync::Mutex;

use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::machine::{
    MachineCallError, NatsMachineFactsReader, call_machine, read_machine_placement_facts,
};
use crate::control::role_client::machine_convergence::gather_dataplane_statuses;
use crate::control::sequencer::{AcceptedBuildExecution, OperationControllers};
use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::roles::machine::protocol::{
    MachineBuildCancelDomainError, MachineBuildCancelOutcome, MachineBuildCancelRpcOk,
    MachineBuildCancelRpcRequest, MachineBuildCleanupOutcome, MachineBuildLogFrame,
    MachineBuildStartDomainError, MachineBuildStartRpcOk, MachineBuildStartRpcRequest,
};
use crate::tasks::TaskSpawner;

use super::log_stream::{
    LogBeforeDeadline, MachineCallOrLog, next_log_before_deadline, next_machine_call_or_log,
};
use super::place_build_platforms;

const BUILD_LOG_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const BUILD_CANCEL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(crate) struct BuildOperationDriver {
    client: async_nats::Client,
    facts: NatsMachineFactsReader,
    intent: NatsIntentReader,
    controllers: OperationControllers,
    timeout: Duration,
    tasks: TaskSpawner,
    active: Arc<Mutex<BTreeMap<OperationId, ActiveBuild>>>,
}

#[derive(Clone)]
enum ActiveBuild {
    Running {
        started: BTreeSet<MachineId>,
        cancellation_reason: Option<CancellationReason>,
        watch_start_sequence: EventSequence,
    },
    Finalizing {
        started: BTreeSet<MachineId>,
    },
}

pub(crate) enum BuildCancelDisposition {
    Accepted { watch_start_sequence: EventSequence },
    NoSuchOperation,
    AlreadyTerminal,
}

impl BuildOperationDriver {
    pub(crate) fn new(
        client: async_nats::Client,
        facts: NatsMachineFactsReader,
        intent: NatsIntentReader,
        controllers: OperationControllers,
        timeout: Duration,
        tasks: TaskSpawner,
    ) -> Self {
        Self {
            client,
            facts,
            intent,
            controllers,
            timeout,
            tasks,
            active: Arc::default(),
        }
    }

    pub(crate) async fn start(&self, accepted: AcceptedBuildExecution) {
        if !accepted.submission.should_start_execution {
            return;
        }
        self.active.lock().await.insert(
            accepted.submission.operation_id.clone(),
            ActiveBuild::Running {
                started: BTreeSet::new(),
                cancellation_reason: None,
                watch_start_sequence: accepted.submission.start_sequence,
            },
        );
        let driver = self.clone();
        let operation_id = accepted.submission.operation_id.clone();
        let admission = self.tasks.spawn(move || async move {
            driver.run(accepted).await;
        });
        let Err(error) = admission else {
            return;
        };
        match claim_rejected_admission(&self.active, &operation_id).await {
            RejectedAdmissionOutcome::Cancelled(reason) => {
                if let Err(record_error) = self
                    .controllers
                    .repository()
                    .record_build_transition(
                        &operation_id,
                        BuildTransition::Cancelled {
                            reason,
                            cleanup: BuildCleanupEvidence::NotRequired,
                        },
                    )
                    .await
                {
                    eprintln!(
                        "build {} cancellation after rejected task admission could not be recorded: {record_error}",
                        operation_id.as_str()
                    );
                }
            }
            RejectedAdmissionOutcome::Interrupted => {
                super::super::finish_rejected_task_admission(
                    &self.controllers,
                    &operation_id,
                    Err(error),
                )
                .await;
            }
        }
        self.active.lock().await.remove(&operation_id);
    }

    pub(crate) async fn cancel(
        &self,
        operation_id: &OperationId,
        reason: CancellationReason,
    ) -> Result<BuildCancelDisposition, String> {
        let (machines, watch_start_sequence) =
            {
                let mut active = self.active.lock().await;
                match active.get_mut(operation_id) {
                    Some(ActiveBuild::Running {
                        started,
                        cancellation_reason,
                        watch_start_sequence,
                    }) => {
                        if cancellation_reason.is_none() {
                            *cancellation_reason = Some(reason);
                        }
                        (started.clone(), *watch_start_sequence)
                    }
                    Some(ActiveBuild::Finalizing { .. }) => {
                        return Ok(BuildCancelDisposition::AlreadyTerminal);
                    }
                    None => {
                        drop(active);
                        let status = self
                            .controllers
                            .repository()
                            .get(operation_id)
                            .await
                            .map_err(|error| error.to_string())?;
                        return Ok(match status {
                            Some(ployz_core::operation::OperationStatus::Build {
                                state, ..
                            }) if state.is_terminal() => BuildCancelDisposition::AlreadyTerminal,
                            Some(ployz_core::operation::OperationStatus::Build { .. }) => {
                                return Err(format!(
                                    "build {} is nonterminal but has no active execution",
                                    operation_id.as_str()
                                ));
                            }
                            _ => BuildCancelDisposition::NoSuchOperation,
                        });
                    }
                }
            };
        futures_util::future::join_all(machines.iter().map(|machine_id| async move {
            let deadline = tokio::time::Instant::now() + BUILD_CANCEL_TIMEOUT;
            while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now())
            {
                let request = MachineBuildCancelRpcRequest {
                    operation_id: operation_id.clone(),
                };
                let result =
                    call_machine::<MachineBuildCancelRpcOk, MachineBuildCancelDomainError>(
                        &self.client,
                        remaining,
                        machine_id,
                        MachineServiceEndpoint::BuildCancel,
                        &request,
                    )
                    .await;
                if !matches!(
                    result,
                    Ok(MachineBuildCancelRpcOk {
                        outcome: MachineBuildCancelOutcome::NotRunning,
                        ..
                    })
                ) {
                    break;
                }
                let Some(delay) = cancel_retry_delay(tokio::time::Instant::now(), deadline) else {
                    break;
                };
                tokio::time::sleep(delay).await;
            }
        }))
        .await;
        Ok(BuildCancelDisposition::Accepted {
            watch_start_sequence,
        })
    }

    async fn run(self, accepted: AcceptedBuildExecution) {
        let id = accepted.submission.operation_id.clone();
        let result = self.run_inner(&accepted).await;
        if let Err(failure) = result {
            let transition = match claim_finalization(&self.active, &id).await {
                Some(reason) => BuildTransition::Cancelled {
                    reason,
                    cleanup: cleanup_for(&id, &self.active, Vec::new()).await,
                },
                None => BuildTransition::Failed { failure },
            };
            let _ = self
                .controllers
                .repository()
                .record_build_transition(&id, transition)
                .await;
        }
        self.active.lock().await.remove(&id);
    }

    async fn run_inner(
        &self,
        accepted: &AcceptedBuildExecution,
    ) -> Result<(), BuildOperationFailure> {
        let id = &accepted.submission.operation_id;
        self.controllers
            .repository()
            .record_build_transition(id, BuildTransition::Placing)
            .await
            .map_err(record_failure)?;
        let intent = self.intent.intent().await.map_err(|error| {
            BuildOperationFailure::ControlUnavailable {
                message: failure_message(error),
            }
        })?;
        let projection = intent.dataplane_projection;
        let facts = read_machine_placement_facts(
            &self.facts,
            intent
                .active_machines
                .into_iter()
                .map(|machine| (machine.machine_id, machine.lifecycle)),
        )
        .await;
        let dataplane_statuses = gather_dataplane_statuses(
            &self.facts,
            projection
                .declared_members()
                .iter()
                .map(|member| &member.machine_id),
        )
        .await;
        let placement = place_build_platforms(
            &accepted.submission.platforms,
            &facts,
            &projection,
            &dataplane_statuses,
        )
        .map_err(|failure| *failure)?;
        for (platform, machine_id) in placement.iter() {
            self.controllers
                .repository()
                .record_build_evidence(
                    id,
                    BuildEvidence::PlatformPlaced {
                        platform: platform.clone(),
                        machine_id: machine_id.clone(),
                    },
                )
                .await
                .map_err(record_failure)?;
        }
        self.controllers
            .repository()
            .record_build_transition(id, BuildTransition::Building)
            .await
            .map_err(record_failure)?;
        let outcomes =
            futures_util::future::join_all(placement.iter().map(|(platform, machine_id)| {
                self.run_platform(accepted, platform.clone(), machine_id.clone())
            }))
            .await;
        let mut images = Vec::new();
        let mut cancelled = Vec::new();
        let mut timed_out = Vec::new();
        let mut first_failure = None;
        for outcome in outcomes {
            match outcome {
                Err(failure) => return Err(failure),
                Ok(PlatformOutcome::Completed { platform, image }) => {
                    images.push((platform, image))
                }
                Ok(PlatformOutcome::Failed(failure)) => {
                    first_failure.get_or_insert(failure);
                }
                Ok(PlatformOutcome::Cancelled {
                    machine_id,
                    cleanup,
                }) => cancelled.push((machine_id, cleanup)),
                Ok(PlatformOutcome::CancelledBeforeStart) => {}
                Ok(PlatformOutcome::TimedOut {
                    machine_id,
                    message,
                    cleanup,
                }) => timed_out.push((machine_id, message, cleanup)),
            }
        }
        let cancellation_reason = claim_finalization(&self.active, id).await;
        if !cancelled.is_empty() || cancellation_reason.is_some() {
            let reason = cancellation_reason.unwrap_or_else(|| {
                CancellationReason::try_new("machine cancelled build").expect("reason")
            });
            let cleanup = cleanup_for(
                id,
                &self.active,
                cancelled
                    .into_iter()
                    .filter_map(|(machine_id, cleanup)| {
                        (cleanup == MachineBuildCleanupOutcome::Confirmed).then_some(machine_id)
                    })
                    .collect(),
            )
            .await;
            self.controllers
                .repository()
                .record_build_transition(id, BuildTransition::Cancelled { reason, cleanup })
                .await
                .map_err(record_failure)?;
            return Ok(());
        }
        if let Some(message) = timed_out.first().map(|(_, message, _)| message.clone()) {
            let cleanup = timeout_cleanup(&timed_out);
            self.controllers
                .repository()
                .record_build_transition(
                    id,
                    BuildTransition::TimedOut {
                        failure: BuildTimeoutFailure::DeadlineExceeded { message },
                        cleanup,
                    },
                )
                .await
                .map_err(record_failure)?;
            return Ok(());
        }
        let receipt = assemble_receipt(images, first_failure).map_err(|failure| *failure)?;
        self.controllers
            .repository()
            .record_build_transition(id, BuildTransition::Completed { receipt })
            .await
            .map_err(record_failure)?;
        Ok(())
    }

    async fn run_platform(
        &self,
        accepted: &AcceptedBuildExecution,
        platform: ployz_core::image::OciPlatform,
        machine_id: MachineId,
    ) -> Result<PlatformOutcome, BuildOperationFailure> {
        let id = &accepted.submission.operation_id;
        if !claim_machine_start(&self.active, id, &machine_id).await {
            return Ok(PlatformOutcome::CancelledBeforeStart);
        }
        let subject = machine_build_log(&machine_id, id);
        let mut logs = self.client.subscribe(subject).await.map_err(|error| {
            platform_failure(
                platform.clone(),
                machine_id.clone(),
                BuildPlatformFailure::MachineUnavailable {
                    message: failure_message(error),
                },
            )
        })?;
        let request = build_start_request(accepted, platform.clone(), self.timeout);
        if !machine_start_is_authorized(&self.active, id, &machine_id).await {
            release_machine_start_claim(&self.active, id, &machine_id).await;
            return Ok(PlatformOutcome::CancelledBeforeStart);
        }
        let call_machine_id = machine_id.clone();
        let machine_call = call_machine::<MachineBuildStartRpcOk, MachineBuildStartDomainError>(
            &self.client,
            build_control_request_timeout(self.timeout),
            &call_machine_id,
            MachineServiceEndpoint::BuildStart,
            &request,
        );
        tokio::pin!(machine_call);
        let mut next = 1;
        let mut logs_open = true;
        let result = loop {
            match next_machine_call_or_log(machine_call.as_mut(), &mut logs, logs_open).await {
                MachineCallOrLog::Call(result) => break result,
                MachineCallOrLog::LogsClosed => logs_open = false,
                MachineCallOrLog::Log(message) => {
                    let Some(message) = message else {
                        logs_open = false;
                        continue;
                    };
                    let Ok(frame) =
                        serde_json::from_slice::<MachineBuildLogFrame>(&message.payload)
                    else {
                        continue;
                    };
                    if valid_next_log_frame(id, &machine_id, &platform, next, &frame) {
                        self.controllers
                            .repository()
                            .record_build_evidence(
                                id,
                                BuildEvidence::PlatformLog {
                                    platform: platform.clone(),
                                    machine_id: machine_id.clone(),
                                    chunk: frame.chunk,
                                },
                            )
                            .await
                            .map_err(record_failure)?;
                        next += 1;
                    }
                }
            }
        };
        let summary = match result {
            Ok(ok) => BuildSummary::Completed(ok),
            Err(MachineCallError::Domain(MachineBuildStartDomainError::PlatformFailed {
                failure,
                final_log_sequence,
                omitted_log_bytes,
            })) => BuildSummary::Failed {
                failure,
                final_log_sequence,
                omitted_log_bytes,
            },
            Err(MachineCallError::Domain(MachineBuildStartDomainError::Cancelled {
                cleanup,
                final_log_sequence,
                omitted_log_bytes,
            })) => BuildSummary::Cancelled {
                cleanup,
                final_log_sequence,
                omitted_log_bytes,
            },
            Err(MachineCallError::Domain(MachineBuildStartDomainError::TimedOut {
                message,
                cleanup,
                final_log_sequence,
                omitted_log_bytes,
            })) => BuildSummary::TimedOut {
                message,
                cleanup,
                final_log_sequence,
                omitted_log_bytes,
            },
            Err(MachineCallError::Unavailable(
                reason @ (MachineRuntimeUnavailableReason::RequestTimedOut
                | MachineRuntimeUnavailableReason::ServiceTimedOut { .. }),
            )) => BuildSummary::TimedOut {
                message: reason.failure_message(),
                cleanup: MachineBuildCleanupOutcome::Unconfirmed,
                final_log_sequence: 0,
                omitted_log_bytes: 0,
            },
            Err(error) => {
                let operation_failure =
                    machine_failure(platform.clone(), machine_id.clone(), error);
                let BuildOperationFailure::PlatformFailed { failure, .. } = &operation_failure
                else {
                    unreachable!("machine failure is platform-scoped")
                };
                self.controllers
                    .repository()
                    .record_build_evidence(
                        id,
                        BuildEvidence::PlatformFailed {
                            platform,
                            machine_id,
                            failure: failure.clone(),
                        },
                    )
                    .await
                    .map_err(record_failure)?;
                return Ok(PlatformOutcome::Failed(operation_failure));
            }
        };
        let (final_log_sequence, omitted_log_bytes) = summary.log_summary();
        let drain_deadline = tokio::time::sleep(BUILD_LOG_DRAIN_TIMEOUT);
        tokio::pin!(drain_deadline);
        while next <= final_log_sequence {
            let message =
                match next_log_before_deadline(drain_deadline.as_mut(), &mut logs, logs_open).await
                {
                    LogBeforeDeadline::Deadline | LogBeforeDeadline::LogsClosed => break,
                    LogBeforeDeadline::Log(message) => message,
                };
            let Some(message) = message else {
                break;
            };
            let Ok(frame) = serde_json::from_slice::<MachineBuildLogFrame>(&message.payload) else {
                continue;
            };
            if valid_next_log_frame(id, &machine_id, &platform, next, &frame) {
                self.controllers
                    .repository()
                    .record_build_evidence(
                        id,
                        BuildEvidence::PlatformLog {
                            platform: platform.clone(),
                            machine_id: machine_id.clone(),
                            chunk: frame.chunk,
                        },
                    )
                    .await
                    .map_err(record_failure)?;
                next += 1;
            }
        }
        if next <= final_log_sequence {
            self.controllers
                .repository()
                .record_build_evidence(
                    id,
                    BuildEvidence::PlatformLogGap {
                        platform: platform.clone(),
                        machine_id: machine_id.clone(),
                        expected_sequence: next,
                        final_sequence: final_log_sequence,
                    },
                )
                .await
                .map_err(record_failure)?;
        }
        if omitted_log_bytes > 0 {
            self.controllers
                .repository()
                .record_build_evidence(
                    id,
                    BuildEvidence::PlatformLogTruncated {
                        platform: platform.clone(),
                        machine_id: machine_id.clone(),
                        omitted_bytes: omitted_log_bytes,
                    },
                )
                .await
                .map_err(record_failure)?;
        }
        let BuildSummary::Completed(ok) = summary else {
            return Ok(match summary {
                BuildSummary::Failed { failure, .. } => {
                    let operation_failure =
                        platform_failure(platform.clone(), machine_id.clone(), failure.clone());
                    if let Err(error) = self
                        .controllers
                        .repository()
                        .record_build_evidence(
                            id,
                            BuildEvidence::PlatformFailed {
                                platform,
                                machine_id,
                                failure,
                            },
                        )
                        .await
                    {
                        return Err(record_failure(error));
                    }
                    PlatformOutcome::Failed(operation_failure)
                }
                BuildSummary::Cancelled { cleanup, .. } => PlatformOutcome::Cancelled {
                    machine_id,
                    cleanup,
                },
                BuildSummary::TimedOut {
                    message, cleanup, ..
                } => PlatformOutcome::TimedOut {
                    machine_id,
                    message,
                    cleanup,
                },
                BuildSummary::Completed(_) => unreachable!(),
            });
        };
        self.controllers
            .repository()
            .record_build_evidence(
                id,
                BuildEvidence::VerifiedCommit {
                    platform: platform.clone(),
                    machine_id: machine_id.clone(),
                    commit: ok.verified_commit,
                },
            )
            .await
            .map_err(record_failure)?;
        self.controllers
            .repository()
            .record_build_evidence(
                id,
                BuildEvidence::ToolchainVerified {
                    platform: platform.clone(),
                    machine_id: machine_id.clone(),
                    toolchain: ok.toolchain,
                },
            )
            .await
            .map_err(record_failure)?;
        self.controllers
            .repository()
            .record_build_evidence(
                id,
                BuildEvidence::PlatformCompleted {
                    platform: platform.clone(),
                    machine_id: machine_id.clone(),
                    image: ok.image.clone(),
                },
            )
            .await
            .map_err(record_failure)?;
        Ok(PlatformOutcome::Completed {
            platform,
            image: ok.image,
        })
    }
}

fn assemble_receipt(
    images: Vec<(
        ployz_core::image::OciPlatform,
        ployz_core::deploy::PlatformImage,
    )>,
    first_failure: Option<BuildOperationFailure>,
) -> Result<PushedImageReceipt, Box<BuildOperationFailure>> {
    if let Some(failure) = first_failure {
        return Err(Box::new(failure));
    }
    PushedImageReceipt::try_new(images)
        .map_err(|error| Box::new(receipt_failure(error.to_string())))
}

async fn claim_machine_start(
    active: &Mutex<BTreeMap<OperationId, ActiveBuild>>,
    operation_id: &OperationId,
    machine_id: &MachineId,
) -> bool {
    let mut active = active.lock().await;
    let Some(build) = active.get_mut(operation_id) else {
        return false;
    };
    match build {
        ActiveBuild::Running {
            started,
            cancellation_reason: None,
            watch_start_sequence: _,
        } => {
            started.insert(machine_id.clone());
            true
        }
        ActiveBuild::Running {
            cancellation_reason: Some(_),
            ..
        }
        | ActiveBuild::Finalizing { .. } => false,
    }
}

async fn machine_start_is_authorized(
    active: &Mutex<BTreeMap<OperationId, ActiveBuild>>,
    operation_id: &OperationId,
    machine_id: &MachineId,
) -> bool {
    let active = active.lock().await;
    matches!(
        active.get(operation_id),
        Some(ActiveBuild::Running {
            started,
            cancellation_reason: None,
            watch_start_sequence: _,
        }) if started.contains(machine_id)
    )
}

async fn release_machine_start_claim(
    active: &Mutex<BTreeMap<OperationId, ActiveBuild>>,
    operation_id: &OperationId,
    machine_id: &MachineId,
) {
    let mut active = active.lock().await;
    if let Some(ActiveBuild::Running { started, .. }) = active.get_mut(operation_id) {
        started.remove(machine_id);
    }
}

fn cancel_retry_delay(
    now: tokio::time::Instant,
    deadline: tokio::time::Instant,
) -> Option<Duration> {
    let remaining = deadline.checked_duration_since(now)?;
    (!remaining.is_zero()).then(|| remaining.min(Duration::from_millis(25)))
}

async fn claim_finalization(
    active: &Mutex<BTreeMap<OperationId, ActiveBuild>>,
    operation_id: &OperationId,
) -> Option<CancellationReason> {
    let mut active = active.lock().await;
    let build = active.get_mut(operation_id)?;
    match build {
        ActiveBuild::Running {
            started,
            cancellation_reason: Some(reason),
            watch_start_sequence: _,
        } => {
            let reason = reason.clone();
            let started = started.clone();
            *build = ActiveBuild::Finalizing { started };
            Some(reason)
        }
        ActiveBuild::Running {
            started,
            cancellation_reason: None,
            watch_start_sequence: _,
        } => {
            let started = started.clone();
            *build = ActiveBuild::Finalizing { started };
            None
        }
        ActiveBuild::Finalizing { .. } => None,
    }
}

enum RejectedAdmissionOutcome {
    Cancelled(CancellationReason),
    Interrupted,
}

async fn claim_rejected_admission(
    active: &Mutex<BTreeMap<OperationId, ActiveBuild>>,
    operation_id: &OperationId,
) -> RejectedAdmissionOutcome {
    let mut active = active.lock().await;
    let Some(build) = active.get_mut(operation_id) else {
        return RejectedAdmissionOutcome::Interrupted;
    };
    match build {
        ActiveBuild::Running {
            cancellation_reason: Some(reason),
            ..
        } => {
            let reason = reason.clone();
            *build = ActiveBuild::Finalizing {
                started: BTreeSet::new(),
            };
            RejectedAdmissionOutcome::Cancelled(reason)
        }
        ActiveBuild::Running { .. } | ActiveBuild::Finalizing { .. } => {
            *build = ActiveBuild::Finalizing {
                started: BTreeSet::new(),
            };
            RejectedAdmissionOutcome::Interrupted
        }
    }
}

fn build_start_request(
    accepted: &AcceptedBuildExecution,
    platform: ployz_core::image::OciPlatform,
    timeout: Duration,
) -> MachineBuildStartRpcRequest {
    MachineBuildStartRpcRequest {
        operation_id: accepted.submission.operation_id.clone(),
        source: accepted.source.clone(),
        adapter: accepted.submission.adapter.clone(),
        platform,
        timeout_millis: execution_timeout_millis(timeout),
    }
}

fn execution_timeout_millis(timeout: Duration) -> u64 {
    timeout.as_millis().try_into().unwrap_or(u64::MAX)
}

fn valid_next_log_frame(
    operation_id: &OperationId,
    machine_id: &MachineId,
    platform: &ployz_core::image::OciPlatform,
    next: u64,
    frame: &MachineBuildLogFrame,
) -> bool {
    frame.operation_id == *operation_id
        && frame.machine_id == *machine_id
        && frame.platform == *platform
        && frame.sequence == next
}

enum BuildSummary {
    Completed(MachineBuildStartRpcOk),
    Failed {
        failure: BuildPlatformFailure,
        final_log_sequence: u64,
        omitted_log_bytes: u64,
    },
    Cancelled {
        cleanup: MachineBuildCleanupOutcome,
        final_log_sequence: u64,
        omitted_log_bytes: u64,
    },
    TimedOut {
        message: FailureMessage,
        cleanup: MachineBuildCleanupOutcome,
        final_log_sequence: u64,
        omitted_log_bytes: u64,
    },
}

impl BuildSummary {
    fn log_summary(&self) -> (u64, u64) {
        match self {
            Self::Completed(ok) => (ok.final_log_sequence, ok.omitted_log_bytes),
            Self::Failed {
                final_log_sequence,
                omitted_log_bytes,
                ..
            }
            | Self::Cancelled {
                cleanup: _,
                final_log_sequence,
                omitted_log_bytes,
            }
            | Self::TimedOut {
                final_log_sequence,
                omitted_log_bytes,
                ..
            } => (*final_log_sequence, *omitted_log_bytes),
        }
    }
}

enum PlatformOutcome {
    Completed {
        platform: ployz_core::image::OciPlatform,
        image: ployz_core::deploy::PlatformImage,
    },
    Failed(BuildOperationFailure),
    Cancelled {
        machine_id: MachineId,
        cleanup: MachineBuildCleanupOutcome,
    },
    CancelledBeforeStart,
    TimedOut {
        machine_id: MachineId,
        message: FailureMessage,
        cleanup: MachineBuildCleanupOutcome,
    },
}

fn timeout_cleanup(
    outcomes: &[(MachineId, FailureMessage, MachineBuildCleanupOutcome)],
) -> BuildCleanupEvidence {
    let unconfirmed = outcomes
        .iter()
        .filter(|(_, _, cleanup)| *cleanup == MachineBuildCleanupOutcome::Unconfirmed)
        .map(|(machine_id, _, _)| machine_id.clone())
        .collect::<Vec<_>>();
    if unconfirmed.is_empty() {
        BuildCleanupEvidence::Completed {
            machine_ids: outcomes
                .iter()
                .map(|(machine_id, _, _)| machine_id.clone())
                .collect(),
        }
    } else {
        BuildCleanupEvidence::Unconfirmed {
            machine_ids: unconfirmed,
        }
    }
}

async fn cleanup_for(
    operation_id: &OperationId,
    active: &Mutex<BTreeMap<OperationId, ActiveBuild>>,
    confirmed: Vec<MachineId>,
) -> BuildCleanupEvidence {
    let started = active
        .lock()
        .await
        .get(operation_id)
        .map(|build| match build {
            ActiveBuild::Running { started, .. } | ActiveBuild::Finalizing { started } => {
                started.clone()
            }
        })
        .unwrap_or_default();
    if started.is_empty() {
        return BuildCleanupEvidence::NotRequired;
    }
    let confirmed: BTreeSet<_> = confirmed.into_iter().collect();
    let unconfirmed = started.difference(&confirmed).cloned().collect::<Vec<_>>();
    if unconfirmed.is_empty() {
        BuildCleanupEvidence::Completed {
            machine_ids: confirmed.into_iter().collect(),
        }
    } else {
        BuildCleanupEvidence::Unconfirmed {
            machine_ids: unconfirmed,
        }
    }
}

fn platform_failure(
    platform: ployz_core::image::OciPlatform,
    machine_id: MachineId,
    failure: BuildPlatformFailure,
) -> BuildOperationFailure {
    BuildOperationFailure::PlatformFailed {
        platform,
        machine_id,
        failure,
    }
}

fn failure_message(error: impl std::fmt::Display) -> FailureMessage {
    FailureMessage::try_new(error.to_string()).expect("error is non-empty")
}

fn record_failure(error: impl std::fmt::Display) -> BuildOperationFailure {
    BuildOperationFailure::EvidenceRecordingFailed {
        message: failure_message(error),
    }
}
fn receipt_failure(message: String) -> BuildOperationFailure {
    BuildOperationFailure::ReceiptAssemblyFailed {
        message: FailureMessage::try_new(message).expect("error is non-empty"),
    }
}
fn machine_failure(
    platform: ployz_core::image::OciPlatform,
    machine_id: MachineId,
    error: MachineCallError<MachineBuildStartDomainError>,
) -> BuildOperationFailure {
    let failure = match error {
        MachineCallError::Unavailable(reason) => BuildPlatformFailure::MachineUnavailable {
            message: reason.failure_message(),
        },
        MachineCallError::Domain(MachineBuildStartDomainError::PlatformFailed {
            failure, ..
        }) => failure,
        MachineCallError::Domain(other) => BuildPlatformFailure::AdapterFailed {
            message: FailureMessage::try_new(format!("{other:?}")).expect("error"),
        },
    };
    BuildOperationFailure::PlatformFailed {
        platform,
        machine_id,
        failure,
    }
}

#[cfg(test)]
mod tests;
