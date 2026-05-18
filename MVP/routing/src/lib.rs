use std::fmt::{self, Display, Formatter};

use mvp_bus::{BusActorHandle, BusError, BusSession, FactKey, FactKeyParseError, FactWriteOutcome};
use mvp_projection::{
    BackendEndpoint, DnsRecordFact, DnsRecordProjection, GatewayRouteProjection,
    ProjectionFactPayload, ProjectionReport, RouteId, ServingCommitFact,
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
    #[error("invalid wire payload: {context}: {source}")]
    WirePayload {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Bus(#[from] BusError),
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

pub async fn write_serving_commit(
    bus: &BusActorHandle,
    session: &BusSession,
    commit: &ServingCommitPlan,
) -> RoutingResult<ServingCommitFacts> {
    let serving = write_projection_fact(
        bus,
        session,
        &format!("/facts/serving/{}", commit.serving_commit_id),
        ProjectionFactPayload::ServingCommit(ServingCommitFact {
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
        }),
    )
    .await?;
    Ok(ServingCommitFacts { serving })
}

async fn write_projection_fact(
    bus: &BusActorHandle,
    session: &BusSession,
    key: &str,
    payload: ProjectionFactPayload,
) -> RoutingResult<FactWriteOutcome> {
    let key = FactKey::parse(key)?;
    let payload = encode_projection_payload(&payload, "projection fact")?;
    let outcome = bus
        .write_fact_payload(session, key.clone(), payload)
        .await?;
    match outcome {
        FactWriteOutcome::Inserted(_) | FactWriteOutcome::AlreadyPresent(_) => Ok(outcome),
        FactWriteOutcome::Conflict(_) => Err(RoutingError::ServingFactConflict { key }),
    }
}

fn encode_projection_payload(
    value: &ProjectionFactPayload,
    context: &'static str,
) -> RoutingResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|source| RoutingError::WirePayload { context, source })
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

    use mvp_identity::NodeId;
    use mvp_projection::{
        BackendEndpoint, DnsProjection, DnsRecordFact, GatewayProjection, ProjectionState,
        SnapshotWriteReport,
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
