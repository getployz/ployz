//! Machine Add operation status projection: join lifecycle, subject
//! checks, failure-phase rules, and cancellation.

use ployz_core::install::HostPortAssurance;
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenFingerprint, MachineAddFailure,
    MachineReadinessCheck, MachineReadinessEvidence,
};
use ployz_core::operation::{
    FailureMessage, MachineAddOperationState, OperationEvent, OperationKind, OperationProjection,
    OperationStatus, ProjectionOperationState, StatusProjectionError, project_operation_event,
};
use ployz_core::roles::InstallRolePolicy;
use ployz_test_support::ids::{event_sequence, machine_id, machine_name, operation_id};

#[test]
fn machine_add_submitted_event_is_satisfied_by_accepted_status() {
    let accepted = OperationStatus::machine_add_pending(
        operation_id("op_machine"),
        machine_id("machine_2"),
        machine_name("edge_2"),
        InstallRolePolicy::install_all().without_gateway(),
        HostPortAssurance::Keeper,
        issued_join_token(),
        event_sequence(7),
    );

    assert_eq!(
        project_operation_event(
            &accepted,
            machine_add_submitted_event("machine_2"),
            event_sequence(8)
        ),
        Ok(OperationProjection::AlreadySatisfied)
    );
}

#[test]
fn machine_add_rejects_submitted_event_for_another_machine() {
    let accepted = OperationStatus::machine_add_pending(
        operation_id("op_machine"),
        machine_id("machine_2"),
        machine_name("edge_2"),
        InstallRolePolicy::install_all().without_gateway(),
        HostPortAssurance::Keeper,
        issued_join_token(),
        event_sequence(7),
    );

    assert_eq!(
        project_operation_event(
            &accepted,
            machine_add_submitted_event("machine_3"),
            event_sequence(8)
        ),
        Err(StatusProjectionError::OperationSubjectMismatch {
            operation_id: operation_id("op_machine"),
            expected: Box::new(ployz_core::operation::OperationSubjectRef::MachineAdd(
                machine_id("machine_2",)
            )),
            actual: Box::new(ployz_core::operation::OperationSubjectRef::MachineAdd(
                machine_id("machine_3",)
            )),
        })
    );
}

#[test]
fn machine_add_cancel_records_terminal_status() {
    let accepted = OperationStatus::machine_add_pending(
        operation_id("op_machine"),
        machine_id("machine_2"),
        machine_name("edge_2"),
        InstallRolePolicy::install_all().without_gateway(),
        HostPortAssurance::Keeper,
        issued_join_token(),
        event_sequence(7),
    );
    let reason = ployz_core::operation::CancellationReason::try_new("operator_cancelled")
        .expect("valid cancellation reason");

    assert_eq!(
        project_operation_event(
            &accepted,
            OperationEvent::Cancelled {
                kind: OperationKind::MachineAdd,
                operation_id: operation_id("op_machine"),
                reason: reason.clone(),
            },
            event_sequence(8),
        ),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::MachineAdd {
                id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
                name: machine_name("edge_2"),
                roles: InstallRolePolicy::install_all().without_gateway(),
                host_port_assurance: HostPortAssurance::Keeper,
                state: MachineAddOperationState::Cancelled { reason },
                last_event_sequence: event_sequence(8),
            }),
        })
    );
}

#[test]
fn cancel_with_mismatched_kind_is_typed_evidence_not_a_cancel() {
    let accepted = machine_add_pending_status();
    let reason = ployz_core::operation::CancellationReason::try_new("operator_cancelled")
        .expect("valid cancellation reason");

    assert_eq!(
        project_operation_event(
            &accepted,
            OperationEvent::Cancelled {
                kind: OperationKind::Deploy,
                operation_id: operation_id("op_machine"),
                reason,
            },
            event_sequence(8),
        ),
        Err(StatusProjectionError::OperationKindMismatch {
            operation_id: operation_id("op_machine"),
            expected: OperationKind::MachineAdd,
            actual: OperationKind::Deploy,
        })
    );
}

#[test]
fn machine_add_join_and_complete_record_lifecycle_status() {
    let pending = machine_add_pending_status();
    let joined_at =
        ployz_core::machine::JoinTokenRedeemedAt::try_new(650).expect("valid joined at");

    let joined = project_operation_event(
        &pending,
        OperationEvent::MachineAddJoined {
            operation_id: operation_id("op_machine"),
            machine_id: machine_id("machine_2"),
            joined_at,
        },
        event_sequence(8),
    )
    .expect("join projects");

    let OperationProjection::StatusChanged {
        status: joined_status,
    } = joined
    else {
        panic!("join should update status");
    };
    assert_eq!(
        joined_status.as_ref(),
        &OperationStatus::MachineAdd {
            id: operation_id("op_machine"),
            machine_id: machine_id("machine_2"),
            name: machine_name("edge_2"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: HostPortAssurance::Keeper,
            state: MachineAddOperationState::Joining { joined_at },
            last_event_sequence: event_sequence(8),
        }
    );

    assert_eq!(
        project_operation_event(
            &joined_status,
            OperationEvent::MachineAddCompleted {
                operation_id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
            },
            event_sequence(9),
        ),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::MachineAdd {
                id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
                name: machine_name("edge_2"),
                roles: InstallRolePolicy::install_all().without_gateway(),
                host_port_assurance: HostPortAssurance::Keeper,
                state: MachineAddOperationState::Completed,
                last_event_sequence: event_sequence(9),
            }),
        })
    );
}

