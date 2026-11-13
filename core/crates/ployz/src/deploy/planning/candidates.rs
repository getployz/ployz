//! Which Machines may take a Service's Containers, decided once per Machine on the
//! snapshot.

use ployz_core::{
    MachineObservation, MembershipObservation, Namespace, RequestedServiceSpec,
    ServicePlacementEligibility, ServicePlacementIneligibleReason,
};

use crate::deploy::DeploySnapshot;

/// Why a Machine cannot take a Service's Containers in this Deploy. A Machine marked
/// leaving is no exclusion: it is no placement rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Exclusion {
    /// The Service's placement names other Machines.
    PlacementMismatch,
    /// Membership says the Machine is down and no RPC went to it.
    Down,
    /// The Machine did not answer this Deploy's Container listing.
    Silent,
    /// The Machine does not accept this kind of work.
    WorkNotAccepted,
    /// Membership is neither up nor down: no RPC goes to the Machine.
    Unreached,
}

/// One Machine of a snapshot read for one Service: a candidate with its storage
/// eligibility, or excluded with the first reason that applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Candidacy {
    Candidate(ServicePlacementEligibility),
    Excluded(Exclusion),
}

/// The Machines of a snapshot, in snapshot order, each classified once for one Service.
pub(super) struct Candidates<'snapshot>(Vec<(&'snapshot MachineObservation, Candidacy)>);

impl DeploySnapshot {
    /// Classify each Machine for `requested` in `namespace`.
    pub(super) fn candidates(
        &self,
        requested: &RequestedServiceSpec,
        namespace: &Namespace,
    ) -> Candidates<'_> {
        Candidates(
            self.machines
                .iter()
                .map(|machine| {
                    let candidacy = match requested.placement_eligibility_in_namespace(
                        namespace,
                        &machine.machine,
                        machine.storage.as_ref(),
                    ) {
                        ServicePlacementEligibility::Ineligible(
                            ServicePlacementIneligibleReason::PlacementMismatch,
                        ) => Candidacy::Excluded(Exclusion::PlacementMismatch),
                        _ if !machine.invites_rpc()
                            && machine.membership == MembershipObservation::Down =>
                        {
                            Candidacy::Excluded(Exclusion::Down)
                        }
                        _ if self.silent(machine) => Candidacy::Excluded(Exclusion::Silent),
                        ServicePlacementEligibility::Ineligible(
                            ServicePlacementIneligibleReason::WorkNotAccepted,
                        ) => Candidacy::Excluded(Exclusion::WorkNotAccepted),
                        _ if !machine.invites_rpc() => Candidacy::Excluded(Exclusion::Unreached),
                        eligibility => Candidacy::Candidate(eligibility),
                    };
                    (machine, candidacy)
                })
                .collect(),
        )
    }
}

impl<'snapshot> Candidates<'snapshot> {
    /// The candidates with their storage eligibility, in snapshot order.
    pub(super) fn placeable(
        &self,
    ) -> Vec<(&'snapshot MachineObservation, ServicePlacementEligibility)> {
        self.0
            .iter()
            .filter_map(|(machine, candidacy)| match candidacy {
                Candidacy::Candidate(eligibility) => Some((*machine, *eligibility)),
                Candidacy::Excluded(_) => None,
            })
            .collect()
    }

    /// The Machines excluded for `why`, in snapshot order.
    pub(super) fn excluded(&self, why: Exclusion) -> Vec<&'snapshot MachineObservation> {
        self.0
            .iter()
            .filter(|(_, candidacy)| *candidacy == Candidacy::Excluded(why))
            .map(|(machine, _)| *machine)
            .collect()
    }

    /// Whether no Machine of the snapshot matches the Service's placement.
    pub(super) fn matches_none(&self) -> bool {
        self.0
            .iter()
            .all(|(_, candidacy)| *candidacy == Candidacy::Excluded(Exclusion::PlacementMismatch))
    }

    /// Whether the snapshot read no Machine.
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
