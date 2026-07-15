use std::net::IpAddr;

use ployz_core::ingress::{
    IngressEndpointProjection, IngressEndpointProjectionState, IngressEndpointSet,
    IngressEndpointUnavailableReason,
};
use ployz_core::operation::{
    IngressRefreshCandidateEvidence, IngressRefreshCandidatePublication,
    IngressRefreshGatewayOutcome,
};

use super::ProjectionEvidenceRecord;

pub(super) fn project_refresh(
    previous: &ProjectionEvidenceRecord,
    mut outcomes: Vec<IngressRefreshCandidateEvidence>,
) -> ProjectionEvidenceRecord {
    outcomes.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    let next_state = if outcomes.is_empty() {
        IngressEndpointProjectionState::Unavailable {
            reason: IngressEndpointUnavailableReason::NoDeclaredGateways,
        }
    } else if !outcomes
        .iter()
        .any(|outcome| gateway_is_valid_reply(outcome.gateway))
    {
        retained_after_total_silence(&previous.projection.state)
    } else {
        let addresses = outcomes
            .iter()
            .flat_map(|outcome| match &outcome.publication {
                IngressRefreshCandidatePublication::Published { addresses } => addresses.as_slice(),
                IngressRefreshCandidatePublication::Excluded { .. } => &[],
            })
            .copied()
            .collect::<Vec<_>>();
        endpoint_state(addresses)
    };
    let revision =
        previous.projection.revision + u64::from(next_state != previous.projection.state);
    let publishable_gateway_ids = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome.publication,
                IngressRefreshCandidatePublication::Published { .. }
            )
        })
        .map(|outcome| outcome.machine_id.clone())
        .collect();
    ProjectionEvidenceRecord {
        projection: IngressEndpointProjection {
            control_plane_epoch: previous.projection.control_plane_epoch,
            revision,
            state: next_state,
        },
        candidate_outcomes: outcomes,
        publishable_gateway_ids,
    }
}

fn retained_after_total_silence(
    previous: &IngressEndpointProjectionState,
) -> IngressEndpointProjectionState {
    match previous {
        IngressEndpointProjectionState::Current { endpoints }
        | IngressEndpointProjectionState::Retained { endpoints } => {
            IngressEndpointProjectionState::Retained {
                endpoints: endpoints.clone(),
            }
        }
        IngressEndpointProjectionState::Pending => IngressEndpointProjectionState::Pending,
        IngressEndpointProjectionState::Unavailable { reason } => {
            IngressEndpointProjectionState::Unavailable { reason: *reason }
        }
    }
}

fn endpoint_state(addresses: Vec<IpAddr>) -> IngressEndpointProjectionState {
    let ipv4 = addresses.iter().filter_map(|address| match address {
        IpAddr::V4(address) => Some(*address),
        IpAddr::V6(_) => None,
    });
    let ipv6 = addresses.iter().filter_map(|address| match address {
        IpAddr::V4(_) => None,
        IpAddr::V6(address) => Some(*address),
    });
    match IngressEndpointSet::try_new(ipv4, ipv6) {
        Ok(endpoints) => IngressEndpointProjectionState::Current { endpoints },
        Err(_) => IngressEndpointProjectionState::Unavailable {
            reason: IngressEndpointUnavailableReason::NoPublishableEndpoints,
        },
    }
}

