mod support;

use ployz_core::roles::DaemonProcessRole;
use ployz_keeper::artifacts::{ArtifactKind, ArtifactTarget};
use ployz_keeper::executor::KeeperPlanFailure;
use ployz_keeper::join_executor::execute_keeper_join;
use ployz_keeper::steps::{
    ContainerRuntime, JoinMaterialError, JoinToken, KeeperJoinTarget, KeeperStep,
    KeeperStepFailure, KeeperStepFailureReason, KeeperStepLabel, NonEmptyRoleSet,
    PloyzdRoleEnvironmentStep, RedactedJoinMaterial, RoleSetError, keeper_join_local_install_plan,
};
use ployz_keeper::systemd::SupervisorUnitTarget;
use ployz_sdk_types::MachineJoinReportFailure;
use ployz_test_support::ids::{failure_message, machine_id};
use ployz_test_support::keeper::ployzd_artifact;
use support::bootstrap::*;

#[test]
fn keeper_join_installs_ployzd_and_only_assigned_role_units() {
    let roles = vec![
        DaemonProcessRole::Machine(machine_id("machine_7")),
        DaemonProcessRole::Gateway,
    ];
    let material = keeper_join_material();
    let plan = keeper_join_local_install_plan(KeeperJoinTarget::new(
        material.clone(),
        ployzd_artifact(),
        dataplane_artifacts(),
        NonEmptyRoleSet::try_new(roles.clone()).expect("non-empty unique roles"),
        edge_role_environment(),
    ));

    assert!(installs_artifact_kind(&plan, ArtifactKind::Ployzd));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfBytecode));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfCtl));
    assert!(writes_ployzd_role_units(&plan));
    assert!(
        plan.steps()
            .contains(&KeeperStep::StoreJoinMaterial(material))
    );
    let [
        store_material,
        prepare_runtime,
        verify_runtime,
        install_ployzd,
        ..,
    ] = plan.steps()
    else {
        panic!("join install plan records material and Docker prep before artifacts");
    };
    assert!(matches!(store_material, KeeperStep::StoreJoinMaterial(_)));
    assert_eq!(
        *prepare_runtime,
        KeeperStep::PrepareContainerRuntime(ContainerRuntime::Docker)
    );
    assert_eq!(
        *verify_runtime,
        KeeperStep::VerifyContainerRuntime(ContainerRuntime::Docker)
    );
    assert!(matches!(
        install_ployzd,
        KeeperStep::InstallArtifact(ArtifactTarget {
            kind: ArtifactKind::Ployzd,
            ..
        })
    ));
    let rendered_env = edge_role_environment()
        .render_for_role(&DaemonProcessRole::Machine(machine_id("machine_7")));
    assert!(rendered_env.contains("PLOYZ_EBPF_BYTECODE=/usr/local/lib/ployz/ebpf/ployz-ebpf-tc\n"));
    assert!(rendered_env.contains("PLOYZ_EBPF_CTL=/usr/local/bin/ployz-ebpf-ctl\n"));
    assert!(
        rendered_env
            .contains("PLOYZ_NATS_NKEY_SEED_FILE=/var/lib/ployz/join-material.d/nats.creds\n")
    );
    assert!(rendered_env.contains("PLOYZ_NATS_CA_FILE=/var/lib/ployz/join-material.d/ca.pem\n"));

    for role in roles {
        assert!(
            plan.steps()
                .contains(&KeeperStep::WritePloyzdRoleEnvironment(
                    PloyzdRoleEnvironmentStep {
                        role: role.clone(),
                        target: edge_role_environment(),
                    }
                ))
        );
        let unit = SupervisorUnitTarget::PloyzdRole(role);
        assert!(plan_writes_unit(&plan, &unit));
        assert!(
            plan.steps()
                .contains(&KeeperStep::StartSupervisorUnit(unit))
        );
    }

    assert!(!plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Control)
    ));
    assert!(!plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Dns)
    ));
}

