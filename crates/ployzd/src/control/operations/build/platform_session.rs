use std::time::Duration;

use ployz_core::build::BuildExecutorEvidence;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::{BuildEvidence, BuildOperationFailure, FailureMessage};

use crate::control::operation_evidence::OperationRepository;
use crate::roles::machine::protocol::{
    BuildExecutorAssignment, BuildLogSummary, MachineBuildLogFrame,
};

use super::log_stream::{LogBeforeDeadline, next_log_before_deadline};

const BUILD_LOG_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct PlatformLogSession<'a> {
    repository: &'a OperationRepository,
    operation_id: &'a OperationId,
    platform: &'a OciPlatform,
    assignment: BuildExecutorAssignment,
    executor: BuildExecutorEvidence,
    logs: async_nats::Subscriber,
    next_sequence: u64,
    logs_open: bool,
}

impl<'a> PlatformLogSession<'a> {
    pub(super) fn new(
        repository: &'a OperationRepository,
        operation_id: &'a OperationId,
        platform: &'a OciPlatform,
        assignment: BuildExecutorAssignment,
        logs: async_nats::Subscriber,
    ) -> Self {
        Self {
            repository,
            operation_id,
            platform,
            executor: BuildExecutorEvidence::from_assignment(&assignment),
            assignment,
            logs,
            next_sequence: 1,
            logs_open: true,
        }
    }

    pub(super) const fn logs_open(&self) -> bool {
        self.logs_open
    }

    pub(super) const fn logs_mut(&mut self) -> &mut async_nats::Subscriber {
        &mut self.logs
    }

    pub(super) async fn record_message(
        &mut self,
        message: Option<async_nats::Message>,
    ) -> Result<(), BuildOperationFailure> {
        let Some(message) = message else {
            self.logs_open = false;
            return Ok(());
        };
        let Ok(frame) = serde_json::from_slice::<MachineBuildLogFrame>(&message.payload) else {
            return Ok(());
        };
        if !is_next_frame(
            self.operation_id,
            self.platform,
            &self.assignment,
            self.next_sequence,
            &frame,
        ) {
            return Ok(());
        }
        self.repository
            .record_build_evidence(
                self.operation_id,
                BuildEvidence::PlatformLog {
                    platform: self.platform.clone(),
                    executor: self.executor.clone(),
                    chunk: frame.chunk,
                },
            )
            .await
            .map_err(evidence_failure)?;
        self.next_sequence += 1;
        Ok(())
    }

    pub(super) async fn drain(
        &mut self,
        summary: BuildLogSummary,
    ) -> Result<(), BuildOperationFailure> {
        let BuildLogSummary {
            final_log_sequence,
            omitted_log_bytes,
        } = summary;
        let deadline = tokio::time::sleep(BUILD_LOG_DRAIN_TIMEOUT);
        tokio::pin!(deadline);
        while self.next_sequence <= final_log_sequence {
            let message =
                match next_log_before_deadline(deadline.as_mut(), &mut self.logs, self.logs_open)
                    .await
                {
                    LogBeforeDeadline::Deadline | LogBeforeDeadline::LogsClosed => break,
                    LogBeforeDeadline::Log(message) => message,
                };
            self.record_message(message).await?;
        }
        if self.next_sequence <= final_log_sequence {
            self.repository
                .record_build_evidence(
                    self.operation_id,
                    BuildEvidence::PlatformLogGap {
                        platform: self.platform.clone(),
                        executor: self.executor.clone(),
                        expected_sequence: self.next_sequence,
                        final_sequence: final_log_sequence,
                    },
                )
                .await
                .map_err(evidence_failure)?;
        }
        if omitted_log_bytes > 0 {
            self.repository
                .record_build_evidence(
                    self.operation_id,
                    BuildEvidence::PlatformLogTruncated {
                        platform: self.platform.clone(),
                        executor: self.executor.clone(),
                        omitted_bytes: omitted_log_bytes,
                    },
                )
                .await
                .map_err(evidence_failure)?;
        }
        Ok(())
    }
}

fn is_next_frame(
    operation_id: &OperationId,
    platform: &OciPlatform,
    assignment: &BuildExecutorAssignment,
    next_sequence: u64,
    frame: &MachineBuildLogFrame,
) -> bool {
    frame.operation_id == *operation_id
        && frame.platform == *platform
        && frame.assignment == *assignment
        && frame.sequence == next_sequence
}

fn evidence_failure(error: impl std::fmt::Display) -> BuildOperationFailure {
    BuildOperationFailure::EvidenceRecordingFailed {
        message: FailureMessage::try_new(error.to_string()).expect("error is non-empty"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::operation::BuildLogChunk;

    #[test]
    fn log_frames_require_exact_provenance_and_next_sequence() {
        let operation_id = OperationId::try_new("build-1").expect("operation id");
        let machine_id = MachineId::try_new("machine-a").expect("machine id");
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        let assignment = BuildExecutorAssignment::Cluster {
            machine_id: machine_id.clone(),
        };
        let frame = MachineBuildLogFrame {
            operation_id: operation_id.clone(),
            assignment: assignment.clone(),
            platform: platform.clone(),
            sequence: 1,
            chunk: BuildLogChunk::try_new("line").expect("log chunk"),
        };

        assert!(is_next_frame(
            &operation_id,
            &platform,
            &assignment,
            1,
            &frame
        ));
        assert!(!is_next_frame(
            &operation_id,
            &platform,
            &assignment,
            2,
            &frame
        ));
        assert!(!is_next_frame(
            &OperationId::try_new("build-2").expect("operation id"),
            &platform,
            &assignment,
            1,
            &frame,
        ));
        assert!(!is_next_frame(
            &operation_id,
            &platform,
            &BuildExecutorAssignment::External {
                pool_id: ployz_core::build::BuildPoolId::try_new("pool-a").expect("pool id"),
                executor_id: ployz_core::build::BuildExecutorId::try_new("executor-a")
                    .expect("executor id"),
                image_seed: machine_id.clone(),
            },
            1,
            &frame,
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn ready_spam_does_not_extend_the_absolute_log_deadline() {
        let deadline = tokio::time::sleep(BUILD_LOG_DRAIN_TIMEOUT);
        tokio::pin!(deadline);
        let mut spam = futures_util::stream::repeat(());

        for _ in 0..100 {
            assert!(matches!(
                next_log_before_deadline(deadline.as_mut(), &mut spam, true).await,
                LogBeforeDeadline::Log(Some(()))
            ));
        }
        tokio::time::advance(BUILD_LOG_DRAIN_TIMEOUT).await;
        assert!(matches!(
            next_log_before_deadline(deadline.as_mut(), &mut spam, true).await,
            LogBeforeDeadline::Deadline
        ));
    }
}
