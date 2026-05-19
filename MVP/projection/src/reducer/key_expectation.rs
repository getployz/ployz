use mvp_acme::{AcmeChallengeToken, AcmeHostname};
use mvp_lease::{LeaseContentHash, LeaseEpoch, LeaseRelease, LeaseTimestamp};

use crate::facts::ProjectionFactPayload;
use crate::model::ProjectionIgnoreReason;
use crate::source::{CandidateStatus, FactCandidate, FactKind, is_reducible_conflict_kind};

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyExpectation {
    NodeJoined {
        node_id: String,
        epoch: u64,
    },
    NodeRemovalStarted {
        node_id: String,
        epoch: u64,
    },
    NodeTombstoned {
        node_id: String,
        epoch: u64,
    },
    PeerAdmitted {
        node_id: String,
        epoch: u64,
    },
    ServiceRegistered {
        service: String,
        node_id: String,
        epoch: u64,
    },
    RouteCommit {
        route_commit_id: String,
    },
    ServingCommit {
        serving_commit_id: String,
    },
    GatewayCommit {
        gateway_commit_id: String,
    },
    DnsCommit {
        dns_commit_id: String,
    },
    LeaseClaimed {
        resource: String,
        epoch: LeaseEpoch,
    },
    LeaseRenewed {
        resource: String,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        renewed_at: LeaseTimestamp,
    },
    LeaseReleased {
        resource: String,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        release: LeaseRelease,
    },
    AcmeHttp01Presented {
        hostname: AcmeHostname,
        token: AcmeChallengeToken,
        epoch: LeaseEpoch,
    },
    AcmeHttp01Cleared {
        hostname: AcmeHostname,
        token: AcmeChallengeToken,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
    },
}

pub fn payload_matches_key(candidate: &FactCandidate, payload: &ProjectionFactPayload) -> bool {
    match (key_expectation(candidate), candidate.kind(), payload) {
        (
            Some(KeyExpectation::NodeJoined { node_id, epoch }),
            FactKind::NodeJoined,
            ProjectionFactPayload::NodeJoined(fact),
        ) => fact.node_id.as_str() == node_id && fact.epoch == epoch,
        (
            Some(KeyExpectation::NodeRemovalStarted { node_id, epoch }),
            FactKind::NodeRemovalStarted,
            ProjectionFactPayload::NodeRemovalStarted(fact),
        ) => fact.node_id.as_str() == node_id && fact.epoch == epoch,
        (
            Some(KeyExpectation::NodeTombstoned { node_id, epoch }),
            FactKind::NodeTombstoned,
            ProjectionFactPayload::NodeTombstoned(fact),
        ) => fact.node_id.as_str() == node_id && fact.epoch == epoch,
        (
            Some(KeyExpectation::PeerAdmitted { node_id, epoch }),
            FactKind::PeerAdmitted,
            ProjectionFactPayload::PeerAdmitted(fact),
        ) => fact.node_id.as_str() == node_id && fact.epoch == epoch,
        (
            Some(KeyExpectation::ServiceRegistered {
                service,
                node_id,
                epoch,
            }),
            FactKind::ServiceRegistered,
            ProjectionFactPayload::ServiceRegistered(fact),
        ) => {
            fact.service.as_str() == service
                && fact.node_id.as_str() == node_id
                && fact.epoch == epoch
        }
        (
            Some(KeyExpectation::RouteCommit { route_commit_id }),
            FactKind::RouteCommit,
            ProjectionFactPayload::RouteCommit(fact),
        ) => fact.route_commit_id == route_commit_id,
        (
            Some(KeyExpectation::ServingCommit { serving_commit_id }),
            FactKind::ServingCommit,
            ProjectionFactPayload::ServingCommit(fact),
        ) => fact.serving_commit_id == serving_commit_id,
        (
            Some(KeyExpectation::GatewayCommit { gateway_commit_id }),
            FactKind::GatewayCommit,
            ProjectionFactPayload::GatewayCommit(fact),
        ) => fact.gateway_commit_id == gateway_commit_id,
        (
            Some(KeyExpectation::DnsCommit { dns_commit_id }),
            FactKind::DnsCommit,
            ProjectionFactPayload::DnsCommit(fact),
        ) => fact.dns_commit_id == dns_commit_id,
        (
            Some(KeyExpectation::LeaseClaimed { resource, epoch }),
            FactKind::LeaseClaimed,
            ProjectionFactPayload::LeaseClaimed(fact),
        ) => fact.resource.as_str() == resource && fact.epoch == epoch,
        (
            Some(KeyExpectation::LeaseRenewed {
                resource,
                epoch,
                claim_hash,
                renewed_at,
            }),
            FactKind::LeaseRenewed,
            ProjectionFactPayload::LeaseRenewed(fact),
        ) => {
            fact.resource.as_str() == resource
                && fact.epoch == epoch
                && fact.claim_hash == claim_hash
                && fact.renewed_at == renewed_at
        }
        (
            Some(KeyExpectation::LeaseReleased {
                resource,
                epoch,
                claim_hash,
                release,
            }),
            FactKind::LeaseReleased,
            ProjectionFactPayload::LeaseReleased(fact),
        ) => {
            fact.resource.as_str() == resource
                && fact.epoch == epoch
                && fact.claim_hash == claim_hash
                && fact.release == release
        }
        (
            Some(KeyExpectation::AcmeHttp01Presented {
                hostname,
                token,
                epoch,
            }),
            FactKind::AcmeHttp01Presented,
            ProjectionFactPayload::AcmeHttp01Presented(fact),
        ) => {
            fact.id().hostname() == &hostname
                && fact.id().token() == &token
                && fact.epoch() == epoch
        }
        (
            Some(KeyExpectation::AcmeHttp01Cleared {
                hostname,
                token,
                epoch,
                claim_hash,
            }),
            FactKind::AcmeHttp01Cleared,
            ProjectionFactPayload::AcmeHttp01Cleared(fact),
        ) => {
            fact.id().hostname() == &hostname
                && fact.id().token() == &token
                && fact.epoch() == epoch
                && fact.claim_hash() == claim_hash
        }
        _ => false,
    }
}

