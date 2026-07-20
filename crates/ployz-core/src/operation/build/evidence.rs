use crate::build::{
    BuildAdapter, BuildExecutorEvidence, BuildPlatformExecutorAssignment, VerifiedBuildSource,
};
use crate::deploy::PlatformImage;
use crate::image::OciPlatform;

use super::{
    BuildAdapterToolchainEvidence, BuildFields, BuildLogChunk, BuildOperationState,
    BuildPlatformFailure, BuildToolchainEvidence, invalid_transition, status,
};
use crate::operation::{EventSequence, OperationProjection, StatusProjectionError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildEvidence {
    VerifiedSource {
        platform: OciPlatform,
        executor: BuildExecutorEvidence,
        source: VerifiedBuildSource,
    },
    PlatformPlaced {
        platform: OciPlatform,
        executor: BuildExecutorEvidence,
    },
    ToolchainVerified {
        platform: OciPlatform,
        executor: BuildExecutorEvidence,
        toolchain: BuildToolchainEvidence,
    },
    PlatformLog {
        platform: OciPlatform,
        executor: BuildExecutorEvidence,
        chunk: BuildLogChunk,
    },
    PlatformLogTruncated {
        platform: OciPlatform,
        executor: BuildExecutorEvidence,
        omitted_bytes: u64,
    },
    PlatformLogGap {
        platform: OciPlatform,
        executor: BuildExecutorEvidence,
        expected_sequence: u64,
        final_sequence: u64,
    },
    PlatformCompleted {
        platform: OciPlatform,
        executor: BuildExecutorEvidence,
        image: PlatformImage,
    },
    PlatformFailed {
        platform: OciPlatform,
        executor: BuildExecutorEvidence,
        failure: BuildPlatformFailure,
    },
}

impl BuildEvidence {
    #[must_use]
    pub const fn platform(&self) -> &OciPlatform {
        match self {
            Self::VerifiedSource { platform, .. }
            | Self::PlatformPlaced { platform, .. }
            | Self::ToolchainVerified { platform, .. }
            | Self::PlatformLog { platform, .. }
            | Self::PlatformLogTruncated { platform, .. }
            | Self::PlatformLogGap { platform, .. }
            | Self::PlatformCompleted { platform, .. }
            | Self::PlatformFailed { platform, .. } => platform,
        }
    }

    #[must_use]
    pub const fn executor(&self) -> &BuildExecutorEvidence {
        match self {
            Self::VerifiedSource { executor, .. }
            | Self::PlatformPlaced { executor, .. }
            | Self::ToolchainVerified { executor, .. }
            | Self::PlatformLog { executor, .. }
            | Self::PlatformLogTruncated { executor, .. }
            | Self::PlatformLogGap { executor, .. }
            | Self::PlatformCompleted { executor, .. }
            | Self::PlatformFailed { executor, .. } => executor,
        }
    }
}

pub(super) fn project(
    fields: &BuildFields<'_>,
    evidence: BuildEvidence,
    sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let platform = evidence.platform();
    let placement = matches!(evidence, BuildEvidence::PlatformPlaced { .. });
    let allowed = if placement {
        matches!(fields.state, BuildOperationState::Placing)
    } else {
        matches!(fields.state, BuildOperationState::Building)
    };
    if !allowed || !fields.platforms.contains(platform) {
        return Err(invalid_transition(fields));
    }

    let assignment = evidence.executor().assignment();
    let mut executor_assignments = fields.executor_assignments.clone();
    if placement {
        if executor_assignments
            .insert(
                fields.target,
                fields.platforms,
                BuildPlatformExecutorAssignment {
                    platform: platform.clone(),
                    executor: assignment.clone(),
                },
            )
            .is_err()
        {
            return Err(invalid_transition(fields));
        }
    } else if !executor_assignments.matches(platform, assignment) {
        return Err(invalid_transition(fields));
    }

    if let BuildEvidence::PlatformCompleted {
        executor, image, ..
    } = &evidence
        && image.seed != *executor.machine_id()
    {
        return Err(invalid_transition(fields));
    }
    if let BuildEvidence::VerifiedSource { source, .. } = &evidence
        && source.evidence() != *fields.source
    {
        return Err(invalid_transition(fields));
    }
    if let BuildEvidence::ToolchainVerified { toolchain, .. } = &evidence
        && !toolchain_matches_adapter(fields.adapter, toolchain)
    {
        return Err(invalid_transition(fields));
    }

    Ok(OperationProjection::StatusChanged {
        status: Box::new(status(
            fields,
            fields.state.clone(),
            executor_assignments,
            sequence,
        )),
    })
}

fn toolchain_matches_adapter(adapter: &BuildAdapter, toolchain: &BuildToolchainEvidence) -> bool {
    matches!(
        (adapter, &toolchain.adapter),
        (
            BuildAdapter::Dockerfile { .. },
            BuildAdapterToolchainEvidence::Dockerfile,
        ) | (
            BuildAdapter::Railpack { .. },
            BuildAdapterToolchainEvidence::Railpack { .. },
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::{
        BuildCacheScope, BuildExecutorAssignment, BuildExecutorId, BuildPlatforms, BuildPoolId,
        BuildTarget, GitSource,
    };
    use crate::deploy::{ImageAvailabilityExpiresAt, PlatformImage};
    use crate::ids::{MachineId, OperationId};
    use crate::image::OciDigest;
    use crate::operation::{BuildTransition, OperationEvent, OperationStatus};

    fn platform() -> OciPlatform {
        OciPlatform::try_new("linux", "amd64").expect("platform")
    }

    fn accepted(target: BuildTarget) -> OperationStatus {
        OperationStatus::build_accepted(
            OperationId::try_new("build-evidence").expect("operation"),
            target,
            GitSource::try_new(
                "https://example.com/repo.git",
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "secret",
                None::<String>,
            )
            .expect("source")
            .evidence()
            .into(),
            BuildAdapter::Railpack {
                cache_scope: BuildCacheScope::try_new("cache").expect("scope"),
            },
            BuildPlatforms::try_new([platform()]).expect("platforms"),
            EventSequence::try_new(1).expect("sequence"),
        )
    }

    fn place_and_start(target: BuildTarget, executor: BuildExecutorEvidence) -> OperationStatus {
        let operation_id = OperationId::try_new("build-evidence").expect("operation");
        let accepted = accepted(target);
        let OperationProjection::StatusChanged { status: placing } =
            crate::operation::project_operation_event(
                &accepted,
                BuildTransition::Placing.event(&operation_id),
                EventSequence::try_new(2).expect("sequence"),
            )
            .expect("placing")
        else {
            panic!("placing status")
        };
        let placed = OperationEvent::BuildPlatformPlaced {
            operation_id: operation_id.clone(),
            platform: platform(),
            executor,
        };
        let OperationProjection::StatusChanged { status: placed } =
            crate::operation::project_operation_event(
                &placing,
                placed,
                EventSequence::try_new(3).expect("sequence"),
            )
            .expect("placement")
        else {
            panic!("placed status")
        };
        let OperationProjection::StatusChanged { status: building } =
            crate::operation::project_operation_event(
                &placed,
                BuildTransition::Building.event(&operation_id),
                EventSequence::try_new(4).expect("sequence"),
            )
            .expect("building")
        else {
            panic!("building status")
        };
        *building
    }

    #[test]
    fn later_cluster_evidence_must_match_recorded_placement() {
        let machine_a = MachineId::try_new("machine-a").expect("machine");
        let machine_b = MachineId::try_new("machine-b").expect("machine");
        let building = place_and_start(
            BuildTarget::Cluster,
            BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::Cluster {
                machine_id: machine_a,
            }),
        );
        let mismatched = OperationEvent::BuildPlatformLog {
            operation_id: OperationId::try_new("build-evidence").expect("operation"),
            platform: platform(),
            executor: BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::Cluster {
                machine_id: machine_b,
            }),
            chunk: BuildLogChunk::try_new("log").expect("chunk"),
        };
        assert!(
            crate::operation::project_operation_event(
                &building,
                mismatched,
                EventSequence::try_new(5).expect("sequence"),
            )
            .is_err()
        );
    }

    #[test]
    fn later_external_evidence_and_image_seed_must_match_recorded_placement() {
        let pool_id = BuildPoolId::try_new("pool-a").expect("pool");
        let executor_a = BuildExecutorAssignment::External {
            pool_id: pool_id.clone(),
            executor_id: BuildExecutorId::try_new("executor-a").expect("executor"),
            image_seed: MachineId::try_new("seed-a").expect("seed"),
        };
        let building = place_and_start(
            BuildTarget::External {
                pool_id: pool_id.clone(),
            },
            BuildExecutorEvidence::from_assignment(&executor_a),
        );
        let executor_b =
            BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::External {
                pool_id,
                executor_id: BuildExecutorId::try_new("executor-b").expect("executor"),
                image_seed: MachineId::try_new("seed-b").expect("seed"),
            });
        let failed = OperationEvent::BuildPlatformFailed {
            operation_id: OperationId::try_new("build-evidence").expect("operation"),
            platform: platform(),
            executor: executor_b.clone(),
            failure: BuildPlatformFailure::AdapterFailed {
                message: crate::operation::FailureMessage::try_new("failed").expect("message"),
            },
        };
        assert!(
            crate::operation::project_operation_event(
                &building,
                failed,
                EventSequence::try_new(5).expect("sequence"),
            )
            .is_err()
        );

        let completed = OperationEvent::BuildPlatformCompleted {
            operation_id: OperationId::try_new("build-evidence").expect("operation"),
            platform: platform(),
            executor: BuildExecutorEvidence::from_assignment(&executor_a),
            image: PlatformImage {
                seed: MachineId::try_new("seed-b").expect("seed"),
                manifest_digest: OciDigest::try_new(format!("sha256:{}", "1".repeat(64)))
                    .expect("digest"),
                image_id: OciDigest::try_new(format!("sha256:{}", "2".repeat(64))).expect("digest"),
                availability_expires_at: ImageAvailabilityExpiresAt::try_new(4_102_444_800)
                    .expect("expiry"),
            },
        };
        assert!(
            crate::operation::project_operation_event(
                &building,
                completed,
                EventSequence::try_new(6).expect("sequence"),
            )
            .is_err()
        );
    }
}