const fn gateway_is_valid_reply(outcome: IngressRefreshGatewayOutcome) -> bool {
    matches!(
        outcome,
        IngressRefreshGatewayOutcome::Current
            | IngressRefreshGatewayOutcome::LastKnownGood
            | IngressRefreshGatewayOutcome::Unavailable
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::MachineId;
    use ployz_core::intent::recovery::ControlPlaneEpoch;
    use ployz_core::operation::{IngressRefreshExclusionReason, IngressRefreshFactsOutcome};

    fn pending() -> ProjectionEvidenceRecord {
        ProjectionEvidenceRecord::pending(ControlPlaneEpoch::initial())
    }

    fn outcome(
        machine: &str,
        gateway: IngressRefreshGatewayOutcome,
        addresses: &[&str],
    ) -> IngressRefreshCandidateEvidence {
        let addresses = addresses
            .iter()
            .map(|address| address.parse().expect("address"))
            .collect::<Vec<_>>();
        IngressRefreshCandidateEvidence {
            machine_id: MachineId::try_new(machine).expect("machine id"),
            gateway,
            facts: IngressRefreshFactsOutcome::Responded {
                public_control_endpoints: addresses.clone(),
            },
            publication: IngressRefreshCandidatePublication::Published { addresses },
        }
    }

    #[test]
    fn zero_candidates_is_decisive_withdrawal() {
        let projected = project_refresh(&pending(), Vec::new());
        assert_eq!(
            projected.projection.state,
            IngressEndpointProjectionState::Unavailable {
                reason: IngressEndpointUnavailableReason::NoDeclaredGateways,
            }
        );
    }

    #[test]
    fn one_valid_responder_removes_silent_peers() {
        let projected = project_refresh(
            &pending(),
            vec![
                outcome(
                    "machine_a",
                    IngressRefreshGatewayOutcome::Current,
                    &["8.8.8.8"],
                ),
                IngressRefreshCandidateEvidence {
                    machine_id: MachineId::try_new("machine_b").expect("machine id"),
                    gateway: IngressRefreshGatewayOutcome::TimedOut,
                    facts: IngressRefreshFactsOutcome::TimedOut,
                    publication: IngressRefreshCandidatePublication::Excluded {
                        reason: IngressRefreshExclusionReason::GatewayTestimonyFailed,
                    },
                },
            ],
        );
        let IngressEndpointProjectionState::Current { endpoints } = projected.projection.state
        else {
            panic!("current projection")
        };
        assert_eq!(
            endpoints.ipv4().iter().copied().collect::<Vec<_>>(),
            vec!["8.8.8.8".parse::<std::net::Ipv4Addr>().expect("address")]
        );
    }

    #[test]
    fn total_silence_retains_current_complete_set() {
        let current = project_refresh(
            &pending(),
            vec![outcome(
                "machine_a",
                IngressRefreshGatewayOutcome::Current,
                &["1.1.1.1"],
            )],
        );
        let retained = project_refresh(
            &current,
            vec![IngressRefreshCandidateEvidence {
                machine_id: MachineId::try_new("machine_a").expect("machine id"),
                gateway: IngressRefreshGatewayOutcome::TimedOut,
                facts: IngressRefreshFactsOutcome::TimedOut,
                publication: IngressRefreshCandidatePublication::Excluded {
                    reason: IngressRefreshExclusionReason::GatewayTestimonyFailed,
                },
            }],
        );
        assert!(matches!(
            retained.projection.state,
            IngressEndpointProjectionState::Retained { .. }
        ));
        assert_eq!(
            retained.projection.revision,
            current.projection.revision + 1
        );
    }

    #[test]
    fn valid_gateway_without_facts_is_decisive_empty_union() {
        let projected = project_refresh(
            &pending(),
            vec![IngressRefreshCandidateEvidence {
                machine_id: MachineId::try_new("machine_a").expect("machine id"),
                gateway: IngressRefreshGatewayOutcome::Current,
                facts: IngressRefreshFactsOutcome::TimedOut,
                publication: IngressRefreshCandidatePublication::Excluded {
                    reason: IngressRefreshExclusionReason::FactsTestimonyFailed,
                },
            }],
        );
        assert_eq!(
            projected.projection.state,
            IngressEndpointProjectionState::Unavailable {
                reason: IngressEndpointUnavailableReason::NoPublishableEndpoints,
            }
        );
    }

    #[test]
    fn identical_refresh_keeps_revision_stable() {
        let first = project_refresh(
            &pending(),
            vec![outcome(
                "machine_a",
                IngressRefreshGatewayOutcome::Current,
                &["1.1.1.1"],
            )],
        );
        let repeated = project_refresh(
            &first,
            vec![outcome(
                "machine_a",
                IngressRefreshGatewayOutcome::Current,
                &["1.1.1.1"],
            )],
        );
        assert_eq!(repeated.projection.revision, first.projection.revision);
    }
}