fn key_expectation(candidate: &FactCandidate) -> Option<KeyExpectation> {
    let segments = candidate.key().segments().collect::<Vec<_>>();
    match segments.as_slice() {
        ["facts", "node", node_id, "joined", epoch]
        | ["facts", "node", node_id, "joined", epoch, _] => Some(KeyExpectation::NodeJoined {
            node_id: (*node_id).to_string(),
            epoch: epoch.parse().ok()?,
        }),
        ["facts", "node", node_id, "removal_started", epoch]
        | ["facts", "node", node_id, "removal_started", epoch, _] => {
            Some(KeyExpectation::NodeRemovalStarted {
                node_id: (*node_id).to_string(),
                epoch: epoch.parse().ok()?,
            })
        }
        ["facts", "node", node_id, "tombstoned", epoch]
        | ["facts", "node", node_id, "tombstoned", epoch, _] => {
            Some(KeyExpectation::NodeTombstoned {
                node_id: (*node_id).to_string(),
                epoch: epoch.parse().ok()?,
            })
        }
        ["facts", "peer", node_id, "admitted", epoch]
        | ["facts", "peer", node_id, "admitted", epoch, _] => Some(KeyExpectation::PeerAdmitted {
            node_id: (*node_id).to_string(),
            epoch: epoch.parse().ok()?,
        }),
        ["facts", "service", service, node_id, "registered", epoch]
        | ["facts", "service", service, node_id, "registered", epoch, _] => {
            Some(KeyExpectation::ServiceRegistered {
                service: (*service).to_string(),
                node_id: (*node_id).to_string(),
                epoch: epoch.parse().ok()?,
            })
        }
        ["facts", "routes", route_commit_id] => Some(KeyExpectation::RouteCommit {
            route_commit_id: (*route_commit_id).to_string(),
        }),
        ["facts", "serving", serving_commit_id] => Some(KeyExpectation::ServingCommit {
            serving_commit_id: (*serving_commit_id).to_string(),
        }),
        ["facts", "gateway", gateway_commit_id] => Some(KeyExpectation::GatewayCommit {
            gateway_commit_id: (*gateway_commit_id).to_string(),
        }),
        ["facts", "dns", dns_commit_id] => Some(KeyExpectation::DnsCommit {
            dns_commit_id: (*dns_commit_id).to_string(),
        }),
        ["facts", "lease", resource, "claimed", epoch]
        | ["facts", "lease", resource, "claimed", epoch, _] => Some(KeyExpectation::LeaseClaimed {
            resource: (*resource).to_string(),
            epoch: parse_lease_epoch(epoch)?,
        }),
        [
            "facts",
            "lease",
            resource,
            "renewed",
            epoch,
            claim_hash,
            renewed_at,
        ]
        | [
            "facts",
            "lease",
            resource,
            "renewed",
            epoch,
            claim_hash,
            renewed_at,
            _,
        ] => Some(KeyExpectation::LeaseRenewed {
            resource: (*resource).to_string(),
            epoch: parse_lease_epoch(epoch)?,
            claim_hash: LeaseContentHash::from_hex(claim_hash).ok()?,
            renewed_at: parse_lease_timestamp(renewed_at)?,
        }),
        [
            "facts",
            "lease",
            resource,
            "released",
            epoch,
            claim_hash,
            release,
        ]
        | [
            "facts",
            "lease",
            resource,
            "released",
            epoch,
            claim_hash,
            release,
            _,
        ] => Some(KeyExpectation::LeaseReleased {
            resource: (*resource).to_string(),
            epoch: parse_lease_epoch(epoch)?,
            claim_hash: LeaseContentHash::from_hex(claim_hash).ok()?,
            release: parse_lease_release(release)?,
        }),
        [
            "facts",
            "acme",
            "http01",
            hostname,
            token,
            "presented",
            epoch,
        ]
        | [
            "facts",
            "acme",
            "http01",
            hostname,
            token,
            "presented",
            epoch,
            _,
        ] => Some(KeyExpectation::AcmeHttp01Presented {
            hostname: AcmeHostname::parse(*hostname).ok()?,
            token: AcmeChallengeToken::parse(*token).ok()?,
            epoch: parse_lease_epoch(epoch)?,
        }),
        [
            "facts",
            "acme",
            "http01",
            hostname,
            token,
            "cleared",
            epoch,
            claim_hash,
        ]
        | [
            "facts",
            "acme",
            "http01",
            hostname,
            token,
            "cleared",
            epoch,
            claim_hash,
            _,
        ] => Some(KeyExpectation::AcmeHttp01Cleared {
            hostname: AcmeHostname::parse(*hostname).ok()?,
            token: AcmeChallengeToken::parse(*token).ok()?,
            epoch: parse_lease_epoch(epoch)?,
            claim_hash: LeaseContentHash::from_hex(claim_hash).ok()?,
        }),
        _ => None,
    }
}

fn parse_lease_epoch(value: &str) -> Option<LeaseEpoch> {
    LeaseEpoch::from_u64(value.parse().ok()?).ok()
}

fn parse_lease_timestamp(value: &str) -> Option<LeaseTimestamp> {
    Some(LeaseTimestamp::from_secs(value.parse().ok()?))
}

fn parse_lease_release(value: &str) -> Option<LeaseRelease> {
    if value == "drop" {
        return Some(LeaseRelease::DroppedWithoutTimestamp);
    }
    parse_lease_timestamp(value).map(LeaseRelease::At)
}

pub(super) fn rejection_reason(candidate: &FactCandidate) -> Option<ProjectionIgnoreReason> {
    match candidate.status() {
        CandidateStatus::Verified => None,
        CandidateStatus::Conflict if is_reducible_conflict_kind(candidate.kind()) => None,
        CandidateStatus::Unverified => Some(ProjectionIgnoreReason::Unverified),
        CandidateStatus::Unauthorized => Some(ProjectionIgnoreReason::Unauthorized),
        CandidateStatus::CrossIsland => Some(ProjectionIgnoreReason::CrossIsland),
        CandidateStatus::Conflict => Some(ProjectionIgnoreReason::Conflict),
    }
}
