use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use ployz::composition;
use ployz::error::MachineFailure;
use ployz::machine::{
    IrohEndpointId, MachineAddOutcome, MachineAddRequest, MachineEpoch, MachineId,
    MachineMembership, MachineMembershipPort, MachineMembershipService, MachineNetworkIdentity,
    MachineRemoval, MachineRemovalReason, MachineStatus, OverlayIp, WireGuardPublicKey,
};
use ployz::operation::{
    AuthorityEpoch, IdempotencyKey, MutationContext, OperationId, PrincipalId, ScopeId,
};

#[derive(Clone)]
struct FakeMembership {
    observed: Rc<RefCell<MachineStatus>>,
    joined: Rc<RefCell<Vec<MachineMembership>>>,
    join_result: Rc<RefCell<Option<MachineMembership>>>,
}

impl FakeMembership {
    fn new(observed: MachineStatus) -> Self {
        Self {
            observed: Rc::new(RefCell::new(observed)),
            joined: Rc::new(RefCell::new(Vec::new())),
            join_result: Rc::new(RefCell::new(None)),
        }
    }

    fn joined(&self) -> Vec<MachineMembership> {
        self.joined.borrow().clone()
    }

    fn return_join_result(&self, membership: MachineMembership) {
        self.join_result.replace(Some(membership));
    }
}

impl MachineMembershipPort for FakeMembership {
    fn observe(
        &self,
        _context: &MutationContext,
        _machine: &MachineId,
    ) -> Result<MachineStatus, MachineFailure> {
        Ok(self.observed.borrow().clone())
    }

    fn join(
        &self,
        _context: &MutationContext,
        membership: &MachineMembership,
    ) -> Result<MachineMembership, MachineFailure> {
        self.joined.borrow_mut().push(membership.clone());
        let joined = self
            .join_result
            .borrow()
            .clone()
            .unwrap_or_else(|| membership.clone());
        self.observed.replace(MachineStatus::Joined(joined.clone()));
        Ok(joined)
    }
}

#[test]
fn first_machine_add_observes_absent_then_writes_join() {
    let membership = FakeMembership::new(MachineStatus::Absent);
    let service = MachineMembershipService::new(membership.clone());

    let outcome = service
        .add_machine(&context(), request("node-a", 1, "fd00::1"))
        .expect("machine add");

    assert!(
        matches!(outcome, MachineAddOutcome::Joined(joined) if joined.machine.as_str() == "node-a")
    );
    assert_eq!(membership.joined().len(), 1);
}

#[test]
fn existing_matching_machine_is_already_present_without_mutation() {
    let current = membership("node-a", 4, "fd00::1");
    let membership = FakeMembership::new(MachineStatus::Joined(current.clone()));
    let service = MachineMembershipService::new(membership.clone());

    let outcome = service
        .add_machine(&context(), request("node-a", 4, "fd00::1"))
        .expect("machine add");

    assert!(matches!(outcome, MachineAddOutcome::AlreadyPresent(joined) if joined == current));
    assert!(membership.joined().is_empty());
}

#[test]
fn existing_different_epoch_rejects_without_mutation() {
    let current = membership("node-a", 2, "fd00::1");
    let membership = FakeMembership::new(MachineStatus::Joined(current));
    let service = MachineMembershipService::new(membership.clone());

    let error = service
        .add_machine(&context(), request("node-a", 1, "fd00::1"))
        .expect_err("epoch conflict");

    assert!(matches!(
        error,
        MachineFailure::MembershipConflict {
            machine,
            epoch: Some(epoch)
        } if machine.as_str() == "node-a" && epoch.value() == 2
    ));
    assert!(membership.joined().is_empty());
}

#[test]
fn existing_conflicting_machine_identity_rejects_without_mutation() {
    let current = membership("node-a", 2, "fd00::2");
    let membership = FakeMembership::new(MachineStatus::Joined(current));
    let service = MachineMembershipService::new(membership.clone());

    let error = service
        .add_machine(&context(), request("node-a", 1, "fd00::1"))
        .expect_err("conflict");

    assert!(matches!(
        error,
        MachineFailure::MembershipConflict {
            machine,
            epoch: Some(epoch)
        } if machine.as_str() == "node-a" && epoch.value() == 2
    ));
    assert!(membership.joined().is_empty());
}

#[test]
fn removing_machine_rejects_without_mutation() {
    let membership = FakeMembership::new(MachineStatus::Removing(MachineRemoval {
        machine: MachineId::parse("node-a").expect("machine"),
        epoch: MachineEpoch::new(9).expect("epoch"),
        reason: MachineRemovalReason::parse("operator requested").expect("reason"),
    }));
    let service = MachineMembershipService::new(membership.clone());

    let error = service
        .add_machine(&context(), request("node-a", 1, "fd00::1"))
        .expect_err("removing");

    assert!(matches!(
        error,
        MachineFailure::MachineRemoving { machine, epoch }
            if machine.as_str() == "node-a" && epoch.value() == 9
    ));
    assert!(membership.joined().is_empty());
}