#[test]
fn machine_add_join_token_failure_after_join_is_rejected() {
    let joined_at =
        ployz_core::machine::JoinTokenRedeemedAt::try_new(650).expect("valid joined at");
    let joined = machine_add_joining_status(joined_at);

    assert_eq!(
        project_operation_event(
            &joined,
            OperationEvent::MachineAddFailed {
                operation_id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
                failure: MachineAddFailure::JoinTokenExpired {
                    expired_at: JoinTokenExpiresAt::try_new(600).expect("valid expiry"),
                },
            },
            event_sequence(9),
        ),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_machine"),
            current: Box::new(ProjectionOperationState::MachineAdd(
                MachineAddOperationState::Joining { joined_at },
            )),
            attempted: Box::new(ProjectionOperationState::MachineAdd(
                MachineAddOperationState::Failed {
                    failure: MachineAddFailure::JoinTokenExpired {
                        expired_at: JoinTokenExpiresAt::try_new(600).expect("valid expiry"),
                    },
                },
            )),
        })
    );
}

#[test]
fn machine_add_duplicate_join_after_join_is_satisfied() {
    let joined_at =
        ployz_core::machine::JoinTokenRedeemedAt::try_new(650).expect("valid joined at");
    let joined = machine_add_joining_status(joined_at);

    assert_eq!(
        project_operation_event(
            &joined,
            OperationEvent::MachineAddJoined {
                operation_id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
                joined_at: ployz_core::machine::JoinTokenRedeemedAt::try_new(651)
                    .expect("valid later joined at"),
            },
            event_sequence(9),
        ),
        Ok(OperationProjection::AlreadySatisfied)
    );
}

#[test]
fn machine_add_readiness_failure_after_join_is_allowed() {
    let joined_at =
        ployz_core::machine::JoinTokenRedeemedAt::try_new(650).expect("valid joined at");
    let joined = machine_add_joining_status(joined_at);
    let failure = MachineAddFailure::ReadinessFailed {
        evidence: missing_heartbeat_readiness(),
    };

    assert_eq!(
        project_operation_event(
            &joined,
            OperationEvent::MachineAddFailed {
                operation_id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
                failure: failure.clone(),
            },
            event_sequence(9),
        ),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::MachineAdd {
                id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
                name: machine_name("edge_2"),
                roles: InstallRolePolicy::install_all().without_gateway(),
                host_port_assurance: HostPortAssurance::Keeper,
                state: MachineAddOperationState::Failed { failure },
                last_event_sequence: event_sequence(9),
            }),
        })
    );
}

#[test]
fn machine_add_bootstrap_failure_after_join_is_allowed() {
    let joined_at =
        ployz_core::machine::JoinTokenRedeemedAt::try_new(650).expect("valid joined at");
    let joined = machine_add_joining_status(joined_at);
    let failure = MachineAddFailure::BootstrapFailed {
        message: FailureMessage::try_new("artifact install failed").expect("valid failure message"),
    };

    assert_eq!(
        project_operation_event(
            &joined,
            OperationEvent::MachineAddFailed {
                operation_id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
                failure: failure.clone(),
            },
            event_sequence(9),
        ),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::MachineAdd {
                id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
                name: machine_name("edge_2"),
                roles: InstallRolePolicy::install_all().without_gateway(),
                host_port_assurance: HostPortAssurance::Keeper,
                state: MachineAddOperationState::Failed { failure },
                last_event_sequence: event_sequence(9),
            }),
        })
    );
}

#[test]
fn machine_add_completed_before_join_is_rejected() {
    let pending = machine_add_pending_status();

    assert_eq!(
        project_operation_event(
            &pending,
            OperationEvent::MachineAddCompleted {
                operation_id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
            },
            event_sequence(8),
        ),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_machine"),
            current: Box::new(ProjectionOperationState::MachineAdd(
                MachineAddOperationState::Pending {
                    join_token: issued_join_token(),
                }
            )),
            attempted: Box::new(ProjectionOperationState::MachineAdd(
                MachineAddOperationState::Completed
            )),
        })
    );
}

fn missing_heartbeat_readiness() -> MachineReadinessEvidence {
    MachineReadinessEvidence {
        nats_connection: MachineReadinessCheck::Confirmed,
        heartbeat: MachineReadinessCheck::Missing {
            reason: FailureMessage::try_new("heartbeat missing").expect("valid failure message"),
        },
        machine_inspect: MachineReadinessCheck::Confirmed,
    }
}

fn issued_join_token() -> IssuedJoinToken {
    IssuedJoinToken::new(
        JoinTokenFingerprint::try_new("join_hash").expect("valid join token fingerprint"),
        JoinTokenExpiresAt::try_new(700).expect("valid join token expiry"),
    )
}

fn machine_add_joining_status(
    joined_at: ployz_core::machine::JoinTokenRedeemedAt,
) -> OperationStatus {
    OperationStatus::MachineAdd {
        id: operation_id("op_machine"),
        machine_id: machine_id("machine_2"),
        name: machine_name("edge_2"),
        roles: InstallRolePolicy::install_all().without_gateway(),
        host_port_assurance: HostPortAssurance::Keeper,
        state: MachineAddOperationState::Joining { joined_at },
        last_event_sequence: event_sequence(8),
    }
}

fn machine_add_pending_status() -> OperationStatus {
    OperationStatus::machine_add_pending(
        operation_id("op_machine"),
        machine_id("machine_2"),
        machine_name("edge_2"),
        InstallRolePolicy::install_all().without_gateway(),
        HostPortAssurance::Keeper,
        issued_join_token(),
        event_sequence(7),
    )
}

fn machine_add_submitted_event(machine_id: &str) -> OperationEvent {
    OperationEvent::MachineAddSubmitted {
        operation_id: operation_id("op_machine"),
        machine_id: self::machine_id(machine_id),
        name: machine_name("edge_2"),
        roles: InstallRolePolicy::install_all().without_gateway(),
        host_port_assurance: HostPortAssurance::Keeper,
        join_token: issued_join_token(),
    }
}
