use std::collections::BTreeSet;

use ployz_core::ids::MachineRowId;
use ployz_core::{
    HandshakeObservationOutcome, HandshakeObservationUnavailable, OperationLookupReply,
    OperationWatchRefusal,
};

/// Provenance of a watch request at the operation owner-routing seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OperationWatchIngress {
    External,
    AuthenticatedMachineHop,
}

/// A resolved nonrecursive detail source. Remote targets are handed to the authenticated
/// machine-hop transport; they are never sent back through owner resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OperationWatchOwner {
    Local,
    Remote { machine_id: MachineRowId },
    OwnerNoLongerRostered,
    ProxyLoop,
}

pub(super) fn resolve_operation_watch_owner(
    local_machine_id: &MachineRowId,
    owner_machine_id: &MachineRowId,
    rostered_machine_ids: &BTreeSet<MachineRowId>,
    ingress: OperationWatchIngress,
) -> OperationWatchOwner {
    if owner_machine_id == local_machine_id {
        return OperationWatchOwner::Local;
    }
    if ingress == OperationWatchIngress::AuthenticatedMachineHop {
        return OperationWatchOwner::ProxyLoop;
    }
    if !rostered_machine_ids.contains(owner_machine_id) {
        return OperationWatchOwner::OwnerNoLongerRostered;
    }
    OperationWatchOwner::Remote {
        machine_id: owner_machine_id.clone(),
    }
}

pub(super) fn owner_routing_refusal(
    resolution: OperationWatchOwner,
    operation: OperationLookupReply,
) -> Option<OperationWatchRefusal> {
    match resolution {
        OperationWatchOwner::Local | OperationWatchOwner::Remote { .. } => None,
        OperationWatchOwner::OwnerNoLongerRostered => {
            Some(OperationWatchRefusal::OwnerNoLongerRostered {
                operation,
                observation: HandshakeObservationOutcome::Unavailable {
                    reason: HandshakeObservationUnavailable::OwnerNotRostered,
                },
            })
        }
        OperationWatchOwner::ProxyLoop => Some(OperationWatchRefusal::ProxyLoop),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ployz_core::ids::MachineRowId;

    use super::{OperationWatchIngress, OperationWatchOwner, resolve_operation_watch_owner};

    fn machine(value: &str) -> MachineRowId {
        MachineRowId::try_new(value).expect("machine")
    }

    #[test]
    fn authenticated_hop_terminates_at_the_owner_and_never_recurses() {
        let local = machine("01J00000000000000000000031");
        let remote = machine("01J00000000000000000000032");
        let roster = BTreeSet::from([local.clone(), remote.clone()]);
        assert_eq!(
            resolve_operation_watch_owner(
                &local,
                &local,
                &roster,
                OperationWatchIngress::AuthenticatedMachineHop,
            ),
            OperationWatchOwner::Local
        );
        assert_eq!(
            resolve_operation_watch_owner(
                &local,
                &remote,
                &roster,
                OperationWatchIngress::AuthenticatedMachineHop,
            ),
            OperationWatchOwner::ProxyLoop
        );
    }

    #[test]
    fn external_watch_proxies_only_to_a_rostered_owner() {
        let local = machine("01J00000000000000000000031");
        let remote = machine("01J00000000000000000000032");
        assert_eq!(
            resolve_operation_watch_owner(
                &local,
                &remote,
                &BTreeSet::new(),
                OperationWatchIngress::External,
            ),
            OperationWatchOwner::OwnerNoLongerRostered
        );
        assert_eq!(
            resolve_operation_watch_owner(
                &local,
                &remote,
                &BTreeSet::from([remote.clone()]),
                OperationWatchIngress::External,
            ),
            OperationWatchOwner::Remote { machine_id: remote }
        );
    }
}
