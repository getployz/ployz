use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use mvp_acme::{AcmeChallengeToken, AcmeHostname, AcmeKeyAuthorization};
use mvp_bus::IslandId;
use mvp_projection::{
    AcmeHttp01ChallengeKey, AcmeHttp01ChallengeProjection, DnsRecordProjection, DnsSnapshotFile,
    GatewayRouteProjection, GatewaySnapshotFile, ProjectionError, load_dns_snapshot,
    load_gateway_snapshot,
};
use serde::{Deserialize, Serialize};

use crate::{ServingError, ServingResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingSnapshotPaths {
    pub gateway: PathBuf,
    pub dns: PathBuf,
}

impl ServingSnapshotPaths {
    #[must_use]
    pub fn new(gateway: impl Into<PathBuf>, dns: impl Into<PathBuf>) -> Self {
        Self {
            gateway: gateway.into(),
            dns: dns.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingSnapshotBatch {
    pub gateway: GatewaySnapshotFile,
    pub dns: DnsSnapshotFile,
    route_by_hostname: BTreeMap<String, usize>,
    records_by_name_type: BTreeMap<DnsRecordKey, Vec<usize>>,
    acme_http01_by_host_token: BTreeMap<AcmeHttp01ChallengeKey, usize>,
}

impl ServingSnapshotBatch {
    pub fn load(paths: &ServingSnapshotPaths, expected_island: &IslandId) -> ServingResult<Self> {
        let gateway = load_gateway(paths.gateway.as_path(), expected_island)?;
        let dns = load_dns(paths.dns.as_path(), expected_island)?;
        Ok(Self::from_snapshots(gateway, dns))
    }

    #[must_use]
    pub fn revisions(&self) -> ServingRevisions {
        ServingRevisions {
            gateway: self.gateway.revision.clone(),
            dns: self.dns.revision.clone(),
        }
    }

    #[must_use]
    pub fn route_for_host(&self, host: &str) -> Option<GatewayRouteProjection> {
        self.route_by_hostname
            .get(&normalize_lookup_part(host))
            .and_then(|route_index| self.gateway.routes.get(*route_index))
            .cloned()
    }

    #[must_use]
    pub fn dns_records(&self, name: &str, record_type: &str) -> Vec<DnsRecordProjection> {
        let key = DnsRecordKey::new(name, record_type);
        let Some(record_indexes) = self.records_by_name_type.get(&key) else {
            return Vec::new();
        };
        record_indexes
            .iter()
            .filter_map(|record_index| self.dns.records.get(*record_index))
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn acme_http01_challenge(&self, host: &str, token: &str) -> Option<AcmeKeyAuthorization> {
        let key = acme_http01_lookup_key(host, token)?;
        self.acme_http01_by_host_token
            .get(&key)
            .and_then(|challenge_index| self.gateway.acme_http01.get(*challenge_index))
            .map(|challenge| challenge.key_authorization.clone())
    }

    #[must_use]
    pub fn acme_http01_challenge_count(&self) -> usize {
        self.gateway.acme_http01.len()
    }

    fn from_snapshots(gateway: GatewaySnapshotFile, dns: DnsSnapshotFile) -> Self {
        let route_by_hostname = index_routes(&gateway.routes);
        let records_by_name_type = index_dns_records(&dns.records);
        let acme_http01_by_host_token = index_acme_http01(&gateway.acme_http01);
        Self {
            gateway,
            dns,
            route_by_hostname,
            records_by_name_type,
            acme_http01_by_host_token,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DnsRecordKey {
    name: String,
    record_type: String,
}

impl DnsRecordKey {
    fn new(name: &str, record_type: &str) -> Self {
        Self {
            name: normalize_lookup_part(name),
            record_type: normalize_lookup_part(record_type),
        }
    }
}

fn acme_http01_lookup_key(host: &str, token: &str) -> Option<AcmeHttp01ChallengeKey> {
    Some(AcmeHttp01ChallengeKey::new(
        AcmeHostname::parse_host_header(host).ok()?,
        AcmeChallengeToken::parse(token).ok()?,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServingRevisions {
    pub gateway: String,
    pub dns: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServingSnapshotKind {
    Gateway,
    Dns,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ServingFreshness {
    Fresh,
    ServingAgedSnapshot,
    ServingLastGoodAfterFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ServingFailureKind {
    MissingSnapshot,
    SnapshotPath,
    InvalidSnapshotJson,
    Io,
    ProjectionSnapshotLoad,
    ActorUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServingFailure {
    pub kind: ServingFailureKind,
    pub snapshot: Option<ServingSnapshotKind>,
    pub message: String,
}

impl ServingFailure {
    #[must_use]
    pub fn from_projection(snapshot: ServingSnapshotKind, error: &ProjectionError) -> Self {
        Self {
            kind: failure_kind(error),
            snapshot: Some(snapshot),
            message: error.to_string(),
        }
    }

    #[must_use]
    pub fn actor(operation: &'static str, message: String) -> Self {
        Self {
            kind: ServingFailureKind::ActorUnavailable,
            snapshot: None,
            message: format!("{operation}: {message}"),
        }
    }
}

impl Display for ServingFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.snapshot {
            Some(snapshot) => write!(f, "{snapshot:?} {:?}: {}", self.kind, self.message),
            None => write!(f, "{:?}: {}", self.kind, self.message),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingStatus {
    pub loaded_revisions: ServingRevisions,
    pub loaded_at: SystemTime,
    pub snapshot_age: Duration,
    pub freshness: ServingFreshness,
    pub acme_http01_challenge_count: usize,
    pub reload_attempts: u64,
    pub last_reload_attempt_at: Option<SystemTime>,
    pub last_reload_success_at: Option<SystemTime>,
    pub last_failure: Option<ServingFailure>,
}

fn load_gateway(path: &Path, expected_island: &IslandId) -> ServingResult<GatewaySnapshotFile> {
    load_gateway_snapshot(path, expected_island)
        .map_err(|error| snapshot_error(ServingSnapshotKind::Gateway, error))
}

fn load_dns(path: &Path, expected_island: &IslandId) -> ServingResult<DnsSnapshotFile> {
    load_dns_snapshot(path, expected_island)
        .map_err(|error| snapshot_error(ServingSnapshotKind::Dns, error))
}

fn index_routes(routes: &[GatewayRouteProjection]) -> BTreeMap<String, usize> {
    let mut index = BTreeMap::new();
    for (route_index, route) in routes.iter().enumerate() {
        for hostname in &route.hostnames {
            index
                .entry(normalize_lookup_part(hostname))
                .or_insert(route_index);
        }
    }
    index
}

fn index_dns_records(records: &[DnsRecordProjection]) -> BTreeMap<DnsRecordKey, Vec<usize>> {
    let mut index: BTreeMap<DnsRecordKey, Vec<usize>> = BTreeMap::new();
    for (record_index, record) in records.iter().enumerate() {
        index
            .entry(DnsRecordKey::new(&record.name, &record.record_type))
            .or_default()
            .push(record_index);
    }
    index
}

fn index_acme_http01(
    challenges: &[AcmeHttp01ChallengeProjection],
) -> BTreeMap<AcmeHttp01ChallengeKey, usize> {
    let mut index = BTreeMap::new();
    for (challenge_index, challenge) in challenges.iter().enumerate() {
        index.insert(
            AcmeHttp01ChallengeKey::new(challenge.hostname.clone(), challenge.token.clone()),
            challenge_index,
        );
    }
    index
}

fn normalize_lookup_part(value: &str) -> String {
    value.to_ascii_lowercase()
}

fn snapshot_error(snapshot: ServingSnapshotKind, error: ProjectionError) -> ServingError {
    ServingError::SnapshotLoad {
        failure: ServingFailure::from_projection(snapshot, &error),
    }
}

fn failure_kind(error: &ProjectionError) -> ServingFailureKind {
    match error {
        ProjectionError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ServingFailureKind::MissingSnapshot
        }
        ProjectionError::Io(_) => ServingFailureKind::Io,
        ProjectionError::Json(_) => ServingFailureKind::InvalidSnapshotJson,
        ProjectionError::SnapshotPath { .. } => ServingFailureKind::SnapshotPath,
        ProjectionError::FactSource(_)
        | ProjectionError::Sqlite(_)
        | ProjectionError::ProjectionStore { .. }
        | ProjectionError::Timeout { .. }
        | ProjectionError::WorkerJoin(_) => ServingFailureKind::ProjectionSnapshotLoad,
    }
}
