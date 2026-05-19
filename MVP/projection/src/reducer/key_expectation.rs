use mvp_acme::{AcmeChallengeToken, AcmeHostname};
use mvp_lease::{LeaseContentHash, LeaseEpoch, LeaseRelease, LeaseTimestamp};

use crate::facts::ProjectionFactPayload;
use crate::model::ProjectionIgnoreReason;
use crate::source::{
    CandidateStatus, FactCandidate, FactKind, ParsedFactKey, is_reducible_conflict_kind,
    parse_fact_key,
};

pub fn payload_matches_key(candidate: &FactCandidate, payload: &ProjectionFactPayload) -> bool {
    match (parse_fact_key(candidate.key()), candidate.kind(), payload) {
        (
            ParsedFactKey::NodeJoined { node_id, epoch },
            FactKind::NodeJoined,
            ProjectionFactPayload::NodeJoined(fact),
        ) => fact.node_id.as_str() == node_id && epoch.parse::<u64>().ok() == Some(fact.epoch),
        (
            ParsedFactKey::NodeRemovalStarted { node_id, epoch },
            FactKind::NodeRemovalStarted,
            ProjectionFactPayload::NodeRemovalStarted(fact),
        ) => fact.node_id.as_str() == node_id && epoch.parse::<u64>().ok() == Some(fact.epoch),
        (
            ParsedFactKey::NodeTombstoned { node_id, epoch },
            FactKind::NodeTombstoned,
            ProjectionFactPayload::NodeTombstoned(fact),
        ) => fact.node_id.as_str() == node_id && epoch.parse::<u64>().ok() == Some(fact.epoch),
        (
            ParsedFactKey::PeerAdmitted { node_id, epoch },
            FactKind::PeerAdmitted,
            ProjectionFactPayload::PeerAdmitted(fact),
        ) => fact.node_id.as_str() == node_id && epoch.parse::<u64>().ok() == Some(fact.epoch),
        (
            ParsedFactKey::ServiceRegistered {
                service,
                node_id,
                epoch,
            },
            FactKind::ServiceRegistered,
            ProjectionFactPayload::ServiceRegistered(fact),
        ) => {
            fact.service.as_str() == service
                && fact.node_id.as_str() == node_id
                && epoch.parse::<u64>().ok() == Some(fact.epoch)
        }
        (
            ParsedFactKey::RouteCommit { route_commit_id },
            FactKind::RouteCommit,
            ProjectionFactPayload::RouteCommit(fact),
        ) => fact.route_commit_id == route_commit_id,
        (
            ParsedFactKey::ServingCommit { serving_commit_id },
            FactKind::ServingCommit,
            ProjectionFactPayload::ServingCommit(fact),
        ) => fact.serving_commit_id == serving_commit_id,
        (
            ParsedFactKey::GatewayCommit { gateway_commit_id },
            FactKind::GatewayCommit,
            ProjectionFactPayload::GatewayCommit(fact),
        ) => fact.gateway_commit_id == gateway_commit_id,
        (
            ParsedFactKey::DnsCommit { dns_commit_id },
            FactKind::DnsCommit,
            ProjectionFactPayload::DnsCommit(fact),
        ) => fact.dns_commit_id == dns_commit_id,
        (
            ParsedFactKey::LeaseClaimed { resource, epoch },
            FactKind::LeaseClaimed,
            ProjectionFactPayload::LeaseClaimed(fact),
        ) => fact.resource.as_str() == resource && parse_lease_epoch(&epoch) == Some(fact.epoch),
        (
            ParsedFactKey::LeaseRenewed {
                resource,
                epoch,
                claim_hash,
                renewed_at,
            },
            FactKind::LeaseRenewed,
            ProjectionFactPayload::LeaseRenewed(fact),
        ) => {
            fact.resource.as_str() == resource
                && parse_lease_epoch(&epoch) == Some(fact.epoch)
                && LeaseContentHash::from_hex(&claim_hash).ok() == Some(fact.claim_hash.clone())
                && parse_lease_timestamp(&renewed_at) == Some(fact.renewed_at)
        }
        (
            ParsedFactKey::LeaseReleased {
                resource,
                epoch,
                claim_hash,
                release,
            },
            FactKind::LeaseReleased,
            ProjectionFactPayload::LeaseReleased(fact),
        ) => {
            fact.resource.as_str() == resource
                && parse_lease_epoch(&epoch) == Some(fact.epoch)
                && LeaseContentHash::from_hex(&claim_hash).ok() == Some(fact.claim_hash.clone())
                && parse_lease_release(&release) == Some(fact.release)
        }
        (
            ParsedFactKey::AcmeHttp01Presented {
                hostname,
                token,
                epoch,
            },
            FactKind::AcmeHttp01Presented,
            ProjectionFactPayload::AcmeHttp01Presented(fact),
        ) => {
            AcmeHostname::parse(&hostname).ok().as_ref() == Some(fact.id().hostname())
                && AcmeChallengeToken::parse(&token).ok().as_ref() == Some(fact.id().token())
                && parse_lease_epoch(&epoch) == Some(fact.epoch())
        }
        (
            ParsedFactKey::AcmeHttp01Cleared {
                hostname,
                token,
                epoch,
                claim_hash,
            },
            FactKind::AcmeHttp01Cleared,
            ProjectionFactPayload::AcmeHttp01Cleared(fact),
        ) => {
            AcmeHostname::parse(&hostname).ok().as_ref() == Some(fact.id().hostname())
                && AcmeChallengeToken::parse(&token).ok().as_ref() == Some(fact.id().token())
                && parse_lease_epoch(&epoch) == Some(fact.epoch())
                && LeaseContentHash::from_hex(&claim_hash).ok() == Some(fact.claim_hash())
        }
        (
            ParsedFactKey::AcmeCertificateActivated {
                hostname,
                issued_at,
            },
            FactKind::AcmeCertificateActivated,
            ProjectionFactPayload::AcmeCertificateActivated(fact),
        ) => {
            AcmeHostname::parse(&hostname).ok().as_ref() == Some(&fact.hostname)
                && issued_at.parse::<u64>().ok() == Some(fact.issued_at_secs)
        }
        _ => false,
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
