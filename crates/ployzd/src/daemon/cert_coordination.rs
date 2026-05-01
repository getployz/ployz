use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use ployz_nats::coord::locks::{Lease, NatsLocks};
use ployz_nats::subjects;
use ployz_orchestrator::certificates::{
    AccountAcquisition, AcmeAccountCoordinator, HTTP01_CHALLENGE_VISIBILITY_TIMEOUT,
    Http01ChallengeReadiness, IssuanceAcquisition, IssuanceCoordinator, IssuanceHold,
};
use ployz_orchestrator::coordination::ReservationId;
use ployz_store_api::{CertificateStore, RoutingSnapshotReader, StoreDriver};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    MachineId, MachineLifecycle, MachineMembership, RoutingState, ServiceReleaseRecord,
    ServiceRevisionRecord, ServiceRoutingPolicy,
};
use ployz_types::spec::{RouteSpec, ServiceSpec};
use ployz_types::time::now_unix_secs;
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

    async fn acquire_key(&self, key: String) -> std::result::Result<Lease, Error> {
        self.locks
            .acquire(
                &key,
                self.owner.0.clone(),
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
        match self.acquire_key(subjects::cert_lock(hostname)).await {
            Ok(lease) => IssuanceAcquisition::Allowed(self.hold_for(lease)),
            Err(error) if is_lock_contention(&error) => {
                IssuanceAcquisition::VetoedByPeer(error.to_string())
            }
            Err(error) => IssuanceAcquisition::CoordinationFailed(error.to_string()),
        }
    }
}

#[async_trait]
impl AcmeAccountCoordinator for NatsIssuanceCoordinator {
    async fn try_acquire_account(&self, issuer_url: &str) -> AccountAcquisition {
        match self
            .acquire_key(subjects::acme_account_lock(issuer_url))
            .await
        {
            Ok(lease) => AccountAcquisition::Allowed(self.hold_for(lease)),
            Err(error) if is_lock_contention(&error) => {
                AccountAcquisition::VetoedByPeer(error.to_string())
            }
            Err(error) => AccountAcquisition::CoordinationFailed(error.to_string()),
        }
    }
}

fn is_lock_contention(error: &Error) -> bool {
    match error {
        Error::Operation {
            operation: "nats_lock_acquire",
            message,
        } => message.contains("already held") || message.contains("contention:"),
        Error::Operation { .. } => false,
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
            return Err(Error::operation(
                "acme_challenge_visibility",
                format!(
                    "eligibility_unknown: no active advertised gateway is eligible for HTTP-01 challenge {hostname}; excluded={}",
                    format_exclusions(&eligibility.excluded)
                ),
            ));
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
                return Err(Error::operation(
                    "acme_challenge_visibility",
                    format!(
                        "HTTP-01 challenge for {hostname} token {token} is missing readiness observations from advertised eligible gateways: {}; reason=missing_ack; excluded={}",
                        format_machine_ids(&missing),
                        format_exclusions(&eligibility.excluded)
                    ),
                ));
            }
            sleep(Duration::from_millis(100)).await;
        }
    }
}

fn challenge_eligibility(routing: &RoutingState, hostname: &str) -> Result<ChallengeEligibility> {
    if !hostname_is_advertised(routing, hostname)? {
        return Err(Error::operation(
            "acme_challenge_visibility",
            format!(
                "eligibility_unknown: hostname {hostname} is not in the current advertised routing state"
            ),
        ));
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
                        Error::operation(
                            "acme_challenge_visibility",
                            format!(
                                "eligibility_unknown: invalid service revision {}/{}@{}: {error}",
                                record.namespace, record.service, record.revision_hash
                            ),
                        )
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
    match &release.release.routing {
        ServiceRoutingPolicy::Direct { revision_hash } => vec![revision_hash.as_str()],
        ServiceRoutingPolicy::Split { allocations } => allocations
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

fn format_machine_ids(machine_ids: &[MachineId]) -> String {
    machine_ids
        .iter()
        .map(|machine_id| machine_id.0.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

fn format_exclusions(exclusions: &[ChallengeReadinessExclusion]) -> String {
    if exclusions.is_empty() {
        return "none".into();
    }
    exclusions
        .iter()
        .map(|excluded| format!("{}:{}", excluded.machine_id, excluded.reason))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{
        DeployId, MachineRole, MachineTopology, OverlayIp, PublicKey, ServiceRelease,
    };
    use ployz_types::spec::Namespace;
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

        assert!(eligibility.eligible.contains(&MachineId("active".into())));
        assert!(!eligibility.eligible.contains(&MachineId("standby".into())));
        assert!(
            !eligibility
                .eligible
                .contains(&MachineId("no-subnet".into()))
        );
        assert!(eligibility.excluded.iter().any(|excluded| {
            excluded.machine_id == MachineId("standby".into())
                && excluded.reason == "excluded_by_lifecycle"
        }));
        assert!(eligibility.excluded.iter().any(|excluded| {
            excluded.machine_id == MachineId("no-subnet".into()) && excluded.reason == "no_subnet"
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

        assert!(error.to_string().contains("eligibility_unknown"));
    }

    #[test]
    fn advertised_eligible_machine_missing_ack_blocks_readiness() {
        let eligibility = ChallengeEligibility {
            eligible: BTreeSet::from([
                MachineId("machine-a".into()),
                MachineId("machine-b".into()),
            ]),
            excluded: Vec::new(),
        };
        let observed = BTreeSet::from([MachineId("machine-a".into())]);

        let missing = missing_readiness(&eligibility, &observed);

        assert_eq!(missing, vec![MachineId("machine-b".into())]);
    }

    #[test]
    fn lock_contention_is_not_backend_failure() {
        let held = Error::operation(
            "nats_lock_acquire",
            "lock 'locks.cert.example' is already held",
        );
        let raced = Error::operation(
            "nats_lock_acquire",
            "lock 'locks.cert.example' contention: wrong last sequence",
        );
        let backend = Error::operation(
            "nats_lock_read_for_acquire",
            "request timed out while reading lock",
        );

        assert!(is_lock_contention(&held));
        assert!(is_lock_contention(&raced));
        assert!(!is_lock_contention(&backend));
    }

    fn test_machine(id: &str, lifecycle: MachineLifecycle, has_subnet: bool) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            subnet: has_subnet.then(|| "10.0.0.0/24".parse().expect("valid cidr")),
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle,
            role: MachineRole::StorageCandidate,
            created_at: 1,
            updated_at: 1,
            labels: Default::default(),
        }
    }

    fn test_revision(service: &str, revision_hash: &str, hostname: &str) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: Namespace("prod".into()),
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
            created_by: MachineId("active".into()),
            created_at: 1,
        }
    }

    fn test_release(service: &str, revision_hash: &str) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace: Namespace("prod".into()),
            service: service.into(),
            release: ServiceRelease {
                primary_revision_hash: revision_hash.into(),
                referenced_revision_hashes: vec![revision_hash.into()],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: revision_hash.into(),
                },
                slots: Vec::new(),
                updated_by_deploy_id: DeployId("deploy-1".into()),
                updated_at: 1,
            },
        }
    }
}

impl DaemonState {
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
            return Err(Error::operation(
                "acme_challenge_visibility",
                format!(
                    "HTTP-01 challenge for {hostname} was not visible in local store within {:?}",
                    HTTP01_CHALLENGE_VISIBILITY_TIMEOUT
                ),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
