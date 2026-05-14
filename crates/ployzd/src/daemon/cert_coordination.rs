use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use ployz_api::{AcmeHttp01ChallengeStatus, AcmeHttp01StatusPayload, DaemonPayload};
use ployz_cert_acme_api::{
    AccountAcquisition, AcmeAccountCoordinator, Http01ChallengeReadiness, IssuanceAcquisition,
    IssuanceCoordinator, IssuanceHold,
};
use ployz_error::{CertificateError, Error, Result};
use ployz_model::{
    MachineId, MachineLifecycle, MachineMembership, RoutingState, ServiceReleaseRecord,
    ServiceRevisionRecord,
};
use ployz_nats::{Lease, LockAcquireError, NatsLocks};
use ployz_nats::{acme_account_lock, cert_lock};
use ployz_orchestrator::certificates::HTTP01_CHALLENGE_VISIBILITY_TIMEOUT;
use ployz_orchestrator::coordination::ReservationId;
use ployz_spec::{RouteSpec, ServiceSpec};
use ployz_store_api::{CertificateStore, RoutingStateStore, StoreDriver};
use ployz_time::now_unix_secs;
use tokio::time::{Instant, sleep};
use tracing::warn;

use crate::daemon::DaemonState;

const DEFAULT_ISSUANCE_TTL_SECS: u64 = 5 * 60;

#[derive(Clone)]
pub struct NatsIssuanceCoordinator {
    locks: NatsLocks,
    owner: MachineId,
    ttl: Duration,
}

impl NatsIssuanceCoordinator {
    #[must_use]
    pub fn new(locks: NatsLocks, owner: MachineId) -> Self {
        Self {
            locks,
            owner,
            ttl: Duration::from_secs(DEFAULT_ISSUANCE_TTL_SECS),
        }
    }

    async fn acquire_key(&self, key: String) -> std::result::Result<Lease, LockAcquireError> {
        self.locks
            .acquire(
                &key,
                self.owner.as_str().to_string(),
                ReservationId::random().0,
                self.ttl,
                now_unix_secs().saturating_add(self.ttl.as_secs()),
            )
            .await
    }

    fn hold_for(&self, lease: Lease) -> IssuanceHold {
        let locks = self.locks.clone();
        IssuanceHold::new(move || async move {
            if let Err(error) = locks.release(lease).await {
                warn!(%error, "failed to release NATS ACME coordination lock");
            }
        })
    }
}

#[async_trait]
impl IssuanceCoordinator for NatsIssuanceCoordinator {
    async fn try_acquire(&self, hostname: &str) -> IssuanceAcquisition {
        match self.acquire_key(cert_lock(hostname)).await {
            Ok(lease) => IssuanceAcquisition::Allowed(self.hold_for(lease)),
            Err(
                error
                @ (LockAcquireError::AlreadyHeld { .. } | LockAcquireError::Contention { .. }),
            ) => IssuanceAcquisition::VetoedByPeer(error.to_string()),
            Err(error) => IssuanceAcquisition::CoordinationFailed(error.to_string()),
        }
    }
}

#[async_trait]
impl AcmeAccountCoordinator for NatsIssuanceCoordinator {
    async fn try_acquire_account(&self, issuer_url: &str) -> AccountAcquisition {
        match self.acquire_key(acme_account_lock(issuer_url)).await {
            Ok(lease) => AccountAcquisition::Allowed(self.hold_for(lease)),
            Err(
                error
                @ (LockAcquireError::AlreadyHeld { .. } | LockAcquireError::Contention { .. }),
            ) => AccountAcquisition::VetoedByPeer(error.to_string()),
            Err(error) => AccountAcquisition::CoordinationFailed(error.to_string()),
        }
    }
}

pub struct NatsChallengeReadiness {
    store: StoreDriver,
}

