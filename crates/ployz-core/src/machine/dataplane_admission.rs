use std::collections::BTreeSet;

use crate::network::{
    DataplaneProjection, DataplaneProjectionMember, DataplaneProjectionRevisions,
    DataplaneProjectionTestimony, EbpfAttachmentStatus, EndpointBridgeStatus,
    MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS, MachineDataplaneStatus, WireGuardHandshakeStatus,
    WireGuardInterfaceMtu, WireGuardPeerEndpointSubnet,
};

use super::{
    DataplaneAdmissionPeer, DataplaneProjectionAdmissionFailure, WireGuardReadinessFailure,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionScope {
    Target,
    Declared,
}

impl AdmissionScope {
    fn expected_revision(
        self,
        projection: &DataplaneProjection,
    ) -> &crate::network::DataplaneProjectionRevision {
        match self {
            Self::Target => projection.target_revision(),
            Self::Declared => projection.declared_revision(),
        }
    }

    fn observed_revision(
        self,
        revisions: &DataplaneProjectionRevisions,
    ) -> crate::network::DataplaneProjectionRevision {
        match self {
            Self::Target => revisions.target_revision.clone(),
            Self::Declared => revisions.declared_revision.clone(),
        }
    }
}

#[must_use]
pub fn validate_target_machine(
    projection: &DataplaneProjection,
    member: &DataplaneProjectionMember,
    status: &MachineDataplaneStatus,
) -> Option<DataplaneProjectionAdmissionFailure> {
    if let Some(failure) = validate_revision(projection, status, AdmissionScope::Target) {
        return Some(failure);
    }
    if let Some(failure) = validate_local_readiness(member, status) {
        return Some(failure);
    }
    let target_members = projection.target_members().iter().collect::<Vec<_>>();
    validate_peer_set_and_handshakes(
        &target_members,
        &target_members,
        member,
        status,
        UnknownPeerPolicy::Reject,
    )
}

#[must_use]
pub fn validate_declared_local_machine(
    projection: &DataplaneProjection,
    member: &DataplaneProjectionMember,
    status: &MachineDataplaneStatus,
) -> Option<DataplaneProjectionAdmissionFailure> {
    if let Some(failure) = validate_revision(projection, status, AdmissionScope::Declared) {
        return Some(failure);
    }
    validate_local_readiness(member, status)
}

#[must_use]
pub fn validate_declared_machine(
    projection: &DataplaneProjection,
    member: &DataplaneProjectionMember,
    status: &MachineDataplaneStatus,
) -> Option<DataplaneProjectionAdmissionFailure> {
    if let Some(failure) = validate_declared_local_machine(projection, member, status) {
        return Some(failure);
    }
    let declared_members = projection.declared_members().iter().collect::<Vec<_>>();
    let target_members = projection.target_members().iter().collect::<Vec<_>>();
    validate_peer_set_and_handshakes(
        &declared_members,
        &target_members,
        member,
        status,
        UnknownPeerPolicy::Reject,
    )
}

#[must_use]
pub fn validate_placement_machine_peers(
    placement_members: &[&DataplaneProjectionMember],
    member: &DataplaneProjectionMember,
    status: &MachineDataplaneStatus,
) -> Option<DataplaneProjectionAdmissionFailure> {
    validate_peer_set_and_handshakes(
        placement_members,
        placement_members,
        member,
        status,
        UnknownPeerPolicy::Ignore,
    )
}

fn validate_revision(
    projection: &DataplaneProjection,
    status: &MachineDataplaneStatus,
    scope: AdmissionScope,
) -> Option<DataplaneProjectionAdmissionFailure> {
    let expected = scope.expected_revision(projection);
    match &status.projection.testimony {
        DataplaneProjectionTestimony::Applied { revisions }
            if scope.observed_revision(revisions) == *expected =>
        {
            None
        }
        DataplaneProjectionTestimony::Applied { revisions } => Some(
            DataplaneProjectionAdmissionFailure::AwaitingTargetRevision {
                expected: expected.clone(),
                observed: Some(scope.observed_revision(revisions)),
            },
        ),
        DataplaneProjectionTestimony::Unusable {
            attempted_revisions,
            failure,
            ..
        } if attempted_revisions
            .as_ref()
            .is_some_and(|revisions| scope.observed_revision(revisions) == *expected) =>
        {
            Some(DataplaneProjectionAdmissionFailure::UnusableProjection {
                failure: failure.clone(),
            })
        }
        DataplaneProjectionTestimony::Unusable {
            attempted_revisions,
            ..
        } => Some(
            DataplaneProjectionAdmissionFailure::AwaitingTargetRevision {
                expected: expected.clone(),
                observed: attempted_revisions
                    .as_ref()
                    .map(|revisions| scope.observed_revision(revisions)),
            },
        ),
    }
}

fn validate_local_readiness(
    member: &DataplaneProjectionMember,
    status: &MachineDataplaneStatus,
) -> Option<DataplaneProjectionAdmissionFailure> {
    if status.projection.endpoint_bridge
        != (EndpointBridgeStatus::Ready {
            subnet: member.endpoint_subnet.clone(),
        })
    {
        return Some(
            DataplaneProjectionAdmissionFailure::EndpointBridgeNotReady {
                status: status.projection.endpoint_bridge.clone(),
            },
        );
    }
    if status.wireguard.interface.is_empty() {
        return Some(DataplaneProjectionAdmissionFailure::WireGuardNotReady {
            failure: WireGuardReadinessFailure::InterfaceMissing,
        });
    }
    if let WireGuardInterfaceMtu::Unavailable { .. } = &status.wireguard.interface_mtu {
        return Some(DataplaneProjectionAdmissionFailure::WireGuardNotReady {
            failure: WireGuardReadinessFailure::InterfaceMtuUnavailable {
                observed: status.wireguard.interface_mtu.clone(),
            },
        });
    }
    if status.ebpf_attachment != EbpfAttachmentStatus::Attached {
        return Some(DataplaneProjectionAdmissionFailure::EbpfNotReady {
            status: status.ebpf_attachment.clone(),
        });
    }
    None
}

fn validate_peer_set_and_handshakes(
    required: &[&DataplaneProjectionMember],
    allowed: &[&DataplaneProjectionMember],
    member: &DataplaneProjectionMember,
    status: &MachineDataplaneStatus,
    unknown_peer_policy: UnknownPeerPolicy,
) -> Option<DataplaneProjectionAdmissionFailure> {
    let mut expected = required
        .iter()
        .filter(|peer| peer.machine_id != member.machine_id)
        .map(|peer| DataplaneAdmissionPeer {
            public_key: peer.wireguard_public_key.clone(),
            endpoint_subnet: WireGuardPeerEndpointSubnet::Valid {
                subnet: peer.endpoint_subnet.clone(),
            },
        })
        .collect::<Vec<_>>();
    let required_keys = required
        .iter()
        .map(|peer| &peer.wireguard_public_key)
        .collect::<BTreeSet<_>>();
    let allowed_keys = allowed
        .iter()
        .map(|peer| &peer.wireguard_public_key)
        .collect::<BTreeSet<_>>();
    let has_unknown_peer = matches!(unknown_peer_policy, UnknownPeerPolicy::Reject)
        && status
            .wireguard
            .peers
            .iter()
            .any(|peer| !allowed_keys.contains(&peer.public_key));
    let mut observed = status
        .wireguard
        .peers
        .iter()
        .filter(|peer| required_keys.contains(&peer.public_key))
        .map(|peer| DataplaneAdmissionPeer {
            public_key: peer.public_key.clone(),
            endpoint_subnet: peer.endpoint_subnet.clone(),
        })
        .collect::<Vec<_>>();
    expected.sort_by(|left, right| left.public_key.cmp(&right.public_key));
    observed.sort_by(|left, right| left.public_key.cmp(&right.public_key));
    if has_unknown_peer || expected != observed {
        return Some(DataplaneProjectionAdmissionFailure::PeerSetMismatch { expected, observed });
    }

    for peer in required
        .iter()
        .filter(|peer| peer.machine_id != member.machine_id)
    {
        let Some(observed) = status
            .wireguard
            .peers
            .iter()
            .find(|observed| observed.public_key == peer.wireguard_public_key)
        else {
            return Some(DataplaneProjectionAdmissionFailure::PeerSetMismatch {
                expected,
                observed,
            });
        };
        match observed.handshake {
            WireGuardHandshakeStatus::Ago { seconds }
                if seconds <= MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS => {}
            WireGuardHandshakeStatus::Ago { seconds } => {
                return Some(DataplaneProjectionAdmissionFailure::PeerHandshakeStale {
                    peer_machine_id: peer.machine_id.clone(),
                    observed_age_seconds: seconds,
                });
            }
            WireGuardHandshakeStatus::Never => {
                return Some(DataplaneProjectionAdmissionFailure::PeerHandshakeNever {
                    peer_machine_id: peer.machine_id.clone(),
                });
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnknownPeerPolicy {
    Reject,
    Ignore,
}
