use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::pin::Pin;

use mvp_bus::{
    BusActorHandle, BusError, BusSession, FactContentHash, FactKey, FactKeyParseError,
    FactKeyPattern, FactPayload, FactWriteOutcome, IslandId,
};
use mvp_projection::{
    BackendEndpoint, CandidateStatus, DnsRecordFact, DnsRecordProjection, FactSource,
    FactSourceError, GatewayRouteProjection, ProjectionFactPayload, ProjectionReport, RouteId,
    ServingCommitFact,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type RoutingResult<T> = Result<T, RoutingError>;

#[derive(Debug, Error)]
pub enum RoutingError {
    #[error("projection catch-up proof is missing")]
    ProjectionCatchUpMissing,
    #[error("projection catch-up proof does not match serving commit {serving_commit_id}")]
    ProjectionCatchUpMismatch { serving_commit_id: ServingCommitId },
    #[error("serving fact already has a conflicting candidate: {key}")]
    ServingFactConflict { key: FactKey },
    #[error("serving fact is missing: {key}")]
    ServingFactMissing { key: FactKey },
    #[error("serving fact payload was not a serving commit: {key}")]
    ServingFactKindMismatch { key: FactKey },
    #[error("serving fact payload does not match serving commit {serving_commit_id}")]
    ServingFactMismatch { serving_commit_id: ServingCommitId },
    #[error("invalid wire payload: {context}: {source}")]
    WirePayload {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    FactSource(#[from] FactSourceError),
    #[error(transparent)]
    FactKeyParse(#[from] FactKeyParseError),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ServingCommitId(String);

impl ServingCommitId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ServingCommitId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RouteCommitId(String);

impl RouteCommitId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for RouteCommitId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GatewayCommitId(String);

impl GatewayCommitId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for GatewayCommitId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DnsCommitId(String);

impl DnsCommitId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for DnsCommitId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServingCommitPlan {
    pub serving_commit_id: ServingCommitId,
    pub route_commit_id: RouteCommitId,
    pub gateway_commit_id: GatewayCommitId,
    pub dns_commit_id: DnsCommitId,
    pub route_id: RouteId,
    pub hostnames: Vec<String>,
    pub active_backends: Vec<BackendEndpoint>,
    pub old_backends_to_drain: Vec<BackendEndpoint>,
    pub dns_records: Vec<DnsRecordFact>,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionCatchUp {
    serving_commit_id: ServingCommitId,
}

impl ProjectionCatchUp {
    pub fn from_report(
        commit: &ServingCommitPlan,
        report: &ProjectionReport,
    ) -> RoutingResult<Self> {
        let gateway = report
            .state
            .gateway
            .as_ref()
            .ok_or(RoutingError::ProjectionCatchUpMissing)?;
        let dns = report
            .state
            .dns
            .as_ref()
            .ok_or(RoutingError::ProjectionCatchUpMissing)?;
        if report.gateway_snapshot.is_none() || report.dns_snapshot.is_none() {
            return Err(RoutingError::ProjectionCatchUpMissing);
        }
        if !gateway_snapshot_revision_matches(report, commit)
            || dns_snapshot_revision(report) != Some(expected_dns_revision(commit).as_str())
            || gateway.gateway_commit_id != commit.gateway_commit_id.to_string()
            || gateway.route_commit_id != commit.route_commit_id.to_string()
            || gateway.routes != expected_gateway_routes(commit)
            || dns.dns_commit_id != commit.dns_commit_id.to_string()
            || dns.records != expected_dns_records(commit)
        {
            return Err(RoutingError::ProjectionCatchUpMismatch {
                serving_commit_id: commit.serving_commit_id.clone(),
            });
        }
        Ok(Self {
            serving_commit_id: commit.serving_commit_id.clone(),
        })
    }

    #[must_use]
    pub fn serving_commit_id(&self) -> &ServingCommitId {
        &self.serving_commit_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingCommitFacts {
    pub serving: FactWriteOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenServingFact {
    key: FactKey,
    content_hash: FactContentHash,
    status: ServingFactWriteStatus,
}

impl WrittenServingFact {
    #[must_use]
    pub fn inserted(key: FactKey, content_hash: FactContentHash) -> Self {
        Self {
            key,
            content_hash,
            status: ServingFactWriteStatus::Inserted,
        }
    }

    #[must_use]
    pub fn already_present(key: FactKey, content_hash: FactContentHash) -> Self {
        Self {
            key,
            content_hash,
            status: ServingFactWriteStatus::AlreadyPresent,
        }
    }

    #[must_use]
    pub fn key(&self) -> &FactKey {
        &self.key
    }

    #[must_use]
    pub fn content_hash(&self) -> &FactContentHash {
        &self.content_hash
    }

    #[must_use]
    pub fn status(&self) -> ServingFactWriteStatus {
        self.status
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServingFactWriteStatus {
    Inserted,
    AlreadyPresent,
}

pub trait ServingFactWriter: Send + Sync {
    fn write_serving_commit<'a>(
        &'a self,
        commit: &'a ServingCommitPlan,
    ) -> Pin<Box<dyn Future<Output = RoutingResult<WrittenServingFact>> + Send + 'a>>;
}

#[derive(Clone)]
pub struct BusServingFactWriter {
    bus: BusActorHandle,
    session: BusSession,
}

impl BusServingFactWriter {
    #[must_use]
    pub fn new(bus: BusActorHandle, session: BusSession) -> Self {
        Self { bus, session }
    }
}

impl ServingFactWriter for BusServingFactWriter {
    fn write_serving_commit<'a>(
        &'a self,
        commit: &'a ServingCommitPlan,
    ) -> Pin<Box<dyn Future<Output = RoutingResult<WrittenServingFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = serving_commit_fact_key(&commit.serving_commit_id)?;
            let facts = write_serving_commit(&self.bus, &self.session, commit).await?;
            match facts.serving {
                FactWriteOutcome::Inserted(fact) => Ok(WrittenServingFact::inserted(
                    key,
                    fact.content_hash().clone(),
                )),
                FactWriteOutcome::AlreadyPresent(fact) => Ok(WrittenServingFact::already_present(
                    key,
                    fact.content_hash().clone(),
                )),
                FactWriteOutcome::Conflict(_) => Err(RoutingError::ServingFactConflict { key }),
            }
        })
    }
}

pub async fn write_serving_commit(
    bus: &BusActorHandle,
    session: &BusSession,
    commit: &ServingCommitPlan,
) -> RoutingResult<ServingCommitFacts> {
    let key = serving_commit_fact_key(&commit.serving_commit_id)?;
    let payload = serving_commit_fact_payload(commit)?;
    let serving = write_projection_fact(bus, session, key, payload).await?;
    Ok(ServingCommitFacts { serving })
}

pub fn serving_commit_fact_key(serving_commit_id: &ServingCommitId) -> RoutingResult<FactKey> {
    FactKey::parse(format!("/facts/serving/{serving_commit_id}")).map_err(RoutingError::from)
}

pub fn serving_commit_fact_body(commit: &ServingCommitPlan) -> ServingCommitFact {
    ServingCommitFact {
        serving_commit_id: commit.serving_commit_id.to_string(),
        route_commit_id: commit.route_commit_id.to_string(),
        gateway_commit_id: commit.gateway_commit_id.to_string(),
        dns_commit_id: commit.dns_commit_id.to_string(),
        route_id: commit.route_id.clone(),
        hostnames: commit.hostnames.clone(),
        backends: commit.active_backends.clone(),
        old_backends_to_drain: commit.old_backends_to_drain.clone(),
        dns_records: commit.dns_records.clone(),
        epoch: commit.epoch,
    }
}

pub fn serving_commit_fact_payload(commit: &ServingCommitPlan) -> RoutingResult<FactPayload> {
    ProjectionFactPayload::ServingCommit(serving_commit_fact_body(commit))
        .to_fact_bytes()
        .map(Into::into)
        .map_err(|source| RoutingError::WirePayload {
            context: "serving commit fact",
            source,
        })
}

pub fn decode_serving_commit_fact_payload(
    key: &FactKey,
    payload: &FactPayload,
) -> RoutingResult<ServingCommitFact> {
    match ProjectionFactPayload::from_fact_bytes(payload.as_bytes()).map_err(|source| {
        RoutingError::WirePayload {
            context: "decode serving commit fact",
            source,
        }
    })? {
        ProjectionFactPayload::ServingCommit(fact) => Ok(fact),
        _ => Err(RoutingError::ServingFactKindMismatch { key: key.clone() }),
    }
}

pub fn validate_serving_commit_fact(
    expected: &ServingCommitPlan,
    fact: &ServingCommitFact,
) -> RoutingResult<()> {
    if *fact == serving_commit_fact_body(expected) {
        Ok(())
    } else {
        Err(RoutingError::ServingFactMismatch {
            serving_commit_id: expected.serving_commit_id.clone(),
        })
    }
}

pub fn read_exact_serving_commit(
    source: &dyn FactSource,
    island: &IslandId,
    session: &BusSession,
    expected: &ServingCommitPlan,
) -> RoutingResult<ServingCommitFact> {
    let key = serving_commit_fact_key(&expected.serving_commit_id)?;
    let expected_body = serving_commit_fact_body(expected);
    let pattern = FactKeyPattern::parse(key.as_str())?;
    let candidates = source
        .list_candidates(island, &pattern, session)?
        .into_iter()
        .filter(|candidate| {
            candidate.key() == &key
                && matches!(
                    candidate.status(),
                    CandidateStatus::Verified | CandidateStatus::Conflict
                )
        })
        .collect::<Vec<_>>();
    let payloads = source.read_payloads(island, &candidates, session)?;
    for candidate in &candidates {
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        let fact = decode_serving_commit_fact_payload(&key, payload)?;
        if fact == expected_body {
            return Ok(fact);
        }
    }
    if candidates.is_empty() {
        Err(RoutingError::ServingFactMissing { key })
    } else {
        Err(RoutingError::ServingFactMismatch {
            serving_commit_id: expected.serving_commit_id.clone(),
        })
    }
}

async fn write_projection_fact(
    bus: &BusActorHandle,
    session: &BusSession,
    key: FactKey,
    payload: FactPayload,
) -> RoutingResult<FactWriteOutcome> {
    let outcome = bus
        .write_fact_payload(session, key.clone(), payload)
        .await?;
    match outcome {
        FactWriteOutcome::Inserted(_) | FactWriteOutcome::AlreadyPresent(_) => Ok(outcome),
        FactWriteOutcome::Conflict(_) => Err(RoutingError::ServingFactConflict { key }),
    }
}

fn expected_gateway_revision_prefix(commit: &ServingCommitPlan) -> String {
    format!(
        "gateway:{}:{}",
        commit.gateway_commit_id, commit.route_commit_id
    )
}

fn expected_dns_revision(commit: &ServingCommitPlan) -> String {
    format!("dns:{}", commit.dns_commit_id)
}

fn gateway_snapshot_revision(report: &ProjectionReport) -> Option<&str> {
    report
        .gateway_snapshot
        .as_ref()
        .map(|snapshot| snapshot.revision.as_str())
}

fn gateway_snapshot_revision_matches(
    report: &ProjectionReport,
    commit: &ServingCommitPlan,
) -> bool {
    let Some(revision) = gateway_snapshot_revision(report) else {
        return false;
    };
    let expected = expected_gateway_revision_prefix(commit);
    revision == expected
        || revision
            .strip_prefix(&expected)
            .is_some_and(|suffix| suffix.starts_with(':'))
}

fn dns_snapshot_revision(report: &ProjectionReport) -> Option<&str> {
    report
        .dns_snapshot
        .as_ref()
        .map(|snapshot| snapshot.revision.as_str())
}

fn expected_gateway_routes(commit: &ServingCommitPlan) -> Vec<GatewayRouteProjection> {
    let mut hostnames = commit.hostnames.clone();
    hostnames.sort();
    let mut backends = commit.active_backends.clone();
    backends.sort();
    let mut old_backends_to_drain = commit.old_backends_to_drain.clone();
    old_backends_to_drain.sort();
    vec![GatewayRouteProjection {
        route_id: commit.route_id.clone(),
        hostnames,
        backends,
        old_backends_to_drain,
    }]
}

fn expected_dns_records(commit: &ServingCommitPlan) -> Vec<DnsRecordProjection> {
    let mut records = commit
        .dns_records
        .clone()
        .into_iter()
        .map(DnsRecordProjection::from)
        .collect::<Vec<_>>();
    records.sort();
    records
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use mvp_bus::{FactContentHash, FactKeyPattern, Grant, IslandId, PrincipalId};
    use mvp_identity::NodeId;
    use mvp_projection::{
        BackendEndpoint, BusFactSource, DnsProjection, DnsRecordFact, GatewayProjection,
        ProjectionState, SnapshotWriteReport,
    };

    use super::*;

    fn serving_commit() -> ServingCommitPlan {
        ServingCommitPlan {
            serving_commit_id: ServingCommitId::new("serving-commit-1"),
            route_commit_id: RouteCommitId::new("route-commit-1"),
            gateway_commit_id: GatewayCommitId::new("gateway-commit-1"),
            dns_commit_id: DnsCommitId::new("dns-commit-1"),
            route_id: RouteId::new("web"),
            hostnames: vec!["app.example.test".to_string()],
            active_backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-new"),
                address: "fd00::2:8080".to_string(),
            }],
            old_backends_to_drain: vec![BackendEndpoint {
                node_id: NodeId::new("node-old"),
                address: "fd00::1:8080".to_string(),
            }],
            dns_records: vec![DnsRecordFact {
                name: "app.example.test".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::2".to_string(),
                ttl_seconds: 30,
            }],
            epoch: 1,
        }
    }

    #[test]
    fn projection_catch_up_allows_unrelated_gateway_revision_suffix() {
        let commit = serving_commit();
        let report = projection_report_for_commit(
            &commit,
            "gateway:gateway-commit-1:route-commit-1:acme:none",
            "dns:dns-commit-1",
        );

        let proof = ProjectionCatchUp::from_report(&commit, &report).expect("catch-up proof");

        assert_eq!(proof.serving_commit_id(), &commit.serving_commit_id);
    }

    #[test]
    fn serving_commit_fact_helpers_round_trip() {
        let commit = serving_commit();
        let key = serving_commit_fact_key(&commit.serving_commit_id).expect("key");
        let payload = serving_commit_fact_payload(&commit).expect("payload");
        let decoded = decode_serving_commit_fact_payload(&key, &payload).expect("decode");

        assert_eq!(key.as_str(), "/facts/serving/serving-commit-1");
        assert_eq!(decoded, serving_commit_fact_body(&commit));
    }

    #[test]
    fn exact_serving_commit_read_refuses_mismatched_epoch() {
        let (bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
        let source = BusFactSource::new(bus.clone());
        let session = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("deploy"),
            Grant::empty()
                .with_fact_write(FactKeyPattern::parse("/facts/serving/>").expect("pattern"))
                .with_fact_read(FactKeyPattern::parse("/facts/serving/>").expect("pattern")),
        );
        let expected = serving_commit();
        let mut stored = expected.clone();
        stored.epoch = 2;
        bus.write_fact_payload(
            &session,
            serving_commit_fact_key(&expected.serving_commit_id).expect("key"),
            serving_commit_fact_payload(&stored).expect("payload"),
        )
        .expect("write stored serving fact");

        let error = read_exact_serving_commit(&source, session.island(), &session, &expected)
            .expect_err("mismatched serving fact should be rejected");

        assert!(matches!(error, RoutingError::ServingFactMismatch { .. }));
    }

    #[test]
    fn exact_serving_commit_read_uses_matching_conflict_candidate() {
        let (bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
        let source = BusFactSource::new(bus.clone());
        let writer_a = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("deploy-a"),
            Grant::empty()
                .with_fact_write(FactKeyPattern::parse("/facts/serving/>").expect("pattern"))
                .with_fact_read(FactKeyPattern::parse("/facts/serving/>").expect("pattern")),
        );
        let writer_b = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("deploy-b"),
            Grant::empty()
                .with_fact_write(FactKeyPattern::parse("/facts/serving/>").expect("pattern"))
                .with_fact_read(FactKeyPattern::parse("/facts/serving/>").expect("pattern")),
        );
        let expected = serving_commit();
        let key = serving_commit_fact_key(&expected.serving_commit_id).expect("key");
        let mut conflict = expected.clone();
        conflict.hostnames = vec!["other.example.test".to_string()];
        bus.write_fact_payload(
            &writer_a,
            key.clone(),
            serving_commit_fact_payload(&conflict).expect("payload"),
        )
        .expect("write conflict");
        bus.write_fact_payload(
            &writer_b,
            key,
            serving_commit_fact_payload(&expected).expect("payload"),
        )
        .expect("write matching candidate");

        let fact = read_exact_serving_commit(&source, writer_a.island(), &writer_a, &expected)
            .expect("matching conflict candidate is accepted");

        assert_eq!(
            FactContentHash::for_payload(&serving_commit_fact_payload(&expected).expect("payload")),
            FactContentHash::for_payload(
                &ProjectionFactPayload::ServingCommit(fact)
                    .to_fact_bytes()
                    .expect("payload")
            )
        );
    }

    fn projection_report_for_commit(
        commit: &ServingCommitPlan,
        gateway_revision: &str,
        dns_revision: &str,
    ) -> ProjectionReport {
        let mut state = ProjectionState::for_island(mvp_bus::IslandId::new("prod"));
        state.gateway = Some(GatewayProjection {
            gateway_commit_id: commit.gateway_commit_id.to_string(),
            route_commit_id: commit.route_commit_id.to_string(),
            routes: expected_gateway_routes(commit),
        });
        state.dns = Some(DnsProjection {
            dns_commit_id: commit.dns_commit_id.to_string(),
            records: expected_dns_records(commit),
        });
        ProjectionReport {
            state,
            sqlite_path: PathBuf::from("projections.sqlite"),
            gateway_snapshot: Some(SnapshotWriteReport {
                path: PathBuf::from("gateway.snapshot"),
                bytes_written: 1,
                revision: gateway_revision.to_string(),
            }),
            dns_snapshot: Some(SnapshotWriteReport {
                path: PathBuf::from("dns.snapshot"),
                bytes_written: 1,
                revision: dns_revision.to_string(),
            }),
            duration: Duration::from_millis(1),
        }
    }
}
