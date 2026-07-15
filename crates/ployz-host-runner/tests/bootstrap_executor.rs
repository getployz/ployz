mod support;

use ployz_core::install::{WrappedCaKey, WrappedCoreSeeds};

use ployz_core::roles::InstallRolePolicy;
use ployz_host_runner::execution::SupervisorUnitTarget;
use ployz_host_runner::plan::{
    ContainerRuntime, FirstMachineInstallTarget, HostRunnerPlanFailure, HostRunnerPlanTerminal,
    HostRunnerRecordFailure, HostRunnerStep, HostRunnerStepEvent, HostRunnerStepFailure,
    HostRunnerStepFailureReason, HostRunnerStepLabel, execute_host_runner_plan,
    first_machine_install_plan, render_step_event,
};
use ployz_test_support::ids::{failure_message, machine_id};
use support::artifacts::{nats_server_artifact, ployzd_artifact};
use support::bootstrap::*;

#[test]
fn host_runner_step_failure_is_bootstrap_scoped_and_typed() {
    let step = HostRunnerStep::StartSupervisorUnit(SupervisorUnitTarget::NatsServer);
    assert_eq!(
        HostRunnerStepFailure::from_step(
            &step,
            failure_message("simulated supervisor start failure")
        ),
        HostRunnerStepFailure {
            step: HostRunnerStepLabel::StartSupervisorUnit(SupervisorUnitTarget::NatsServer),
            reason: HostRunnerStepFailureReason::SupervisorStartFailed,
            message: failure_message("simulated supervisor start failure"),
        },
    );
}

#[test]
fn host_runner_plan_executor_runs_steps_in_order_and_records_progress() {
    let plan = first_machine_install_plan(FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all().without_gateway(),
        ployz_core::install::HostPortAssurance::Keeper,
        test_identity().clone(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    ));
    let mut effects = RecordingEffects::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);
    let rendered = format!("{execution:?}");

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert!(rendered.contains("/usr/local/bin/ployzd"));
    assert!(rendered.contains(PLOYZD_DIGEST));
    assert_eq!(effects.calls.len(), plan.steps().len());
    assert_eq!(recorder.events, execution.events);
    assert_eq!(
        effects.calls,
        plan.steps()
            .iter()
            .map(HostRunnerStepLabel::from_step)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        execution.events,
        plan.steps()
            .iter()
            .flat_map(|step| {
                let label = HostRunnerStepLabel::from_step(step);
                [
                    HostRunnerStepEvent::Started {
                        step: label.clone(),
                    },
                    HostRunnerStepEvent::Succeeded { step: label },
                ]
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn host_runner_plan_executor_stops_on_first_failed_step() {
    let plan = first_machine_plan();
    let ployzd_target = ployzd_artifact();
    let mut effects = RecordingEffects {
        fail_on: Some(HostRunnerStepLabel::InstallArtifact(ployzd_target.clone())),
        fail_message: "simulated artifact install failure",
        ..RecordingEffects::default()
    };
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);
    let failure = HostRunnerStepFailure {
        step: HostRunnerStepLabel::InstallArtifact(ployzd_target.clone()),
        reason: HostRunnerStepFailureReason::ArtifactInstallFailed,
        message: failure_message("simulated artifact install failure"),
    };

    assert_eq!(
        execution.terminal,
        HostRunnerPlanTerminal::Failed(Box::new(HostRunnerPlanFailure::Step(failure.clone())))
    );
    assert_eq!(
        effects.calls,
        plan.steps()
            .iter()
            .take(effects.calls.len())
            .map(HostRunnerStepLabel::from_step)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        execution.events.last(),
        Some(&HostRunnerStepEvent::Failed(failure))
    );
    assert_eq!(recorder.events, execution.events);
}

#[test]
fn host_runner_progress_renders_container_runtime_steps() {
    assert_eq!(
        render_step_event(&HostRunnerStepEvent::Started {
            step: HostRunnerStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
        }),
        "started prepare-container-runtime docker"
    );
    assert_eq!(
        render_step_event(&HostRunnerStepEvent::Failed(HostRunnerStepFailure {
            step: HostRunnerStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker),
            reason: HostRunnerStepFailureReason::ContainerRuntimeVerifyFailed,
            message: failure_message("docker info failed"),
        })),
        "failed verify-container-runtime docker container-runtime-verify-failed: docker info failed"
    );
}

#[test]
fn host_runner_plan_executor_records_started_before_applying_step() {
    let plan = first_machine_plan();
    let failed_event = HostRunnerStepEvent::Started {
        step: HostRunnerStepLabel::InstallArtifact(ployzd_artifact()),
    };
    let mut effects = RecordingEffects::default();
    let mut recorder = RecordingRecorder {
        fail_on: Some(failed_event.clone()),
        fail_message: "simulated event recorder failure",
        ..RecordingRecorder::default()
    };

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(
        execution.terminal,
        HostRunnerPlanTerminal::Failed(Box::new(HostRunnerPlanFailure::Record(
            HostRunnerRecordFailure {
                event: failed_event,
                message: failure_message("simulated event recorder failure"),
            }
        )))
    );
    assert_eq!(
        effects.calls,
        plan.steps()
            .iter()
            .take(effects.calls.len())
            .map(HostRunnerStepLabel::from_step)
            .collect::<Vec<_>>()
    );
    assert_eq!(execution.events, recorder.events);
}
