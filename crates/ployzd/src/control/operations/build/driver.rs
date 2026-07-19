use std::future::Future;
use std::time::Duration;

use ployz_core::build::{
    BUILD_MAX_PLACEMENT_TIMEOUT, BuildExecutorOrigin, BuildTarget, build_control_request_timeout,
};
use ployz_core::deploy::PushedImageReceipt;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{
    BuildCleanupEvidence, BuildEvidence, BuildOperationFailure, BuildPlatformFailure,
    BuildTimeoutFailure, BuildTransition, CancellationReason, EventSequence, FailureMessage,
    OperationStatus,
};
use ployz_nats::subjects::{MachineServiceEndpoint, machine_build_log};

use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::machine::{
    MachineCallError, NatsMachineFactsReader, call_machine, read_machine_placement_facts,
};
use crate::control::role_client::machine_convergence::gather_dataplane_statuses;
use crate::control::sequencer::{AcceptedBuildExecution, OperationControllers};
use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::roles::machine::protocol::{
    BuildExecutorAcceptance, BuildLogSummary, MachineBuildCancelDomainError,
    MachineBuildCancelOutcome, MachineBuildCancelRpcOk, MachineBuildCancelRpcRequest,
    MachineBuildCleanupOutcome, MachineBuildStartDomainError, MachineBuildStartRpcOk,
    MachineBuildStartRpcRequest,
};
use crate::tasks::TaskSpawner;

use super::active_registry::{ActiveBuildRegistry, CancellationRequest, RejectedAdmissionOutcome};
use super::log_stream::{MachineCallOrLog, next_machine_call_or_log};
use super::placement::{BuildExecutorAssignment, place_build_platforms};
use super::platform_session::PlatformLogSession;

const BUILD_CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
const BUILD_CANCEL_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);

async fn within_placement_deadline<T>(
    placement: impl Future<Output = Result<T, BuildOperationFailure>>,
) -> Result<T, BuildOperationFailure> {
    tokio::time::timeout(BUILD_MAX_PLACEMENT_TIMEOUT, placement)
        .await
        .map_err(|_| BuildOperationFailure::ControlUnavailable {
            message: FailureMessage::try_new("build placement exceeded its 180-second deadline")
                .expect("placement timeout failure is non-empty"),
        })?
}

