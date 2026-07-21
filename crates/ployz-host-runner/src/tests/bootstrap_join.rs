use super::support;

use crate::execution::SupervisorUnitTarget;
use crate::execution::{ArtifactKind, ArtifactTarget};
use crate::lifecycle::machine_join::execution::execute_host_runner_join;
use crate::plan::HostRunnerPlanFailure;
use crate::plan::{
    ContainerRuntime, HostRunnerJoinTarget, HostRunnerStep, HostRunnerStepFailure,
    HostRunnerStepFailureReason, HostRunnerStepLabel, JoinMaterialError, JoinToken,
    NonEmptyRoleSet, PloyzdRoleEnvironmentStep, RedactedJoinMaterial, RoleSetError,
    host_runner_join_local_install_plan,
};
use ployz_core::roles::DaemonProcessRole;
use ployz_sdk_types::MachineJoinReportFailure;
use ployz_test_support::ids::{failure_message, machine_id};
use support::artifacts::{ployzd_artifact, railpack_artifact};
use support::bootstrap::*;

#[test]
fn host_runner_join_installs_ployzd_and_only_assigned_role_units() {
    let roles = vec![
        DaemonProcessRole::Machine(machine_id("machine_7")),
        DaemonProcessRole::Gateway,
    ];
    let material = host_runner_join_material();
    let plan = host_runner_join_local_install_plan(HostRunnerJoinTarget::new(
        material.clone(),
        ployzd_artifact(),
        dataplane_artifacts(),
        railpack_artifact(),
        NonEmptyRoleSet::try_new(roles.clone()).expect("non-empty unique roles"),
        edge_role_environment(),
        ployz_core::install::HostPortAssurance::Keeper,
    ));

    assert!(installs_artifact_kind(&plan, ArtifactKind::Ployzd));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfBytecode));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfCtl));
    assert!(installs_artifact_kind(&plan, ArtifactKind::Railpack));
    assert!(writes_ployzd_role_units(&plan));
    assert!(
        plan.steps()
            .contains(&HostRunnerStep::StoreJoinMaterial(material))
    );
    let [
        verify_host,
        preflight_material,
        store_assigned,
        store_material,
        preflight_install,
        assure_ports,
        prepare_dataplane_host,
        prepare_runtime,
        verify_runtime,
        install_ployzd,
        ..,
    ] = plan.steps()
    else {
        panic!("join install plan records material and Docker prep before artifacts");
    };
    assert!(matches!(verify_host, HostRunnerStep::VerifyHost(_)));
    assert!(matches!(
        preflight_material,
        HostRunnerStep::PreflightHostPorts(_)
    ));
    assert!(matches!(
        store_assigned,
        HostRunnerStep::StoreAssignedSubstrate(_)
    ));
    assert!(matches!(
        store_material,
        HostRunnerStep::StoreJoinMaterial(_)
    ));
    assert!(matches!(
        preflight_install,
        HostRunnerStep::PreflightHostPorts(_)
    ));
    assert!(matches!(assure_ports, HostRunnerStep::AssureHostPorts(_)));
    assert_eq!(
        *prepare_dataplane_host,
        HostRunnerStep::PrepareDataplaneHost
    );
    assert_eq!(
        *prepare_runtime,
        HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            ployz_core::network::MachineEndpointSupernet::default_v1(),
        )
    );
    assert_eq!(
        *verify_runtime,
        HostRunnerStep::VerifyContainerRuntime(ContainerRuntime::Docker)
    );
    assert!(matches!(
        install_ployzd,
        HostRunnerStep::InstallArtifact(ArtifactTarget {
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
                .contains(&HostRunnerStep::WritePloyzdRoleEnvironment(
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
                .contains(&HostRunnerStep::StartSupervisorUnit(unit))
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
fn host_runner_join_executor_redacts_join_token_from_progress() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer {
        fail_message: Some("simulated join token redeem failure"),
        ..RecordingJoinRedeemer::default()
    };
    let mut effects = RecordingEffects {
        ..RecordingEffects::default()
    };
    let mut resolver = RecordingJoinResolver::default();
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_join(
        &token,
        &mut redeemer,
        &mut resolver,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );
    let rendered = format!("{execution:?}");

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::RedeemJoinToken,
            reason: HostRunnerStepFailureReason::JoinTokenRedeemFailed,
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
fn host_runner_join_reports_target_resolution_failure_after_redemption() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut resolver = RecordingJoinResolver {
        failure: Some(
            crate::lifecycle::machine_join::execution::JoinTargetResolutionFailure::ReleasePlatform {
                failure: ployz_core::install::ReleasePlatformFailure::Unsupported {
                    platform: "linux/riscv64".to_owned(),
                },
            },
        ),
    };
    let mut effects = RecordingEffects::default();
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_join(
        &token,
        &mut redeemer,
        &mut resolver,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::ResolveJoinTarget,
            reason: HostRunnerStepFailureReason::JoinTargetResolutionFailed,
            message,
        })) if message.as_str() == "unsupported release platform linux/riscv64"
    ));
    assert!(effects.calls.is_empty());
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::ReleasePlatform {
                failure: ployz_core::install::ReleasePlatformFailure::Unsupported {
                    platform: "linux/riscv64".to_owned(),
                },
            },
        }]
    );
}

