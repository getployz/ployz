use ployz_core::deploy::VolumeName;
use ployz_core::operation::{
    EventSequence, OperationProjection, OperationStatus, VolumeRemoveOperationState,
    VolumeRemoveRunningStage, VolumeRemoveTransition, project_volume_remove_transition,
};
use ployz_test_support::ids::{namespace_id, operation_id};

#[test]
fn volume_remove_projects_its_single_running_stage_to_completion() {
    let accepted = OperationStatus::volume_remove_accepted(
        operation_id("op_volume_rm"),
        namespace_id("prod"),
        VolumeName::try_new("data").expect("valid volume name"),
        EventSequence::first(),
    );

    let running = project_volume_remove_transition(
        &accepted,
        VolumeRemoveTransition::Running {
            stage: VolumeRemoveRunningStage::RemovingVolumeData,
        },
        EventSequence::try_new(2).expect("valid event sequence"),
    )
    .expect("accepted operation enters running");
    let OperationProjection::StatusChanged { status: running } = running else {
        panic!("running transition changes status");
    };
    assert!(matches!(
        running.as_ref(),
        OperationStatus::VolumeRemove {
            state: VolumeRemoveOperationState::Running {
                stage: VolumeRemoveRunningStage::RemovingVolumeData,
            },
            ..
        }
    ));

    let completed = project_volume_remove_transition(
        &running,
        VolumeRemoveTransition::Completed,
        EventSequence::try_new(3).expect("valid event sequence"),
    )
    .expect("running operation completes");
    assert!(matches!(
        completed,
        OperationProjection::StatusChanged { status }
            if matches!(
                status.as_ref(),
                OperationStatus::VolumeRemove {
                    state: VolumeRemoveOperationState::Completed,
                    ..
                }
            )
    ));
}
