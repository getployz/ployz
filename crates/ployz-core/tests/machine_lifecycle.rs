use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenFingerprint, JoinTokenRedeemedAt,
    MachineActivationOutcome, MachineAddCommand, MachineAddFailure, MachineAddOperationState,
    MachineAddOperationStateName, MachineJoinOutcome, MachineReadinessCheck,
    MachineReadinessEvidence, MachineTransitionRejected, activate_joined_machine, plan_machine_add,
    redeem_join_token,
};
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, InstallRolePolicy};
use ployz_test_support::ids::{machine_id, machine_name, operation_id};

#[test]
fn machine_add_reserves_name_but_does_not_make_machine_schedulable() {
    let plan = plan_machine_add(machine_add_command(
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
    ));

    assert_eq!(
        plan.operation,
        MachineAddOperationState::Pending {
            join_token: issued_token()
        }
    );
    assert_eq!(
        plan.process_set.roles(),
        &[DaemonProcessRole::Machine(machine_id("machine_2"))]
    );
}

#[test]
fn machine_add_plan_defaults_gateway_and_dns_on_joining_machine() {
    let plan = plan_machine_add(machine_add_command(InstallRolePolicy::install_all()));

    assert_eq!(
        plan.process_set.roles(),
        &[
            DaemonProcessRole::Machine(machine_id("machine_2")),
            DaemonProcessRole::Gateway,
            DaemonProcessRole::Dns,
        ]
    );
}

#[test]
fn issued_join_token_serializes_only_the_fingerprint() {
    let json = serde_json::to_string(&issued_token()).expect("issued token serializes");

    assert!(!json.contains("join_once"));
    assert!(json.contains("join_hash"));
    assert!(!format!("{:?}", issued_token()).contains("join_once"));
}

#[test]
fn join_token_reuse_is_rejected_without_erasing_current_operation_state() {
    let plan = plan_machine_add(machine_add_command(
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
    ));
    let MachineJoinOutcome::Redeemed(joining) =
        redeem_join_token(plan.operation, &fingerprint("join_hash"), redeemed_at(50))
    else {
        panic!("first redemption should succeed");
    };
    let original = joining.clone();

    assert_eq!(
        redeem_join_token(joining, &fingerprint("join_hash"), redeemed_at(55)),
        MachineJoinOutcome::Rejected(MachineTransitionRejected {
            current: MachineAddOperationStateName::Joining,
        })
    );
    assert_eq!(
        original,
        MachineAddOperationState::Joining {
            joined_at: redeemed_at(50)
        }
    );
}

#[test]
fn expired_join_token_fails_the_operation_not_machine_truth() {
    let plan = plan_machine_add(machine_add_command(
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
    ));

    assert_eq!(
        redeem_join_token(plan.operation, &fingerprint("join_hash"), redeemed_at(120)),
        MachineJoinOutcome::Failed(MachineAddOperationState::Failed {
            failure: MachineAddFailure::JoinTokenExpired {
                expired_at: expires_at(100),
            },
        })
    );
}

#[test]
fn missing_readiness_check_fails_without_activating_machine() {
    let plan = plan_machine_add(machine_add_command(
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
    ));
    let MachineJoinOutcome::Redeemed(joining) =
        redeem_join_token(plan.operation, &fingerprint("join_hash"), redeemed_at(50))
    else {
        panic!("join token should redeem");
    };
    let evidence = MachineReadinessEvidence {
        nats_connection: MachineReadinessCheck::Confirmed,
        heartbeat: MachineReadinessCheck::Missing {
            reason: failure("heartbeat missing"),
        },
        machine_inspect: MachineReadinessCheck::Confirmed,
    };

    assert_eq!(
        activate_joined_machine(plan.reservation, joining, evidence.clone()),
        MachineActivationOutcome::Failed(MachineAddOperationState::Failed {
            failure: MachineAddFailure::ReadinessFailed { evidence },
        })
    );
}

#[test]
fn confirmed_readiness_activates_machine_truth() {
    let plan = plan_machine_add(machine_add_command(
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
    ));
    let MachineJoinOutcome::Redeemed(joining) =
        redeem_join_token(plan.operation, &fingerprint("join_hash"), redeemed_at(50))
    else {
        panic!("join token should redeem");
    };

    let MachineActivationOutcome::Completed {
        operation,
        active_machine,
    } = activate_joined_machine(
        plan.reservation,
        joining,
        MachineReadinessEvidence::confirmed(),
    )
    else {
        panic!("confirmed readiness should activate machine");
    };

    assert_eq!(operation, MachineAddOperationState::Completed);
    assert_eq!(active_machine.machine_id, machine_id("machine_2"));
    assert_eq!(active_machine.activated_by, operation_id("op_machine"));
}

fn machine_add_command(roles: InstallRolePolicy) -> MachineAddCommand {
    MachineAddCommand {
        operation_id: operation_id("op_machine"),
        machine_id: machine_id("machine_2"),
        name: machine_name("edge_2"),
        join_token: issued_token(),
        roles,
    }
}

fn issued_token() -> IssuedJoinToken {
    IssuedJoinToken::new(fingerprint("join_hash"), expires_at(100))
}

fn fingerprint(value: &str) -> JoinTokenFingerprint {
    JoinTokenFingerprint::try_new(value).expect("valid join token fingerprint")
}

fn expires_at(value: u64) -> JoinTokenExpiresAt {
    JoinTokenExpiresAt::try_new(value).expect("valid expiry")
}

fn redeemed_at(value: u64) -> JoinTokenRedeemedAt {
    JoinTokenRedeemedAt::try_new(value).expect("valid time")
}

fn failure(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}