#[test]
fn host_runner_join_reports_missing_release_platform_without_install_effects() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut resolver = RecordingJoinResolver {
        failure: Some(
            crate::lifecycle::machine_join::execution::JoinTargetResolutionFailure::ReleasePlatform {
                failure: ployz_core::install::ReleasePlatformFailure::Missing,
            },
        ),
    };
    let mut effects = RecordingEffects::default();
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_join(
        &token,
        &mut redeemer,
        &mut resolver,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::ResolveJoinTarget,
            reason: HostRunnerStepFailureReason::JoinTargetResolutionFailed,
            ..
        }))
    ));
    assert!(effects.calls.is_empty());
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::ReleasePlatform {
                failure: ployz_core::install::ReleasePlatformFailure::Missing,
            },
        }]
    );
}

#[test]
fn host_runner_join_keeps_token_when_material_store_fails() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let material = host_runner_join_material();
    let redacted = material.redacted();
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut resolver = RecordingJoinResolver::default();
    let mut effects = RecordingEffects {
        fail_on: Some(HostRunnerStepLabel::StoreJoinMaterial(redacted.clone())),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_join(
        &token,
        &mut redeemer,
        &mut resolver,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::StoreJoinMaterial(failed_material),
            reason: HostRunnerStepFailureReason::JoinMaterialStoreFailed,
            ..
        })) if *failed_material == redacted
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated Host Runner step failure"),
            },
        }]
    );
}

#[test]
fn host_runner_join_keeps_token_when_install_fails_after_redemption() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut resolver = RecordingJoinResolver::default();
    let mut effects = RecordingEffects {
        fail_on: Some(HostRunnerStepLabel::InstallArtifact(ployzd_artifact())),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_join(
        &token,
        &mut redeemer,
        &mut resolver,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::InstallArtifact(ArtifactTarget {
                kind: ArtifactKind::Ployzd,
                ..
            }),
            reason: HostRunnerStepFailureReason::ArtifactInstallFailed,
            ..
        }))
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated Host Runner step failure"),
            },
        }]
    );
}

#[test]
fn host_runner_join_reports_docker_prepare_failure_after_redemption() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut resolver = RecordingJoinResolver::default();
    let mut effects = RecordingEffects {
        fail_on: Some(HostRunnerStepLabel::PrepareContainerRuntime(
            ContainerRuntime::Docker,
        )),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_join(
        &token,
        &mut redeemer,
        &mut resolver,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
            reason: HostRunnerStepFailureReason::ContainerRuntimePrepareFailed,
            ..
        }))
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated Host Runner step failure"),
            },
        }]
    );
}

#[test]
fn host_runner_join_does_not_report_completed_when_token_consume_fails() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut resolver = RecordingJoinResolver::default();
    let mut reporter = RecordingJoinReporter::default();
    let mut effects = RecordingEffects {
        ..RecordingEffects::default()
    };
    let mut token_consumer = RecordingTokenConsumer {
        fail_message: Some("simulated token cleanup failure"),
        ..RecordingTokenConsumer::default()
    };
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_join(
        &token,
        &mut redeemer,
        &mut resolver,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::ConsumeJoinTokenFile,
            reason: HostRunnerStepFailureReason::JoinTokenConsumeFailed,
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