impl NatsChallengeReadiness {
    #[must_use]
    pub fn new(store: StoreDriver) -> Self {
        Self { store }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChallengeEligibility {
    eligible: BTreeSet<MachineId>,
    excluded: Vec<ChallengeReadinessExclusion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChallengeReadinessExclusion {
    machine_id: MachineId,
    reason: &'static str,
}

#[async_trait]
impl Http01ChallengeReadiness for NatsChallengeReadiness {
    async fn wait_ready(&self, store: &StoreDriver, hostname: &str, token: &str) -> Result<()> {
        wait_for_local_challenge(store, hostname, token).await?;

        let routing = self.store.load_routing_state().await.map_err(|error| {
            Error::operation(
                "acme_challenge_visibility",
                format!("eligibility_unknown: could not load routing state: {error}"),
            )
        })?;
        let eligibility = challenge_eligibility(&routing, hostname)?;
        if eligibility.eligible.is_empty() {
            return Err(CertificateError::Http01NoEligibleGateway {
                hostname: hostname.to_string(),
                excluded: exclusion_codes(&eligibility.excluded),
            }
            .into());
        }

        let deadline = Instant::now() + HTTP01_CHALLENGE_VISIBILITY_TIMEOUT;
        loop {
            let records = self
                .store
                .list_acme_challenge_readiness(hostname, token)
                .await
                .map_err(|error| {
                    Error::operation(
                        "acme_challenge_visibility",
                        format!(
                            "eligibility_unknown: could not list readiness observations: {error}"
                        ),
                    )
                })?;
            let observed = records
                .into_iter()
                .map(|record| record.machine_id)
                .collect::<BTreeSet<_>>();
            let missing = missing_readiness(&eligibility, &observed);
            if missing.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(CertificateError::Http01MissingReadiness {
                    hostname: hostname.to_string(),
                    token: token.to_string(),
                    missing_machine_ids: machine_id_strings(&missing),
                    excluded: exclusion_codes(&eligibility.excluded),
                }
                .into());
            }
            sleep(Duration::from_millis(100)).await;
        }
    }
}

fn challenge_eligibility(routing: &RoutingState, hostname: &str) -> Result<ChallengeEligibility> {
    if !hostname_is_advertised(routing, hostname)? {
        return Err(CertificateError::Http01HostnameNotAdvertised {
            hostname: hostname.to_string(),
        }
        .into());
    }

    let mut eligible = BTreeSet::new();
    let mut excluded = Vec::new();
    for machine in &routing.machines {
        match readiness_exclusion(machine) {
            Some(reason) => excluded.push(ChallengeReadinessExclusion {
                machine_id: machine.id.clone(),
                reason,
            }),
            None => {
                eligible.insert(machine.id.clone());
            }
        }
    }

    Ok(ChallengeEligibility { eligible, excluded })
}

fn missing_readiness(
    eligibility: &ChallengeEligibility,
    observed: &BTreeSet<MachineId>,
) -> Vec<MachineId> {
    eligibility
        .eligible
        .difference(observed)
        .cloned()
        .collect::<Vec<_>>()
}

fn readiness_exclusion(machine: &MachineMembership) -> Option<&'static str> {
    match machine.lifecycle {
        MachineLifecycle::Active => {}
        MachineLifecycle::Standby => return Some("excluded_by_lifecycle"),
        MachineLifecycle::Draining => return Some("excluded_by_lifecycle"),
    }
    if machine.subnet.is_none() {
        return Some("no_subnet");
    }
    None
}

fn hostname_is_advertised(routing: &RoutingState, hostname: &str) -> Result<bool> {
    let normalized = normalize_hostname(hostname);
    for release in &routing.releases {
        for revision in active_release_revisions(release) {
            for record in matching_revisions(routing, release, revision) {
                let spec: ServiceSpec =
                    serde_json::from_str(&record.spec_json).map_err(|error| {
                        CertificateError::Http01InvalidServiceRevision {
                            namespace: record.namespace.as_str().to_string(),
                            service: record.service.clone(),
                            revision_hash: record.revision_hash.clone(),
                            message: error.to_string(),
                        }
                    })?;
                if spec.routes.iter().any(|route| match route {
                    RouteSpec::Http(route) => route
                        .hostnames
                        .iter()
                        .any(|candidate| normalize_hostname(candidate) == normalized),
                    RouteSpec::Tcp(_) => false,
                }) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn active_release_revisions(release: &ServiceReleaseRecord) -> Vec<&str> {
    match &release.release.target {
        ployz_model::ServiceReleaseTarget::Direct { revision_hash } => {
            vec![revision_hash.as_str()]
        }
        ployz_model::ServiceReleaseTarget::Split { allocations, .. } => allocations
            .iter()
            .filter(|allocation| allocation.percent > 0)
            .map(|allocation| allocation.revision_hash.as_str())
            .collect(),
    }
}

fn matching_revisions<'a>(
    routing: &'a RoutingState,
    release: &ServiceReleaseRecord,
    revision_hash: &str,
) -> impl Iterator<Item = &'a ServiceRevisionRecord> {
    routing.revisions.iter().filter(move |record| {
        record.namespace == release.namespace
            && record.service == release.service
            && record.revision_hash == revision_hash
    })
}

fn normalize_hostname(hostname: &str) -> String {
    hostname.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn machine_id_strings(machine_ids: &[MachineId]) -> Vec<String> {
    machine_ids
        .iter()
        .map(|machine_id| machine_id.as_str().to_string())
        .collect::<Vec<_>>()
}

fn exclusion_codes(exclusions: &[ChallengeReadinessExclusion]) -> Vec<String> {
    exclusions
        .iter()
        .map(|excluded| format!("{}:{}", excluded.machine_id, excluded.reason))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_model::{DeployId, MachineTopology, OverlayIp, PublicKey, ServiceRelease};
    use ployz_spec::Namespace;
    use std::net::Ipv6Addr;

    #[test]
    fn challenge_eligibility_excludes_standby_and_no_subnet_machines() {
        let routing = RoutingState {
            machines: vec![
                test_machine("active", MachineLifecycle::Active, true),
                test_machine("standby", MachineLifecycle::Standby, true),
                test_machine("no-subnet", MachineLifecycle::Active, false),
            ],
            revisions: vec![test_revision("api", "rev-a", "example.com")],
            releases: vec![test_release("api", "rev-a")],
            instances: Vec::new(),
        };

        let eligibility =
            challenge_eligibility(&routing, "example.com").expect("hostname should be advertised");

        assert!(eligibility.eligible.contains(&MachineId::new("active")));
        assert!(!eligibility.eligible.contains(&MachineId::new("standby")));
        assert!(!eligibility.eligible.contains(&MachineId::new("no-subnet")));
        assert!(eligibility.excluded.iter().any(|excluded| {
            excluded.machine_id == MachineId::new("standby")
                && excluded.reason == "excluded_by_lifecycle"
        }));
        assert!(eligibility.excluded.iter().any(|excluded| {
            excluded.machine_id == MachineId::new("no-subnet") && excluded.reason == "no_subnet"
        }));
    }

    #[test]
    fn challenge_eligibility_fails_loudly_when_hostname_is_unknown() {
        let routing = RoutingState {
            machines: vec![test_machine("active", MachineLifecycle::Active, true)],
            revisions: vec![test_revision("api", "rev-a", "example.com")],
            releases: vec![test_release("api", "rev-a")],
            instances: Vec::new(),
        };

        let error =
            challenge_eligibility(&routing, "missing.example.com").expect_err("must fail loudly");

        assert!(matches!(
            error,
            Error::Certificate(CertificateError::Http01HostnameNotAdvertised { .. })
        ));
    }

    #[test]
    fn advertised_eligible_machine_missing_ack_blocks_readiness() {
        let eligibility = ChallengeEligibility {
            eligible: BTreeSet::from([MachineId::new("machine-a"), MachineId::new("machine-b")]),
            excluded: Vec::new(),
        };
        let observed = BTreeSet::from([MachineId::new("machine-a")]);

        let missing = missing_readiness(&eligibility, &observed);

        assert_eq!(missing, vec![MachineId::new("machine-b")]);
    }

    #[test]
    fn lock_contention_is_not_backend_failure() {
        let held = LockAcquireError::AlreadyHeld {
            key: "locks.cert.example".into(),
        };
        let raced = LockAcquireError::Contention {
            key: "locks.cert.example".into(),
            source: Error::operation("nats_lock_publish", "wrong last sequence"),
        };
        let backend = LockAcquireError::Backend(Error::operation(
            "nats_lock_read_for_acquire",
            "request timed out while reading lock",
        ));

        assert!(matches!(held, LockAcquireError::AlreadyHeld { .. }));
        assert!(matches!(raced, LockAcquireError::Contention { .. }));
        assert!(matches!(backend, LockAcquireError::Backend(_)));
    }

    fn test_machine(id: &str, lifecycle: MachineLifecycle, has_subnet: bool) -> MachineMembership {
        MachineMembership {
            id: MachineId::new(id),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            region_role: ployz_model::RegionRole::HomeData,
            subnet: has_subnet.then(|| "10.0.0.0/24".parse().expect("valid cidr")),
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle,
            storage_role: ployz_model::StorageParticipation::default_authority().into(),
            created_at: 1,
            updated_at: 1,
            labels: Default::default(),
        }
    }

    fn test_revision(service: &str, revision_hash: &str, hostname: &str) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: Namespace::new("prod"),
            service: service.into(),
            revision_hash: revision_hash.into(),
            spec_json: serde_json::json!({
                "name": service,
                "placement": "global",
                "template": { "image": "example/app:latest" },
                "network": "overlay",
                "service_ports": [{ "name": "http", "container_port": 8080 }],
                "routes": [{
                    "http": {
                        "service_port": "http",
                        "hostnames": [hostname],
                        "path_prefix": "/"
                    }
                }]
            })
            .to_string(),
            created_by: MachineId::new("active"),
            created_at: 1,
        }
    }

    fn test_release(service: &str, revision_hash: &str) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace: Namespace::new("prod"),
            service: service.into(),
            release: ServiceRelease::direct(
                revision_hash,
                Vec::new(),
                DeployId::new("deploy-1"),
                1,
            ),
        }
    }
}

