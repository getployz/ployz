use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use ployz_core::build::build_control_request_timeout;
use ployz_core::deploy::PushedImageReceipt;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{
    BuildCleanupEvidence, BuildEvidence, BuildOperationFailure, BuildPlatformFailure,
    BuildTimeoutFailure, BuildTransition, CancellationReason, FailureMessage,
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
    MachineBuildCancelDomainError, MachineBuildCancelRpcOk, MachineBuildCancelRpcRequest,
    MachineBuildCleanupOutcome, MachineBuildLogFrame, MachineBuildStartDomainError,
    MachineBuildStartRpcOk, MachineBuildStartRpcRequest,
};
use crate::tasks::TaskSpawner;

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

#[derive(Clone, Default)]
struct ActiveBuild {
    started: BTreeSet<MachineId>,
    cancellation_reason: Option<CancellationReason>,
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
        let driver = self.clone();
        let operation_id = accepted.submission.operation_id.clone();
        let admission = self.tasks.spawn(move || async move {
            driver.run(accepted).await;
        });
        super::super::finish_rejected_task_admission(&self.controllers, &operation_id, admission)
            .await;
    }

    pub(crate) async fn cancel(
        &self,
        operation_id: &OperationId,
        reason: CancellationReason,
    ) -> Result<(), String> {
        let machines = {
            let mut active = self.active.lock().await;
            let Some(build) = active.get_mut(operation_id) else {
                self.controllers
                    .repository()
                    .record_build_transition(
                        operation_id,
                        BuildTransition::Cancelled {
                            reason,
                            cleanup: BuildCleanupEvidence::NotRequired,
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(());
            };
            request_cancellation(build, reason)
        };
        let _results =
            futures_util::future::join_all(machines.iter().map(|machine_id| async move {
                let request = MachineBuildCancelRpcRequest {
                    operation_id: operation_id.clone(),
                };
                call_machine::<MachineBuildCancelRpcOk, MachineBuildCancelDomainError>(
                    &self.client,
                    BUILD_CANCEL_TIMEOUT,
                    machine_id,
                    MachineServiceEndpoint::BuildCancel,
                    &request,
                )
                .await
            }))
            .await;
        Ok(())
    }

    async fn run(self, accepted: AcceptedBuildExecution) {
        let id = accepted.submission.operation_id.clone();
        let result = self.run_inner(&accepted).await;
        self.active.lock().await.remove(&id);
        if let Err(failure) = result {
            let _ = self
                .controllers
                .repository()
                .record_build_transition(&id, BuildTransition::Failed { failure })
                .await;
        }
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
        let intent = self
            .intent
            .intent()
            .await
            .map_err(|error| receipt_failure(error.to_string()))?;
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
        self.active.lock().await.insert(
            id.clone(),
            ActiveBuild {
                started: BTreeSet::new(),
                cancellation_reason: None,
            },
        );
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
        let cancellation_reason = self
            .active
            .lock()
            .await
            .get(id)
            .and_then(|build| build.cancellation_reason.clone());
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
                Ok(PlatformOutcome::Cancelled(machine_id)) => cancelled.push(machine_id),
                Ok(PlatformOutcome::CancelledBeforeStart) => {}
                Ok(PlatformOutcome::TimedOut {
                    machine_id,
                    message,
                    cleanup,
                }) => timed_out.push((machine_id, message, cleanup)),
            }
        }
        if !cancelled.is_empty() || cancellation_reason.is_some() {
            let reason = cancellation_reason.unwrap_or_else(|| {
                CancellationReason::try_new("machine cancelled build").expect("reason")
            });
            let cleanup = cleanup_for(id, &self.active, cancelled).await;
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
        let result = call_machine::<MachineBuildStartRpcOk, MachineBuildStartDomainError>(
            &self.client,
            build_control_request_timeout(self.timeout),
            &machine_id,
            MachineServiceEndpoint::BuildStart,
            &request,
        )
        .await;
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
                final_log_sequence,
                omitted_log_bytes,
            })) => BuildSummary::Cancelled {
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
        let mut next = 1;
        while next <= final_log_sequence {
            let Some(message) = tokio::time::timeout(BUILD_LOG_DRAIN_TIMEOUT, logs.next())
                .await
                .ok()
                .flatten()
            else {
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
            let failure = BuildPlatformFailure::AdapterFailed {
                message: FailureMessage::try_new(format!(
                    "build log sequence gap: expected {next}, machine finished at {final_log_sequence}"
                ))
                .expect("message"),
            };
            self.controllers
                .repository()
                .record_build_evidence(
                    id,
                    BuildEvidence::PlatformFailed {
                        platform: platform.clone(),
                        machine_id: machine_id.clone(),
                        failure: failure.clone(),
                    },
                )
                .await
                .map_err(record_failure)?;
            return Ok(PlatformOutcome::Failed(platform_failure(
                platform, machine_id, failure,
            )));
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
                BuildSummary::Cancelled { .. } => PlatformOutcome::Cancelled(machine_id),
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

fn request_cancellation(
    build: &mut ActiveBuild,
    reason: CancellationReason,
) -> BTreeSet<MachineId> {
    build.cancellation_reason = Some(reason);
    build.started.clone()
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
    if build.cancellation_reason.is_some() {
        return false;
    }
    build.started.insert(machine_id.clone());
    true
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
    Cancelled(MachineId),
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
        .map(|build| build.started.clone())
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
    receipt_failure(error.to_string())
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
mod tests {
    use super::*;
    use ployz_core::deploy::PlatformImage;
    use ployz_core::image::{OciDigest, OciPlatform};
    use ployz_core::operation::BuildLogChunk;

    #[test]
    fn caller_budget_is_explicit_and_deterministic() {
        assert_eq!(
            execution_timeout_millis(Duration::from_millis(7_321)),
            7_321
        );
        assert_eq!(
            build_control_request_timeout(ployz_core::build::BUILD_MAX_EXECUTION_TIMEOUT),
            ployz_core::build::BUILD_MAX_MACHINE_RESPONSE_LIFETIME
                + ployz_core::build::BUILD_CONTROL_RESPONSE_MARGIN
        );
    }

    #[test]
    fn log_frames_require_exact_provenance_and_next_sequence() {
        let operation_id = OperationId::try_new("build-1").expect("operation id");
        let machine_id = MachineId::try_new("machine-a").expect("machine id");
        let platform = ployz_core::image::OciPlatform::try_new("linux", "amd64").expect("platform");
        let frame = MachineBuildLogFrame {
            operation_id: operation_id.clone(),
            machine_id: machine_id.clone(),
            platform: platform.clone(),
            sequence: 1,
            chunk: BuildLogChunk::try_new("line").expect("log chunk"),
        };

        assert!(valid_next_log_frame(
            &operation_id,
            &machine_id,
            &platform,
            1,
            &frame
        ));
        assert!(!valid_next_log_frame(
            &operation_id,
            &machine_id,
            &platform,
            2,
            &frame
        ));
        assert!(!valid_next_log_frame(
            &OperationId::try_new("build-2").expect("operation id"),
            &machine_id,
            &platform,
            1,
            &frame,
        ));
    }

    #[tokio::test]
    async fn cancellation_fence_prevents_a_late_machine_start() {
        let operation_id = OperationId::try_new("build-1").expect("operation id");
        let machine_id = MachineId::try_new("machine-a").expect("machine id");
        let active = Mutex::new(BTreeMap::from([(
            operation_id.clone(),
            ActiveBuild {
                started: BTreeSet::new(),
                cancellation_reason: Some(CancellationReason::try_new("stop").expect("reason")),
            },
        )]));

        assert!(!claim_machine_start(&active, &operation_id, &machine_id).await);
        assert!(
            active
                .lock()
                .await
                .get(&operation_id)
                .expect("active build")
                .started
                .is_empty()
        );
    }

    #[test]
    fn cancellation_fanout_targets_every_started_machine() {
        let first = MachineId::try_new("machine-a").expect("machine id");
        let second = MachineId::try_new("machine-b").expect("machine id");
        let mut build = ActiveBuild {
            started: BTreeSet::from([second.clone(), first.clone()]),
            cancellation_reason: None,
        };

        let targets = request_cancellation(
            &mut build,
            CancellationReason::try_new("stop").expect("reason"),
        );

        assert_eq!(targets, BTreeSet::from([first, second]));
        assert!(build.cancellation_reason.is_some());
    }

    #[test]
    fn platform_failure_prevents_partial_receipt_assembly() {
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        let machine_id = MachineId::try_new("machine-a").expect("machine id");
        let failure = platform_failure(
            platform.clone(),
            machine_id.clone(),
            BuildPlatformFailure::AdapterFailed {
                message: FailureMessage::try_new("adapter failed").expect("message"),
            },
        );
        let image = PlatformImage {
            seed: machine_id,
            manifest_digest: OciDigest::try_new(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .expect("digest"),
            image_id: OciDigest::try_new(
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .expect("image id"),
        };

        assert_eq!(
            assemble_receipt(vec![(platform, image)], Some(failure.clone())),
            Err(Box::new(failure))
        );
    }

    #[test]
    fn timeout_cleanup_is_completed_only_when_every_machine_confirms() {
        let confirmed = MachineId::try_new("confirmed").expect("machine id");
        let unconfirmed = MachineId::try_new("unconfirmed").expect("machine id");
        let message = FailureMessage::try_new("deadline exceeded").expect("message");

        assert_eq!(
            timeout_cleanup(&[(
                confirmed.clone(),
                message.clone(),
                MachineBuildCleanupOutcome::Confirmed,
            )]),
            BuildCleanupEvidence::Completed {
                machine_ids: vec![confirmed.clone()],
            }
        );
        assert_eq!(
            timeout_cleanup(&[
                (
                    confirmed,
                    message.clone(),
                    MachineBuildCleanupOutcome::Confirmed,
                ),
                (
                    unconfirmed.clone(),
                    message,
                    MachineBuildCleanupOutcome::Unconfirmed,
                ),
            ]),
            BuildCleanupEvidence::Unconfirmed {
                machine_ids: vec![unconfirmed],
            }
        );
    }
}