#[test]
fn role_sets_reject_empty_and_duplicate_assignments() {
    assert_eq!(NonEmptyRoleSet::try_new(vec![]), Err(RoleSetError::Empty));
    assert_eq!(
        NonEmptyRoleSet::try_new(vec![DaemonProcessRole::Gateway, DaemonProcessRole::Gateway]),
        Err(RoleSetError::Duplicate {
            role: DaemonProcessRole::Gateway,
        })
    );
}

#[test]
fn join_material_cluster_name_rejects_persisted_format_breakers() {
    for value in ["prod\nnext", "prod\rnext"] {
        assert_eq!(
            RedactedJoinMaterial::new(machine_id("machine_7"), value, NATS_CA_DIGEST),
            Err(JoinMaterialError::InvalidJoinMaterialValue {
                label: "cluster name",
                value: value.to_owned(),
            })
        );
    }
}

#[test]
fn join_material_rejects_persisted_line_breakers() {
    for value in ["cccc\nnext", "cccc\rnext"] {
        assert_eq!(
            RedactedJoinMaterial::new(machine_id("machine_7"), "prod", value),
            Err(JoinMaterialError::InvalidJoinMaterialValue {
                label: "trusted NATS CA digest",
                value: value.to_owned(),
            })
        );
    }
}

#[test]
fn keeper_join_executor_redacts_join_token_from_progress() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer {
        fail_message: Some("simulated join token redeem failure"),
        ..RecordingJoinRedeemer::default()
    };
    let mut effects = RecordingEffects {
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );
    let rendered = format!("{execution:?}");

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::RedeemJoinToken,
            reason: KeeperStepFailureReason::JoinTokenRedeemFailed,
            message,
        }))
            if message.as_str() == "simulated join token redeem failure"
    ));
    assert!(!rendered.contains("join_secret"));
    assert_eq!(
        redeemer.redeemed_tokens,
        vec![JoinToken::try_new("join_secret").expect("valid join token")]
    );
    assert_eq!(token_consumer.consumed, 0);
    assert!(reporter.reports.is_empty());
}

#[test]
fn keeper_join_keeps_token_when_material_store_fails() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let material = keeper_join_material();
    let redacted = material.redacted();
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::StoreJoinMaterial(redacted.clone())),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::StoreJoinMaterial(failed_material),
            reason: KeeperStepFailureReason::JoinMaterialStoreFailed,
            ..
        })) if *failed_material == redacted
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated keeper step failure"),
            },
        }]
    );
}

#[test]
fn keeper_join_keeps_token_when_install_fails_after_redemption() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::InstallArtifact(ployzd_artifact())),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::InstallArtifact(ArtifactTarget {
                kind: ArtifactKind::Ployzd,
                ..
            }),
            reason: KeeperStepFailureReason::ArtifactInstallFailed,
            ..
        }))
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated keeper step failure"),
            },
        }]
    );
}

#[test]
fn keeper_join_reports_docker_prepare_failure_after_redemption() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::PrepareContainerRuntime(
            ContainerRuntime::Docker,
        )),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
            reason: KeeperStepFailureReason::ContainerRuntimePrepareFailed,
            ..
        }))
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated keeper step failure"),
            },
        }]
    );
}

#[test]
fn keeper_join_does_not_report_completed_when_token_consume_fails() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut reporter = RecordingJoinReporter::default();
    let mut effects = RecordingEffects {
        ..RecordingEffects::default()
    };
    let mut token_consumer = RecordingTokenConsumer {
        fail_message: Some("simulated token cleanup failure"),
        ..RecordingTokenConsumer::default()
    };
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::ConsumeJoinTokenFile,
            reason: KeeperStepFailureReason::JoinTokenConsumeFailed,
            ..
        }))
    ));
    assert_eq!(token_consumer.consumed, 1);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated token cleanup failure"),
            },
        }]
    );
}
