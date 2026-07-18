use super::{CandidateEndpointState, CandidateOutcome, ProjectionEvidenceRecord};
use ployz_core::ingress::{
    IngressEndpointProjection, IngressEndpointProjectionState, IngressEndpointSet,
    IngressEndpointUnavailableReason,
};

pub(super) fn project_refresh(
    previous: &ProjectionEvidenceRecord,
    mut outcomes: Vec<CandidateOutcome>,
) -> ProjectionEvidenceRecord {
    outcomes.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    let next_state = if outcomes.is_empty() {
        IngressEndpointProjectionState::Unavailable {
            reason: IngressEndpointUnavailableReason::NoDeclaredGateways,
        }
    } else if outcomes
        .iter()
        .any(|outcome| matches!(&outcome.state, CandidateEndpointState::Publishable { .. }))
    {
        let endpoints = outcomes
            .iter()
            .filter_map(|outcome| match &outcome.state {
                CandidateEndpointState::Publishable { endpoints } => Some(endpoints),
                CandidateEndpointState::Indecisive | CandidateEndpointState::Unavailable => None,
            })
            .collect::<Vec<_>>();
        endpoint_state(endpoints)
    } else if outcomes
        .iter()
        .all(|outcome| outcome.state == CandidateEndpointState::Unavailable)
    {
        IngressEndpointProjectionState::Unavailable {
            reason: IngressEndpointUnavailableReason::NoPublishableEndpoints,
        }
    } else {
        retained_after_indecisive_gather(&previous.projection.state)
    };
    let revision =
        previous.projection.revision + u64::from(next_state != previous.projection.state);
    let publishable_gateway_ids = outcomes
        .iter()
        .filter(|outcome| matches!(&outcome.state, CandidateEndpointState::Publishable { .. }))
        .map(|outcome| outcome.machine_id.clone())
        .collect();
    ProjectionEvidenceRecord {
        projection: IngressEndpointProjection {
            control_plane_epoch: previous.projection.control_plane_epoch,
            revision,
            state: next_state,
        },
        publishable_gateway_ids,
    }
}

fn retained_after_indecisive_gather(
    previous: &IngressEndpointProjectionState,
) -> IngressEndpointProjectionState {
    match previous {
        IngressEndpointProjectionState::Current { endpoints } => {
            IngressEndpointProjectionState::Retained {
                endpoints: endpoints.clone(),
            }
        }
        IngressEndpointProjectionState::Retained { endpoints } => {
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

fn endpoint_state(endpoints: Vec<&IngressEndpointSet>) -> IngressEndpointProjectionState {
    let ipv4 = endpoints
        .iter()
        .flat_map(|endpoints| endpoints.ipv4().iter().copied());
    let ipv6 = endpoints
        .iter()
        .flat_map(|endpoints| endpoints.ipv6().iter().copied());
    match IngressEndpointSet::try_new(ipv4, ipv6) {
        Ok(endpoints) => IngressEndpointProjectionState::Current { endpoints },
        Err(_) => IngressEndpointProjectionState::Unavailable {
            reason: IngressEndpointUnavailableReason::NoPublishableEndpoints,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::MachineId;
    use ployz_core::intent::recovery::ControlPlaneEpoch;
    use std::net::IpAddr;

    fn pending() -> ProjectionEvidenceRecord {
        ProjectionEvidenceRecord::pending(ControlPlaneEpoch::initial())
    }

    fn publishable(machine: &str, addresses: &[&str]) -> CandidateOutcome {
        let addresses = addresses
            .iter()
            .map(|address| address.parse::<IpAddr>().expect("address"))
            .collect::<Vec<_>>();
        let ipv4 = addresses.iter().filter_map(|address| match address {
            IpAddr::V4(address) => Some(*address),
            IpAddr::V6(_) => None,
        });
        let ipv6 = addresses.iter().filter_map(|address| match address {
            IpAddr::V4(_) => None,
            IpAddr::V6(address) => Some(*address),
        });
        CandidateOutcome {
            machine_id: MachineId::try_new(machine).expect("machine id"),
            state: CandidateEndpointState::Publishable {
                endpoints: IngressEndpointSet::try_new(ipv4, ipv6).expect("endpoint set"),
            },
        }
    }

    fn indecisive(machine: &str) -> CandidateOutcome {
        CandidateOutcome {
            machine_id: MachineId::try_new(machine).expect("machine id"),
            state: CandidateEndpointState::Indecisive,
        }
    }

    fn unavailable(machine: &str) -> CandidateOutcome {
        CandidateOutcome {
            machine_id: MachineId::try_new(machine).expect("machine id"),
            state: CandidateEndpointState::Unavailable,
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
                publishable("machine_a", &["8.8.8.8"]),
                indecisive("machine_b"),
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
        assert_eq!(
            projected.publishable_gateway_ids,
            vec![MachineId::try_new("machine_a").expect("machine id")]
        );
    }

    #[test]
    fn total_silence_retains_current_complete_set() {
        let current = project_refresh(&pending(), vec![publishable("machine_a", &["1.1.1.1"])]);
        let retained = project_refresh(&current, vec![indecisive("machine_a")]);
        assert!(matches!(
            retained.projection.state,
            IngressEndpointProjectionState::Retained { .. }
        ));
        assert!(retained.publishable_gateway_ids.is_empty());
        assert_eq!(
            retained.projection.revision,
            current.projection.revision + 1
        );
    }

    #[test]
    fn indecisive_candidate_keeps_pending_projection_pending() {
        let projected = project_refresh(&pending(), vec![indecisive("machine_a")]);
        assert_eq!(
            projected.projection.state,
            IngressEndpointProjectionState::Pending
        );
    }

    #[test]
    fn all_explicitly_unavailable_candidates_authorize_withdrawal() {
        let projected = project_refresh(
            &pending(),
            vec![unavailable("machine_a"), unavailable("machine_b")],
        );
        assert_eq!(
            projected.projection.state,
            IngressEndpointProjectionState::Unavailable {
                reason: IngressEndpointUnavailableReason::NoPublishableEndpoints,
            }
        );
    }

    #[test]
    fn unavailable_and_indecisive_candidates_retain_current_projection() {
        let current = project_refresh(&pending(), vec![publishable("machine_a", &["1.1.1.1"])]);
        let retained = project_refresh(
            &current,
            vec![unavailable("machine_a"), indecisive("machine_b")],
        );
        assert!(matches!(
            retained.projection.state,
            IngressEndpointProjectionState::Retained { .. }
        ));
    }

    #[test]
    fn identical_refresh_keeps_revision_stable() {
        let first = project_refresh(&pending(), vec![publishable("machine_a", &["1.1.1.1"])]);
        let repeated = project_refresh(&first, vec![publishable("machine_a", &["1.1.1.1"])]);
        assert_eq!(repeated, first);
    }
}