#[test]
fn tombstoned_machine_rejects_without_mutation() {
    let membership = FakeMembership::new(MachineStatus::Tombstoned(
        ployz::machine::MachineTombstone {
            machine: MachineId::parse("node-a").expect("machine"),
            epoch: MachineEpoch::new(8).expect("epoch"),
        },
    ));
    let service = MachineMembershipService::new(membership.clone());

    let error = service
        .add_machine(&context(), request("node-a", 1, "fd00::1"))
        .expect_err("tombstoned");

    assert!(matches!(
        error,
        MachineFailure::MachineTombstoned { machine, epoch }
            if machine.as_str() == "node-a" && epoch.value() == 8
    ));
    assert!(membership.joined().is_empty());
}

#[test]
fn mismatched_join_result_is_membership_conflict() {
    let membership = FakeMembership::new(MachineStatus::Absent);
    membership.return_join_result(membership_record("node-a", 1, "fd00::2"));
    let service = MachineMembershipService::new(membership.clone());

    let error = service
        .add_machine(&context(), request("node-a", 1, "fd00::1"))
        .expect_err("mismatch");

    assert!(matches!(
        error,
        MachineFailure::MembershipConflict {
            machine,
            epoch: Some(epoch)
        } if machine.as_str() == "node-a" && epoch.value() == 1
    ));
    assert_eq!(
        membership.joined(),
        vec![membership_record("node-a", 1, "fd00::1")]
    );
}

#[test]
fn stale_absent_observation_rejects_different_epoch_join_result() {
    let membership = FakeMembership::new(MachineStatus::Absent);
    membership.return_join_result(membership_record("node-a", 4, "fd00::1"));
    let service = MachineMembershipService::new(membership.clone());

    let error = service
        .add_machine(&context(), request("node-a", 1, "fd00::1"))
        .expect_err("epoch conflict");

    assert!(matches!(
        error,
        MachineFailure::MembershipConflict {
            machine,
            epoch: Some(epoch)
        } if machine.as_str() == "node-a" && epoch.value() == 4
    ));
    assert_eq!(
        membership.joined(),
        vec![membership_record("node-a", 1, "fd00::1")]
    );
}

#[test]
fn fact_backed_machine_add_reuses_projected_membership() {
    let membership = composition::in_memory_machine_membership();
    let service = MachineMembershipService::new(membership);

    let first = service
        .add_machine(&context(), request("node-a", 1, "fd00::1"))
        .expect("first add");
    let second = service
        .add_machine(&context(), request("node-a", 1, "fd00::1"))
        .expect("same add");

    assert!(
        matches!(first, MachineAddOutcome::Joined(joined) if joined == membership_record("node-a", 1, "fd00::1"))
    );
    assert!(
        matches!(second, MachineAddOutcome::AlreadyPresent(joined) if joined == membership_record("node-a", 1, "fd00::1"))
    );
}

#[test]
fn fact_backed_machine_add_rejects_different_epoch() {
    let membership = composition::in_memory_machine_membership();
    let service = MachineMembershipService::new(membership);

    service
        .add_machine(&context(), request("node-a", 1, "fd00::1"))
        .expect("first add");
    let error = service
        .add_machine(&context(), request("node-a", 2, "fd00::1"))
        .expect_err("epoch conflict");

    assert!(matches!(
        error,
        MachineFailure::MembershipConflict {
            machine,
            epoch: Some(epoch)
        } if machine.as_str() == "node-a" && epoch.value() == 1
    ));
}

fn context() -> MutationContext {
    MutationContext::test_authorized(
        OperationId::parse("machine-add-1").expect("operation"),
        IdempotencyKey::parse("machine-add-1").expect("idempotency"),
        PrincipalId::parse("node-a").expect("principal"),
        ScopeId::parse("cluster").expect("scope"),
        AuthorityEpoch::new(7),
        None,
        UNIX_EPOCH + Duration::from_secs(60),
    )
}

fn request(machine: &str, epoch: u64, overlay_ip: &str) -> MachineAddRequest {
    MachineAddRequest {
        machine: MachineId::parse(machine).expect("machine"),
        epoch: MachineEpoch::new(epoch).expect("epoch"),
        network: network(overlay_ip),
    }
}

fn membership(machine: &str, epoch: u64, overlay_ip: &str) -> MachineMembership {
    membership_record(machine, epoch, overlay_ip)
}

fn membership_record(machine: &str, epoch: u64, overlay_ip: &str) -> MachineMembership {
    MachineMembership::new(
        MachineId::parse(machine).expect("machine"),
        MachineEpoch::new(epoch).expect("epoch"),
        network(overlay_ip),
    )
}

fn network(overlay_ip: &str) -> MachineNetworkIdentity {
    MachineNetworkIdentity::new(
        OverlayIp::parse(overlay_ip).expect("overlay ip"),
        IrohEndpointId::parse("iroh-node-a").expect("iroh endpoint"),
        WireGuardPublicKey::parse("wg-node-a").expect("wireguard key"),
    )
}
