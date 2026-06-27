mod support;

use std::path::PathBuf;

use ployz_core::roles::InstallRolePolicy;
use ployz_keeper::executor::{
    KeeperPlanFailure, KeeperPlanTerminal, KeeperRecordFailure, KeeperStepEvent,
    execute_keeper_plan,
};
use ployz_keeper::report::render_step_event;
use ployz_keeper::steps::{
    ContainerRuntime, FirstMachineInstallTarget, HostPrerequisite, KeeperStep, KeeperStepFailure,
    KeeperStepFailureReason, KeeperStepLabel, first_machine_install_plan,
};
use ployz_keeper::systemd::SupervisorUnitTarget;
use ployz_test_support::ids::{failure_message, machine_id};
use ployz_test_support::keeper::{nats_server_artifact, ployzd_artifact};
use support::bootstrap::*;

#[test]
fn keeper_step_failure_is_bootstrap_scoped_and_typed() {
    let step = KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::NatsServer);
    assert_eq!(
        KeeperStepFailure::from_step(&step, failure_message("simulated supervisor start failure")),
        KeeperStepFailure {
            step: KeeperStepLabel::StartSupervisorUnit(SupervisorUnitTarget::NatsServer),
            reason: KeeperStepFailureReason::SupervisorStartFailed,
            message: failure_message("simulated supervisor start failure"),
        },
    );
}

#[test]
fn keeper_plan_executor_runs_steps_in_order_and_records_progress() {
    let plan = first_machine_install_plan(FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
        test_identity().clone(),
    ));
    let mut effects = RecordingEffects::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);
    let rendered = format!("{execution:?}");

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    assert!(rendered.contains("/usr/local/bin/ployzd"));
    assert!(rendered.contains(PLOYZD_DIGEST));
    assert_eq!(effects.calls.len(), plan.steps().len());
    assert_eq!(recorder.events, execution.events);
    let [first, second, third, fourth, ..] = effects.calls.as_slice() else {
        panic!("plan records at least four calls");
    };
    assert_eq!(
        *first,
        KeeperStepLabel::VerifyHost(HostPrerequisite::LinuxRootSystemd)
    );
    assert_eq!(
        *second,
        KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker)
    );
    assert_eq!(
        *third,
        KeeperStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker)
    );
    assert_eq!(*fourth, KeeperStepLabel::InstallArtifact(ployzd_artifact()));
    let [
        _,
        _,
        _,
        _,
        fifth,
        sixth,
        seventh,
        eighth,
        ninth,
        tenth,
        eleventh,
        ..,
    ] = effects.calls.as_slice()
    else {
        panic!("first-machine plan records nats setup calls");
    };
    assert_eq!(
        *fifth,
        KeeperStepLabel::InstallArtifact(ebpf_bytecode_artifact())
    );
    assert_eq!(
        *sixth,
        KeeperStepLabel::InstallArtifact(ebpf_ctl_artifact())
    );
    assert_eq!(
        *seventh,
        KeeperStepLabel::InstallArtifact(nats_server_artifact())
    );
    assert_eq!(
        *eighth,
        KeeperStepLabel::WriteNatsTlsMaterial {
            state_dir: PathBuf::from("/var/lib/ployz/nats"),
        }
    );
    assert_eq!(
        *ninth,
        KeeperStepLabel::WriteNatsAuthorizedUsers {
            path: PathBuf::from("/etc/nats/authorized-users.conf"),
        }
    );
    assert_eq!(
        *tenth,
        KeeperStepLabel::WriteNatsClientCredentials {
            state_dir: PathBuf::from("/var/lib/ployz/nats"),
        }
    );
    assert_eq!(
        *eleventh,
        KeeperStepLabel::WriteNatsServerConfig(first_machine_nats_target(machine_id("machine_1")))
    );
    assert_eq!(
        execution.events,
        plan.steps()
            .iter()
            .flat_map(|step| {
                let label = KeeperStepLabel::from_step(step);
                [
                    KeeperStepEvent::Started {
                        step: label.clone(),
                    },
                    KeeperStepEvent::Succeeded { step: label },
                ]
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn keeper_plan_executor_stops_on_first_failed_step() {
    let plan = first_machine_plan();
    let ployzd_target = ployzd_artifact();
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::InstallArtifact(ployzd_target.clone())),
        fail_message: "simulated artifact install failure",
        ..RecordingEffects::default()
    };
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);
    let failure = KeeperStepFailure {
        step: KeeperStepLabel::InstallArtifact(ployzd_target.clone()),
        reason: KeeperStepFailureReason::ArtifactInstallFailed,
        message: failure_message("simulated artifact install failure"),
    };

    assert_eq!(
        execution.terminal,
        KeeperPlanTerminal::Failed(Box::new(KeeperPlanFailure::Step(failure.clone())))
    );
    assert_eq!(
        effects.calls,
        vec![
            KeeperStepLabel::VerifyHost(HostPrerequisite::LinuxRootSystemd),
            KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
            KeeperStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker),
            KeeperStepLabel::InstallArtifact(ployzd_target),
        ]
    );
    assert_eq!(
        execution.events.last(),
        Some(&KeeperStepEvent::Failed(failure))
    );
    assert_eq!(recorder.events, execution.events);
}

#[test]
fn keeper_progress_renders_container_runtime_steps() {
    assert_eq!(
        render_step_event(&KeeperStepEvent::Started {
            step: KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
        }),
        "started prepare-container-runtime docker"
    );
    assert_eq!(
        render_step_event(&KeeperStepEvent::Failed(KeeperStepFailure {
            step: KeeperStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker),
            reason: KeeperStepFailureReason::ContainerRuntimeVerifyFailed,
            message: failure_message("docker info failed"),
        })),
        "failed verify-container-runtime docker container-runtime-verify-failed: docker info failed"
    );
}

#[test]
fn keeper_plan_executor_records_started_before_applying_step() {
    let plan = first_machine_plan();
    let failed_event = KeeperStepEvent::Started {
        step: KeeperStepLabel::InstallArtifact(ployzd_artifact()),
    };
    let mut effects = RecordingEffects::default();
    let mut recorder = RecordingRecorder {
        fail_on: Some(failed_event.clone()),
        fail_message: "simulated event recorder failure",
        ..RecordingRecorder::default()
    };

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(
        execution.terminal,
        KeeperPlanTerminal::Failed(Box::new(KeeperPlanFailure::Record(KeeperRecordFailure {
            event: failed_event,
            message: failure_message("simulated event recorder failure"),
        })))
    );
    assert_eq!(
        effects.calls,
        vec![
            KeeperStepLabel::VerifyHost(HostPrerequisite::LinuxRootSystemd),
            KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
            KeeperStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker),
        ]
    );
    assert_eq!(execution.events, recorder.events);
}