#[derive(Clone)]
pub(crate) struct BuildOperationDriver {
    client: async_nats::Client,
    facts: NatsMachineFactsReader,
    intent: NatsIntentReader,
    controllers: OperationControllers,
    timeout: Duration,
    tasks: TaskSpawner,
    active: ActiveBuildRegistry,
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
            active: ActiveBuildRegistry::default(),
        }
    }

    pub(crate) async fn start(&self, accepted: AcceptedBuildExecution) {
        if !accepted.submission.should_start_execution {
            return;
        }
        self.active
            .start(
                accepted.submission.operation_id.clone(),
                accepted.submission.start_sequence,
            )
            .await;
        let driver = self.clone();
        let operation_id = accepted.submission.operation_id.clone();
        let admission = self.tasks.spawn(move || async move {
            driver.run(accepted).await;
        });
        let Err(error) = admission else {
            return;
        };
        match self.active.claim_rejected_admission(&operation_id).await {
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
        self.active.remove(&operation_id).await;
    }

    pub(crate) async fn cancel(
        &self,
        operation_id: &OperationId,
        reason: CancellationReason,
    ) -> Result<BuildCancelDisposition, String> {
        let (machines, watch_start_sequence) =
            match self.active.request_cancellation(operation_id, reason).await {
                CancellationRequest::Accepted {
                    machines,
                    watch_start_sequence,
                } => (machines, watch_start_sequence),
                CancellationRequest::Finalizing => {
                    return Ok(BuildCancelDisposition::AlreadyTerminal);
                }
                CancellationRequest::Missing => {
                    let status = self
                        .controllers
                        .repository()
                        .get(operation_id)
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(match status {
                        Some(OperationStatus::Build { state, .. }) if state.is_terminal() => {
                            BuildCancelDisposition::AlreadyTerminal
                        }
                        Some(OperationStatus::Build { .. }) => {
                            return Err(format!(
                                "build {} is nonterminal but has no active execution",
                                operation_id.as_str()
                            ));
                        }
                        None
                        | Some(OperationStatus::Deploy { .. })
                        | Some(OperationStatus::Cert { .. })
                        | Some(OperationStatus::MachineAdd { .. })
                        | Some(OperationStatus::MachineUpdate { .. })
                        | Some(OperationStatus::MachineBuildCachePrune { .. })
                        | Some(OperationStatus::MachineStoragePrepare { .. })
                        | Some(OperationStatus::MachineLifecycle { .. })
                        | Some(OperationStatus::CoreReplace { .. })
                        | Some(OperationStatus::CredentialGrant { .. })
                        | Some(OperationStatus::NetworkRepair { .. })
                        | Some(OperationStatus::ServiceRestart { .. })
                        | Some(OperationStatus::ManagedDnsReconcile { .. })
                        | Some(OperationStatus::IngressConfigure { .. })
                        | Some(OperationStatus::NamespaceRemove { .. })
                        | Some(OperationStatus::VolumeCreate { .. })
                        | Some(OperationStatus::VolumeRemove { .. }) => {
                            BuildCancelDisposition::NoSuchOperation
                        }
                    });
                }
            };
        futures_util::future::join_all(machines.iter().map(|machine_id| async move {
            let deadline = tokio::time::Instant::now() + BUILD_CANCEL_TIMEOUT;
            deliver_build_cancel_to_machine(machine_id, deadline, |request_timeout| {
                let request = MachineBuildCancelRpcRequest {
                    operation_id: operation_id.clone(),
                    origin: BuildExecutorOrigin::Cluster {
                        machine_id: machine_id.clone(),
                    },
                    image_seed: machine_id.clone(),
                };
                async move {
                    call_machine::<MachineBuildCancelRpcOk, MachineBuildCancelDomainError>(
                        &self.client,
                        request_timeout,
                        machine_id,
                        MachineServiceEndpoint::BuildCancel,
                        &request,
                    )
                    .await
                }
            })
            .await;
        }))
        .await;
        Ok(BuildCancelDisposition::Accepted {
            watch_start_sequence,
        })
    }

    async fn run(self, accepted: AcceptedBuildExecution) {
        let id = accepted.submission.operation_id.clone();
        let result = self.run_inner(&accepted).await;
        self.record_run_result(&id, result).await;
        self.active.remove(&id).await;
    }

    async fn record_run_result(&self, id: &OperationId, result: Result<(), BuildOperationFailure>) {
        if let Err(failure) = result {
            let transition = match self.active.claim_finalization(id).await {
                Some(reason) => BuildTransition::Cancelled {
                    reason,
                    cleanup: self.active.cleanup_evidence(id, Vec::new()).await,
                },
                None => BuildTransition::Failed { failure },
            };
            let _ = self
                .controllers
                .repository()
                .record_build_transition(id, transition)
                .await;
        }
    }

    async fn run_inner(
        &self,
        accepted: &AcceptedBuildExecution,
    ) -> Result<(), BuildOperationFailure> {
        let placement = within_placement_deadline(self.place(accepted)).await?;
        let id = &accepted.submission.operation_id;
        let outcomes = futures_util::future::join_all(
            placement
                .into_iter()
                .map(|assignment| self.run_platform(accepted, assignment)),
        )
        .await;
        self.finalize_joined_outcomes(id, outcomes).await
    }

    async fn finalize_joined_outcomes(
        &self,
        id: &OperationId,
        outcomes: Vec<Result<PlatformOutcome, BuildOperationFailure>>,
    ) -> Result<(), BuildOperationFailure> {
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
        let cancellation_reason = self.active.claim_finalization(id).await;
        if !cancelled.is_empty() || cancellation_reason.is_some() {
            let reason = cancellation_reason.unwrap_or_else(|| {
                CancellationReason::try_new("machine cancelled build").expect("reason")
            });
            let cleanup = self
                .active
                .cleanup_evidence(
                    id,
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
        if let Some(failure) = first_failure {
            return Err(failure);
        }
        let receipt = PushedImageReceipt::try_new(images)
            .map_err(|error| receipt_failure(error.to_string()))?;
        self.controllers
            .repository()
            .record_build_transition(id, BuildTransition::Completed { receipt })
            .await
            .map_err(record_failure)?;
        Ok(())
    }

    async fn place(
        &self,
        accepted: &AcceptedBuildExecution,
    ) -> Result<Vec<BuildExecutorAssignment>, BuildOperationFailure> {
        let id = &accepted.submission.operation_id;
        self.controllers
            .repository()
            .record_build_transition(id, BuildTransition::Placing)
            .await
            .map_err(record_failure)?;
        let BuildTarget::Cluster = &accepted.submission.target else {
            return Err(BuildOperationFailure::ControlUnavailable {
                message: FailureMessage::try_new(
                    "external build execution was not admitted before transport dispatch",
                )
                .expect("external dispatch failure is non-empty"),
            });
        };
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
            &accepted.submission.adapter,
            &facts,
            &projection,
            &dataplane_statuses,
        )
        .map_err(|failure| *failure)?;
        for assignment in &placement {
            self.controllers
                .repository()
                .record_build_evidence(
                    id,
                    BuildEvidence::PlatformPlaced {
                        platform: assignment.platform.clone(),
                        machine_id: assignment.image_seed.clone(),
                        executor_origin: assignment.origin.clone(),
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
        Ok(placement)
    }

    async fn run_platform(
        &self,
        accepted: &AcceptedBuildExecution,
        assignment: BuildExecutorAssignment,
    ) -> Result<PlatformOutcome, BuildOperationFailure> {
        let BuildExecutorAssignment {
            origin,
            platform,
            image_seed: machine_id,
        } = assignment;
        let BuildExecutorOrigin::Cluster {
            machine_id: cluster_machine_id,
        } = &origin
        else {
            return Err(BuildOperationFailure::ControlUnavailable {
                message: FailureMessage::try_new(
                    "external build execution transport is not implemented",
                )
                .expect("external dispatch failure is non-empty"),
            });
        };
        if cluster_machine_id != &machine_id {
            return Err(BuildOperationFailure::ControlUnavailable {
                message: FailureMessage::try_new(
                    "cluster build executor and image seed must be the same Machine",
                )
                .expect("cluster assignment failure is non-empty"),
            });
        }
        let id = &accepted.submission.operation_id;
        if !self.active.claim_machine_start(id, &machine_id).await {
            return Ok(PlatformOutcome::CancelledBeforeStart);
        }
        let subject = machine_build_log(&machine_id, id);
        let logs = self.client.subscribe(subject).await.map_err(|error| {
            platform_failure(
                platform.clone(),
                machine_id.clone(),
                BuildPlatformFailure::MachineUnavailable {
                    message: failure_message(error),
                },
            )
        })?;
        let mut log_session = PlatformLogSession::new(
            self.controllers.repository(),
            id,
            &machine_id,
            &platform,
            origin.clone(),
            logs,
        );
        let request = build_start_request(
            accepted,
            origin.clone(),
            machine_id.clone(),
            platform.clone(),
            self.timeout,
        );
        if !self
            .active
            .machine_start_is_authorized(id, &machine_id)
            .await
        {
            self.active
                .release_machine_start_claim(id, &machine_id)
                .await;
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
        let result = loop {
            let logs_open = log_session.logs_open();
            match next_machine_call_or_log(machine_call.as_mut(), log_session.logs_mut(), logs_open)
                .await
            {
                MachineCallOrLog::Call(result) => break result,
                MachineCallOrLog::LogsClosed => log_session.record_message(None).await?,
                MachineCallOrLog::Log(message) => log_session.record_message(message).await?,
            }
        };
        let summary = match result {
            Ok(ok) => BuildSummary::Completed(ok),
            Err(MachineCallError::Domain(MachineBuildStartDomainError::PlatformFailed {
                acceptance,
                failure,
                log_summary,
            })) => BuildSummary::Failed {
                acceptance,
                failure,
                log_summary,
            },
            Err(MachineCallError::Domain(MachineBuildStartDomainError::Cancelled {
                acceptance,
                cleanup,
                log_summary,
            })) => BuildSummary::Cancelled {
                acceptance,
                cleanup,
                log_summary,
            },
            Err(MachineCallError::Domain(MachineBuildStartDomainError::TimedOut {
                acceptance,
                message,
                cleanup,
                log_summary,
            })) => BuildSummary::TimedOut {
                acceptance,
                message,
                cleanup,
                log_summary,
            },
            Err(MachineCallError::Unavailable(
                reason @ (MachineRuntimeUnavailableReason::RequestTimedOut
                | MachineRuntimeUnavailableReason::ServiceTimedOut { .. }),
            )) => {
                log_session.drain(BuildLogSummary::none()).await?;
                return Ok(PlatformOutcome::TimedOut {
                    machine_id,
                    message: reason.failure_message(),
                    cleanup: MachineBuildCleanupOutcome::Unconfirmed,
                });
            }
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
                            executor_origin: origin.clone(),
                            failure: failure.clone(),
                        },
                    )
                    .await
                    .map_err(record_failure)?;
                return Ok(PlatformOutcome::Failed(operation_failure));
            }
        };
        if let Err(failure) = validate_executor_acceptance(
            &expected_acceptance(id, &machine_id, &platform),
            summary.acceptance(),
        ) {
            let operation_failure =
                platform_failure(platform.clone(), machine_id.clone(), failure.clone());
            self.controllers
                .repository()
                .record_build_evidence(
                    id,
                    BuildEvidence::PlatformFailed {
                        platform,
                        machine_id,
                        executor_origin: origin.clone(),
                        failure,
                    },
                )
                .await
                .map_err(record_failure)?;
            return Ok(PlatformOutcome::Failed(operation_failure));
        }
        log_session.drain(summary.log_summary()).await?;
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
                                executor_origin: origin.clone(),
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
        if let Err(failure) = validate_completed_image_seed(&machine_id, &ok) {
            let operation_failure =
                platform_failure(platform.clone(), machine_id.clone(), failure.clone());
            self.controllers
                .repository()
                .record_build_evidence(
                    id,
                    BuildEvidence::PlatformFailed {
                        platform,
                        machine_id,
                        executor_origin: origin.clone(),
                        failure,
                    },
                )
                .await
                .map_err(record_failure)?;
            return Ok(PlatformOutcome::Failed(operation_failure));
        }
        self.controllers
            .repository()
            .record_build_evidence(
                id,
                BuildEvidence::VerifiedCommit {
                    platform: platform.clone(),
                    machine_id: machine_id.clone(),
                    executor_origin: origin.clone(),
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
                    executor_origin: origin.clone(),
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
                    executor_origin: origin,
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

async fn deliver_build_cancel_to_machine<Call, CallFuture>(
    machine_id: &MachineId,
    deadline: tokio::time::Instant,
    mut call: Call,
) where
    Call: FnMut(Duration) -> CallFuture,
    CallFuture: Future<
        Output = Result<MachineBuildCancelRpcOk, MachineCallError<MachineBuildCancelDomainError>>,
    >,
{
    while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) {
        let result = call(remaining.min(BUILD_CANCEL_ATTEMPT_TIMEOUT)).await;
        if !cancel_delivery_should_retry(machine_id, &result) {
            break;
        }
        let Some(delay) = cancel_retry_delay(tokio::time::Instant::now(), deadline) else {
            break;
        };
        tokio::time::sleep(delay).await;
    }
}

fn cancel_delivery_should_retry(
    machine_id: &MachineId,
    result: &Result<MachineBuildCancelRpcOk, MachineCallError<MachineBuildCancelDomainError>>,
) -> bool {
    let expected_origin = BuildExecutorOrigin::Cluster {
        machine_id: machine_id.clone(),
    };
    match result {
        Ok(MachineBuildCancelRpcOk {
            machine_id: _,
            origin,
            outcome: MachineBuildCancelOutcome::NotRunning,
        }) if origin == &expected_origin => true,
        Ok(MachineBuildCancelRpcOk {
            machine_id: _,
            origin,
            outcome: MachineBuildCancelOutcome::Requested,
        }) if origin == &expected_origin => false,
        Ok(MachineBuildCancelRpcOk { .. })
        | Err(MachineCallError::Domain(MachineBuildCancelDomainError::CancelFailed {
            message: _,
        }))
        | Err(MachineCallError::Domain(
            MachineBuildCancelDomainError::OriginMismatch {
                expected: _,
                actual: _,
            }
            | MachineBuildCancelDomainError::ImageSeedMismatch {
                expected: _,
                actual: _,
            },
        )) => false,
        Err(MachineCallError::Unavailable(reason)) => match reason {
            MachineRuntimeUnavailableReason::RequestTimedOut
            | MachineRuntimeUnavailableReason::NoResponders
            | MachineRuntimeUnavailableReason::RequestFailed { message: _ }
            | MachineRuntimeUnavailableReason::ServiceUnavailable { message: _ }
            | MachineRuntimeUnavailableReason::ServiceTimedOut { message: _ }
            | MachineRuntimeUnavailableReason::ServiceInternal { message: _ } => true,
            MachineRuntimeUnavailableReason::EncodeRequest { message: _ }
            | MachineRuntimeUnavailableReason::InvalidSubject
            | MachineRuntimeUnavailableReason::MaxPayloadExceeded
            | MachineRuntimeUnavailableReason::ServiceBadRequest { message: _ }
            | MachineRuntimeUnavailableReason::ServiceConflict { message: _ }
            | MachineRuntimeUnavailableReason::ServiceResponseTooLarge
            | MachineRuntimeUnavailableReason::MalformedServiceError { message: _ }
            | MachineRuntimeUnavailableReason::DecodeResponse { message: _ }
            | MachineRuntimeUnavailableReason::WrongResponder {
                actual_machine_id: _,
            } => false,
        },
    }
}

fn cancel_retry_delay(
    now: tokio::time::Instant,
    deadline: tokio::time::Instant,
) -> Option<Duration> {
    let remaining = deadline.checked_duration_since(now)?;
    (!remaining.is_zero()).then(|| remaining.min(Duration::from_millis(25)))
}

fn build_start_request(
    accepted: &AcceptedBuildExecution,
    origin: BuildExecutorOrigin,
    image_seed: MachineId,
    platform: ployz_core::image::OciPlatform,
    timeout: Duration,
) -> MachineBuildStartRpcRequest {
    MachineBuildStartRpcRequest {
        operation_id: accepted.submission.operation_id.clone(),
        origin,
        image_seed,
        source: accepted.source.clone(),
        adapter: accepted.submission.adapter.clone(),
        platform,
        timeout_millis: execution_timeout_millis(timeout),
    }
}

fn expected_acceptance(
    operation_id: &OperationId,
    machine_id: &MachineId,
    platform: &ployz_core::image::OciPlatform,
) -> BuildExecutorAcceptance {
    BuildExecutorAcceptance {
        operation_id: operation_id.clone(),
        origin: BuildExecutorOrigin::Cluster {
            machine_id: machine_id.clone(),
        },
        image_seed: machine_id.clone(),
        platform: platform.clone(),
    }
}

fn validate_executor_acceptance(
    expected: &BuildExecutorAcceptance,
    actual: &BuildExecutorAcceptance,
) -> Result<(), BuildPlatformFailure> {
    if actual != expected {
        return Err(BuildPlatformFailure::MachineUnavailable {
            message: failure_message("build executor returned mismatched acceptance provenance"),
        });
    }
    Ok(())
}

fn validate_completed_image_seed(
    machine_id: &MachineId,
    completed: &MachineBuildStartRpcOk,
) -> Result<(), BuildPlatformFailure> {
    if completed.image.seed != *machine_id {
        return Err(BuildPlatformFailure::MachineUnavailable {
            message: failure_message("build executor returned an image from a different seed"),
        });
    }
    Ok(())
}

fn execution_timeout_millis(timeout: Duration) -> u64 {
    timeout.as_millis().try_into().unwrap_or(u64::MAX)
}

enum BuildSummary {
    Completed(MachineBuildStartRpcOk),
    Failed {
        acceptance: BuildExecutorAcceptance,
        failure: BuildPlatformFailure,
        log_summary: BuildLogSummary,
    },
    Cancelled {
        acceptance: BuildExecutorAcceptance,
        cleanup: MachineBuildCleanupOutcome,
        log_summary: BuildLogSummary,
    },
    TimedOut {
        acceptance: BuildExecutorAcceptance,
        message: FailureMessage,
        cleanup: MachineBuildCleanupOutcome,
        log_summary: BuildLogSummary,
    },
}

impl BuildSummary {
    fn acceptance(&self) -> &BuildExecutorAcceptance {
        match self {
            Self::Completed(ok) => &ok.acceptance,
            Self::Failed { acceptance, .. }
            | Self::Cancelled { acceptance, .. }
            | Self::TimedOut { acceptance, .. } => acceptance,
        }
    }

    fn log_summary(&self) -> BuildLogSummary {
        match self {
            Self::Completed(ok) => ok.log_summary,
            Self::Failed { log_summary, .. }
            | Self::Cancelled {
                cleanup: _,
                log_summary,
                acceptance: _,
            }
            | Self::TimedOut { log_summary, .. } => *log_summary,
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
        MachineCallError::Unavailable(reason) => machine_unavailable_failure(reason),
        MachineCallError::Domain(MachineBuildStartDomainError::AlreadyRunning) => {
            BuildPlatformFailure::MachineUnavailable {
                message: failure_message("machine reports this build is already running"),
            }
        }
        MachineCallError::Domain(MachineBuildStartDomainError::RuntimeUnavailable) => {
            BuildPlatformFailure::MachineUnavailable {
                message: failure_message("machine build runtime is unavailable"),
            }
        }
        MachineCallError::Domain(MachineBuildStartDomainError::RuntimeStopped) => {
            BuildPlatformFailure::MachineUnavailable {
                message: failure_message("machine build runtime is shutting down"),
            }
        }
        MachineCallError::Domain(MachineBuildStartDomainError::PlatformMismatch {
            expected,
            actual,
        }) => BuildPlatformFailure::PlatformMismatch { expected, actual },
        MachineCallError::Domain(MachineBuildStartDomainError::InvalidTimeout {
            timeout_millis: _,
        }) => BuildPlatformFailure::MachineUnavailable {
            message: failure_message("machine rejected the build execution timeout"),
        },
        MachineCallError::Domain(
            MachineBuildStartDomainError::OriginMismatch {
                expected: _,
                actual: _,
            }
            | MachineBuildStartDomainError::ImageSeedMismatch {
                expected: _,
                actual: _,
            },
        ) => BuildPlatformFailure::MachineUnavailable {
            message: failure_message("machine rejected build executor provenance"),
        },
        MachineCallError::Domain(MachineBuildStartDomainError::PlatformFailed {
            acceptance: _,
            failure,
            log_summary: _,
        }) => failure,
        MachineCallError::Domain(MachineBuildStartDomainError::Cancelled {
            acceptance: _,
            cleanup: _,
            log_summary: _,
        }) => BuildPlatformFailure::MachineUnavailable {
            message: failure_message("machine cancelled the build unexpectedly"),
        },
        MachineCallError::Domain(MachineBuildStartDomainError::TimedOut {
            acceptance: _,
            message,
            cleanup: _,
            log_summary: _,
        }) => BuildPlatformFailure::MachineUnavailable { message },
    };
    BuildOperationFailure::PlatformFailed {
        platform,
        machine_id,
        failure,
    }
}

fn machine_unavailable_failure(reason: MachineRuntimeUnavailableReason) -> BuildPlatformFailure {
    let message = match reason {
        MachineRuntimeUnavailableReason::EncodeRequest { message } => failure_message(format!(
            "machine runtime request could not be encoded: {message}"
        )),
        MachineRuntimeUnavailableReason::RequestTimedOut => {
            failure_message("machine runtime request timed out")
        }
        MachineRuntimeUnavailableReason::NoResponders => {
            failure_message("machine runtime has no responders")
        }
        MachineRuntimeUnavailableReason::InvalidSubject => {
            failure_message("machine runtime subject was invalid")
        }
        MachineRuntimeUnavailableReason::MaxPayloadExceeded => {
            failure_message("machine runtime request exceeded NATS max payload")
        }
        MachineRuntimeUnavailableReason::RequestFailed { message } => {
            failure_message(format!("machine runtime request failed: {message}"))
        }
        MachineRuntimeUnavailableReason::ServiceBadRequest { message } => {
            failure_message(format!("machine runtime rejected the request: {message}"))
        }
        MachineRuntimeUnavailableReason::ServiceConflict { message } => {
            failure_message(format!("machine runtime reported a conflict: {message}"))
        }
        MachineRuntimeUnavailableReason::ServiceResponseTooLarge => {
            failure_message("machine runtime response_too_large")
        }
        MachineRuntimeUnavailableReason::ServiceUnavailable { message } => {
            failure_message(format!("machine runtime service unavailable: {message}"))
        }
        MachineRuntimeUnavailableReason::ServiceTimedOut { message } => {
            failure_message(format!("machine runtime service timed out: {message}"))
        }
        MachineRuntimeUnavailableReason::ServiceInternal { message } => failure_message(format!(
            "machine runtime service failed internally: {message}"
        )),
        MachineRuntimeUnavailableReason::MalformedServiceError { message } => failure_message(
            format!("machine runtime returned malformed service error headers: {message}"),
        ),
        MachineRuntimeUnavailableReason::DecodeResponse { message } => failure_message(format!(
            "machine runtime response could not be decoded: {message}"
        )),
        MachineRuntimeUnavailableReason::WrongResponder { actual_machine_id } => {
            failure_message(format!(
                "machine runtime replied for a different machine: {}",
                actual_machine_id.as_str()
            ))
        }
    };
    BuildPlatformFailure::MachineUnavailable { message }
}

#[cfg(test)]
mod tests;
