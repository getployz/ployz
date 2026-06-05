use std::path::PathBuf;

use ployz_core::ids::{NodeId, OperationId};
use ployz_core::roles::{DaemonProcessRole, TunnelSide};
use ployz_keeper::artifacts::{
    ArtifactKind, ArtifactSource, ArtifactTarget, ArtifactVersion, PloyzdArtifactTarget,
    Sha256Digest,
};
use ployz_keeper::health::{KeeperStepFailureReason, RoleHealthGate};
use ployz_keeper::steps::{
    KeeperRolloutStatus, KeeperStep, KeeperStepFailure, NonEmptyRoleSet, SubstrateRolloutTarget,
    pause_rollout_on_failure, substrate_rollout_plan,
};
use ployz_keeper::systemd::SupervisorUnitTarget;

#[test]
fn substrate_rollout_installs_ployzd_then_restarts_assigned_roles_under_health_gates() {
    let operation_id = operation_id("op_rollout");
    let roles = vec![
        DaemonProcessRole::Control,
        DaemonProcessRole::Node(node_id("node_7")),
        DaemonProcessRole::Tunnel(TunnelSide::Core),
    ];
    let plan = substrate_rollout_plan(SubstrateRolloutTarget::new(
        operation_id.clone(),
        ployzd_artifact(),
        NonEmptyRoleSet::try_new(roles.clone()).expect("non-empty unique roles"),
    ));

    assert!(plan.installs_artifact_kind(ArtifactKind::Ployzd));
    assert!(
        plan.steps()
            .contains(&KeeperStep::ReportProgress(operation_id.clone()))
    );
    assert!(
        plan.steps()
            .contains(&KeeperStep::VerifyArtifact(ArtifactTarget::Ployzd(
                ployzd_artifact()
            )))
    );
    assert!(
        plan.steps()
            .contains(&KeeperStep::InstallArtifact(ArtifactTarget::Ployzd(
                ployzd_artifact()
            )))
    );

    for role in roles {
        assert!(plan.steps().contains(&KeeperStep::RestartSupervisorUnit(
            SupervisorUnitTarget::PloyzdRole(role.clone())
        )));
        assert!(
            plan.steps()
                .contains(&KeeperStep::HealthCheck(RoleHealthGate::new(role, 60)))
        );
    }
}

#[test]
fn failed_keeper_step_pauses_rollout_with_typed_failure() {
    let failing_step = KeeperStep::HealthCheck(RoleHealthGate::new(DaemonProcessRole::Gateway, 60));
    let failure = KeeperStepFailure::new(
        failing_step.clone(),
        KeeperStepFailureReason::HealthGateFailed,
    );

    assert_eq!(
        pause_rollout_on_failure(failure.clone()),
        KeeperRolloutStatus::Paused { failure }
    );
    assert_ne!(
        pause_rollout_on_failure(KeeperStepFailure::new(
            failing_step,
            KeeperStepFailureReason::HealthGateFailed,
        )),
        KeeperRolloutStatus::Running
    );
}

fn ployzd_artifact() -> PloyzdArtifactTarget {
    PloyzdArtifactTarget::new(
        version("0.1.1"),
        source("https://example.invalid/ployzd"),
        digest(PLOYZD_DIGEST),
        PathBuf::from("/usr/local/bin/ployzd"),
    )
    .expect("valid ployzd artifact")
}

fn version(value: &str) -> ArtifactVersion {
    ArtifactVersion::try_new(value).expect("valid artifact version")
}

fn source(value: &str) -> ArtifactSource {
    ArtifactSource::try_new(value).expect("valid artifact source")
}

fn digest(value: &str) -> Sha256Digest {
    Sha256Digest::try_new(value).expect("valid artifact digest")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

const PLOYZD_DIGEST: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