impl DaemonState {
    pub(crate) async fn handle_acme_http01_status(
        &self,
        hostname: &str,
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };

        let certificate = match active.mesh.store.get_certificate(hostname).await {
            Ok(certificate) => certificate,
            Err(error) => return self.err("ACME_HTTP01_STATUS_FAILED", error.to_string()),
        };
        let challenges = match active.mesh.store.list_acme_challenges().await {
            Ok(challenges) => challenges,
            Err(error) => return self.err("ACME_HTTP01_STATUS_FAILED", error.to_string()),
        };

        let mut statuses = Vec::new();
        for challenge in challenges.into_iter().filter(|challenge| {
            normalize_hostname(&challenge.hostname) == normalize_hostname(hostname)
        }) {
            let readiness = match active
                .mesh
                .store
                .list_acme_challenge_readiness(&challenge.hostname, &challenge.token)
                .await
            {
                Ok(readiness) => readiness,
                Err(error) => return self.err("ACME_HTTP01_STATUS_FAILED", error.to_string()),
            };
            statuses.push(AcmeHttp01ChallengeStatus {
                challenge,
                readiness,
            });
        }

        self.ok_with_payload(
            format!("ACME HTTP-01 status for {hostname}"),
            Some(DaemonPayload::AcmeHttp01Status(AcmeHttp01StatusPayload {
                hostname: hostname.to_string(),
                certificate,
                challenges: statuses,
            })),
        )
    }

    pub(crate) async fn handle_acme_challenge_ready(
        &self,
        hostname: &str,
        token: &str,
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        match wait_for_local_challenge(&active.mesh.store, hostname, token).await {
            Ok(()) => self.ok("acme challenge ready"),
            Err(error) => self.err("ACME_CHALLENGE_NOT_READY", error.to_string()),
        }
    }
}

async fn wait_for_local_challenge(store: &StoreDriver, hostname: &str, token: &str) -> Result<()> {
    let start = tokio::time::Instant::now();
    loop {
        let visible = store
            .list_acme_challenges()
            .await?
            .iter()
            .any(|challenge| challenge.hostname == hostname && challenge.token == token);
        if visible {
            return Ok(());
        }
        if start.elapsed() >= HTTP01_CHALLENGE_VISIBILITY_TIMEOUT {
            return Err(CertificateError::Http01LocalChallengeNotVisible {
                hostname: hostname.to_string(),
                token: token.to_string(),
            }
            .into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
