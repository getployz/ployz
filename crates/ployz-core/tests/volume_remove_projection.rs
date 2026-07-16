use ployz_core::deploy::{DatasetName, VolumeName, ZfsPoolName};
use ployz_core::operation::{
    EventSequence, FailureMessage, OperationProjection, OperationStatus, ProjectionOperationState,
    StatusProjectionError, VolumeRemoveFailure, VolumeRemoveOperationState,
    VolumeRemoveRunningStage, VolumeRemoveTransition, project_volume_remove_transition,
};
use ployz_test_support::ids::{event_sequence, machine_id, namespace_id, operation_id};

#[test]
fn provisioned_volume_remove_projects_dataset_destruction_before_completion() {
    let removing_volume_data =
        enter_running(&accepted(), VolumeRemoveRunningStage::RemovingVolumeData, 2);
    let removing_dataset = enter_running(
        &removing_volume_data,
        VolumeRemoveRunningStage::RemovingDataset,
        3,
    );

    let completed = project_volume_remove_transition(
        &removing_dataset,
        VolumeRemoveTransition::Completed,
        event_sequence(4),
    )
    .expect("dataset destruction completes the provisioned volume removal");

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

#[test]
fn plain_volume_remove_completes_after_removing_volume_data() {
    let removing_volume_data =
        enter_running(&accepted(), VolumeRemoveRunningStage::RemovingVolumeData, 2);

    assert!(matches!(
        project_volume_remove_transition(
            &removing_volume_data,
            VolumeRemoveTransition::Completed,
            event_sequence(3),
        )
        .expect("plain volume removal completes without dataset destruction"),
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

#[test]
fn volume_remove_rejects_dataset_stage_skip_and_reverse_transition() {
    assert_eq!(
        project_volume_remove_transition(
            &accepted(),
            VolumeRemoveTransition::Running {
                stage: VolumeRemoveRunningStage::RemovingDataset,
            },
            event_sequence(2),
        ),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_volume_rm"),
            current: Box::new(ProjectionOperationState::VolumeRemove(
                VolumeRemoveOperationState::Accepted,
            )),
            attempted: Box::new(ProjectionOperationState::VolumeRemove(
                VolumeRemoveOperationState::Running {
                    stage: VolumeRemoveRunningStage::RemovingDataset,
                },
            )),
        })
    );

    let removing_dataset = running(VolumeRemoveRunningStage::RemovingDataset, 3);
    assert_eq!(
        project_volume_remove_transition(
            &removing_dataset,
            VolumeRemoveTransition::Running {
                stage: VolumeRemoveRunningStage::RemovingVolumeData,
            },
            event_sequence(4),
        ),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_volume_rm"),
            current: Box::new(ProjectionOperationState::VolumeRemove(
                VolumeRemoveOperationState::Running {
                    stage: VolumeRemoveRunningStage::RemovingDataset,
                },
            )),
            attempted: Box::new(ProjectionOperationState::VolumeRemove(
                VolumeRemoveOperationState::Running {
                    stage: VolumeRemoveRunningStage::RemovingVolumeData,
                },
            )),
        })
    );
}

#[test]
fn volume_remove_failure_is_allowed_from_every_nonterminal_state() {
    let failure = dataset_destroy_failure();
    let states = [
        accepted(),
        running(VolumeRemoveRunningStage::RemovingVolumeData, 2),
        running(VolumeRemoveRunningStage::RemovingDataset, 3),
    ];

    for state in states {
        assert!(matches!(
            project_volume_remove_transition(
                &state,
                VolumeRemoveTransition::Failed {
                    failure: failure.clone(),
                },
                state.next_event_sequence().expect("next event sequence"),
            )
            .expect("a nonterminal volume removal may fail"),
            OperationProjection::StatusChanged { status } if status.is_terminal()
        ));
    }
}

#[test]
fn volume_remove_terminal_state_is_final() {
    let completed = OperationStatus::VolumeRemove {
        id: operation_id("op_volume_rm"),
        namespace_id: namespace_id("prod"),
        volume_name: volume_name(),
        state: VolumeRemoveOperationState::Completed,
        last_event_sequence: event_sequence(4),
    };

    assert_eq!(
        project_volume_remove_transition(
            &completed,
            VolumeRemoveTransition::Failed {
                failure: dataset_destroy_failure(),
            },
            event_sequence(5),
        ),
        Err(StatusProjectionError::TerminalState {
            operation_id: operation_id("op_volume_rm"),
            current: Box::new(ProjectionOperationState::VolumeRemove(
                VolumeRemoveOperationState::Completed,
            )),
            attempted: Box::new(ProjectionOperationState::VolumeRemove(
                VolumeRemoveOperationState::Failed {
                    failure: dataset_destroy_failure(),
                },
            )),
        })
    );
}

#[test]
fn dataset_destroy_failure_has_exact_wire_shape() {
    assert_eq!(
        serde_json::to_value(dataset_destroy_failure()).expect("failure serializes"),
        serde_json::json!({
            "kind": "dataset_destroy_failed",
            "machine_id": "machine_a",
            "dataset": "tank/ployz/volumes/ployz-n4-prod-v4-data",
            "message": "zfs destroy failed",
        })
    );
}

fn accepted() -> OperationStatus {
    OperationStatus::volume_remove_accepted(
        operation_id("op_volume_rm"),
        namespace_id("prod"),
        volume_name(),
        EventSequence::first(),
    )
}

fn running(stage: VolumeRemoveRunningStage, sequence: u64) -> OperationStatus {
    OperationStatus::VolumeRemove {
        id: operation_id("op_volume_rm"),
        namespace_id: namespace_id("prod"),
        volume_name: volume_name(),
        state: VolumeRemoveOperationState::Running { stage },
        last_event_sequence: event_sequence(sequence),
    }
}

fn enter_running(
    current: &OperationStatus,
    stage: VolumeRemoveRunningStage,
    sequence: u64,
) -> Box<OperationStatus> {
    let projection = project_volume_remove_transition(
        current,
        VolumeRemoveTransition::Running { stage },
        event_sequence(sequence),
    )
    .expect("volume removal enters the next running stage");
    let OperationProjection::StatusChanged { status } = projection else {
        panic!("running transition changes status");
    };
    status
}

fn volume_name() -> VolumeName {
    VolumeName::try_new("data").expect("valid volume name")
}

fn dataset() -> DatasetName {
    DatasetName::for_volume(
        &ZfsPoolName::try_new("tank").expect("valid pool name"),
        &namespace_id("prod"),
        &volume_name(),
    )
    .expect("valid dataset name")
}

fn dataset_destroy_failure() -> VolumeRemoveFailure {
    VolumeRemoveFailure::DatasetDestroyFailed {
        machine_id: machine_id("machine_a"),
        dataset: dataset(),
        message: FailureMessage::try_new("zfs destroy failed").expect("valid failure message"),
    }
}
