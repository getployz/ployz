//! Role-local testimony caching for daemon processes.
//!
//! Each role process owns an independent in-memory cache of testimony received
//! from NATS. This is last-known evidence for that process, never shared
//! cluster truth and never an owner of Core machine testimony contracts.

use crate::identity::format_nuid_identity;
use futures_util::StreamExt;
use ployz_core::ids::MachineId;
use ployz_core::machine::GatewayStatusObservation;
use ployz_core::machine::MachineEndpointObservation;
#[cfg(test)]
use ployz_core::machine::runtime::ManagedContainerObservation;
use ployz_core::machine::runtime::{
    MachineContainerFactDelta, MachineContainerObservationSnapshot, MachineFactsSnapshot,
};
use ployz_core::network::internal_dns::{
    InternalDnsFactGeneration, InternalDnsFactWatermark, InternalDnsResolverCacheIncarnation,
};

use ployz_nats::subjects::{gateway_status_scope, machine_facts_scope};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct RoleTestimonyCache {
    resolver_cache_incarnation: InternalDnsResolverCacheIncarnation,
    state: Arc<RwLock<RoleTestimonyState>>,
    changes: watch::Sender<u64>,
}

impl Default for RoleTestimonyCache {
    fn default() -> Self {
        let (changes, _) = watch::channel(0);
        Self {
            resolver_cache_incarnation: InternalDnsResolverCacheIncarnation::try_new(
                format_nuid_identity("", &nuid::next()),
            )
            .expect("NUID is a valid resolver cache incarnation"),
            state: Arc::default(),
            changes,
        }
    }
}

#[derive(Debug, Default)]
struct RoleTestimonyState {
    machine_facts: BTreeMap<MachineId, MachineFactsSnapshot>,
    machine_fact_watermarks: BTreeMap<MachineId, InternalDnsFactWatermark>,
    gateway_statuses: BTreeMap<MachineId, GatewayStatusObservation>,
}

impl RoleTestimonyCache {
    #[must_use]
    pub(crate) fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn record_machine_facts(&self, facts: MachineFactsSnapshot) {
        {
            let mut state = self
                .state
                .write()
                .expect("role testimony cache lock is not poisoned");
            let machine_id = facts.machine_id().clone();
            let observed_at_unix_ms = facts.observed_at_unix_ms();
            state
                .machine_fact_watermarks
                .entry(machine_id.clone())
                .and_modify(|watermark| {
                    watermark.observed_at_unix_ms = observed_at_unix_ms;
                    watermark.generation = watermark.generation.next();
                })
                .or_insert_with(|| InternalDnsFactWatermark {
                    machine_id: machine_id.clone(),
                    observed_at_unix_ms,
                    resolver_cache_incarnation: self.resolver_cache_incarnation.clone(),
                    generation: InternalDnsFactGeneration::first(),
                });
            state.machine_facts.insert(machine_id, facts);
        }
        self.notify_change();
    }

    pub fn record_machine_container_fact(&self, delta: MachineContainerFactDelta) {
        let changed = {
            let mut state = self
                .state
                .write()
                .expect("role testimony cache lock is not poisoned");
            match delta {
                MachineContainerFactDelta::ContainerObserved {
                    observed_at_unix_ms,
                    observation,
                } => {
                    let observation = *observation;
                    let machine_id = observation.machine_id.clone();
                    let next = match state.machine_facts.get(&machine_id) {
                        Some(facts) => {
                            facts.with_container_replaced(observation, observed_at_unix_ms)
                        }
                        None => empty_machine_facts(machine_id.clone(), observed_at_unix_ms)
                            .and_then(|facts| {
                                facts.with_container_replaced(observation, observed_at_unix_ms)
                            }),
                    };
                    if let Ok(facts) = next {
                        state.machine_facts.insert(machine_id, facts);
                        true
                    } else {
                        false
                    }
                }
                MachineContainerFactDelta::ContainerRemoved {
                    machine_id,
                    container_id,
                    observed_at_unix_ms,
                } => {
                    let Some(facts) = state.machine_facts.get(&machine_id) else {
                        return;
                    };
                    if let Ok(next) =
                        facts.with_container_removed(&container_id, observed_at_unix_ms)
                    {
                        state.machine_facts.insert(machine_id, next);
                        true
                    } else {
                        false
                    }
                }
            }
        };
        if changed {
            self.notify_change();
        }
    }

    pub fn record_gateway_status(&self, status: GatewayStatusObservation) {
        {
            self.state
                .write()
                .expect("role testimony cache lock is not poisoned")
                .gateway_statuses
                .insert(status.machine_id.clone(), status);
        }
        self.notify_change();
    }

    fn notify_change(&self) {
        self.changes.send_modify(|generation| {
            *generation = generation.wrapping_add(1);
        });
    }

    #[must_use]
    #[cfg(test)]
    pub fn machine_facts(&self, machine_id: &MachineId) -> Option<MachineFactsSnapshot> {
        self.state
            .read()
            .expect("role testimony cache lock is not poisoned")
            .machine_facts
            .get(machine_id)
            .cloned()
    }

    #[must_use]
    pub fn machine_facts_all(&self) -> Vec<MachineFactsSnapshot> {
        self.state
            .read()
            .expect("role testimony cache lock is not poisoned")
            .machine_facts
            .values()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn machine_fact_watermarks(&self) -> Vec<InternalDnsFactWatermark> {
        self.state
            .read()
            .expect("role testimony cache lock is not poisoned")
            .machine_fact_watermarks
            .values()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn machine_container_snapshots(&self) -> Vec<MachineContainerObservationSnapshot> {
        self.machine_facts_all()
            .into_iter()
            .map(|facts| facts.containers().clone())
            .collect()
    }

    #[must_use]
    pub fn machine_endpoint_observations(&self) -> Vec<MachineEndpointObservation> {
        self.machine_facts_all()
            .into_iter()
            .filter_map(|facts| facts.endpoints().cloned())
            .collect()
    }

    #[must_use]
    pub fn gateway_status(&self, machine_id: &MachineId) -> Option<GatewayStatusObservation> {
        self.state
            .read()
            .expect("role testimony cache lock is not poisoned")
            .gateway_statuses
            .get(machine_id)
            .cloned()
    }

    #[must_use]
    pub fn gateway_statuses(&self) -> Vec<GatewayStatusObservation> {
        self.state
            .read()
            .expect("role testimony cache lock is not poisoned")
            .gateway_statuses
            .values()
            .cloned()
            .collect()
    }

    #[must_use]
    pub(crate) fn runtime_projection_facts(
        &self,
    ) -> (
        BTreeMap<MachineId, MachineFactsSnapshot>,
        BTreeMap<MachineId, GatewayStatusObservation>,
    ) {
        let state = self
            .state
            .read()
            .expect("role testimony cache lock is not poisoned");
        (state.machine_facts.clone(), state.gateway_statuses.clone())
    }
}

pub struct RunningRoleTestimonyCache {
    cache: RoleTestimonyCache,
    task: JoinHandle<()>,
}

impl RunningRoleTestimonyCache {
    #[must_use]
    pub fn cache(&self) -> RoleTestimonyCache {
        self.cache.clone()
    }

    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

pub async fn start_role_testimony_cache(
    client: async_nats::Client,
) -> Result<RunningRoleTestimonyCache, RoleTestimonyCacheError> {
    let machine_subject = machine_facts_scope();
    let gateway_subject = gateway_status_scope();
    let machine_facts = client
        .subscribe(machine_subject.clone())
        .await
        .map_err(|error| RoleTestimonyCacheError::Subscribe {
            subject: machine_subject,
            message: error.to_string(),
        })?;
    let gateway_statuses = client
        .subscribe(gateway_subject.clone())
        .await
        .map_err(|error| RoleTestimonyCacheError::Subscribe {
            subject: gateway_subject,
            message: error.to_string(),
        })?;
    let cache = RoleTestimonyCache::default();
    let task_cache = cache.clone();
    let task = tokio::spawn(consume_role_testimony(
        task_cache,
        machine_facts,
        gateway_statuses,
    ));

    Ok(RunningRoleTestimonyCache { cache, task })
}

async fn consume_role_testimony(
    cache: RoleTestimonyCache,
    mut machine_facts: async_nats::Subscriber,
    mut gateway_statuses: async_nats::Subscriber,
) {
    let mut throttle = DecodeFailureThrottle::default();
    loop {
        tokio::select! {
            Some(message) = machine_facts.next() => {
                ingest_machine_fact(&cache, &mut throttle, message.subject.as_str(), &message.payload);
            }
            Some(message) = gateway_statuses.next() => {
                ingest_gateway_status(&cache, &mut throttle, message.subject.as_str(), &message.payload);
            }
            else => break,
        }
    }
}

/// Per-subject throttle for undecodable-testimony warnings. Schema drift
/// between a machine and this core persists for a whole version-skew
/// window, and machines publish periodically, so an unthrottled warn per
/// message would flood the journal from the one loop that ingests every
/// machine's testimony. The first failure per subject reports immediately;
/// afterwards at most one report per interval, carrying the count of
/// messages dropped silently since the last report.
struct DecodeFailureThrottle {
    interval: Duration,
    subjects: HashMap<String, DecodeFailureWindow>,
}

struct DecodeFailureWindow {
    last_report: Instant,
    suppressed: u64,
}

const DECODE_FAILURE_REPORT_INTERVAL: Duration = Duration::from_secs(60);

impl Default for DecodeFailureThrottle {
    fn default() -> Self {
        Self {
            interval: DECODE_FAILURE_REPORT_INTERVAL,
            subjects: HashMap::new(),
        }
    }
}

/// Whether one more decode failure on a subject should be reported now.
enum DecodeFailureReport {
    /// Report, including how many failures were suppressed since the last
    /// report on this subject.
    Report {
        suppressed: u64,
    },
    Suppress,
}

impl DecodeFailureThrottle {
    fn observe(&mut self, subject: &str, now: Instant) -> DecodeFailureReport {
        match self.subjects.get_mut(subject) {
            None => {
                self.subjects.insert(
                    subject.to_owned(),
                    DecodeFailureWindow {
                        last_report: now,
                        suppressed: 0,
                    },
                );
                DecodeFailureReport::Report { suppressed: 0 }
            }
            Some(window) => {
                if now.duration_since(window.last_report) >= self.interval {
                    let suppressed = window.suppressed;
                    window.last_report = now;
                    window.suppressed = 0;
                    DecodeFailureReport::Report { suppressed }
                } else {
                    window.suppressed = window.suppressed.saturating_add(1);
                    DecodeFailureReport::Suppress
                }
            }
        }
    }
}

/// The machine that owns a testimony subject. Subject permissions scope a
/// machine credential to its own testimony subjects, so the subject token is
/// the authority over who a fact is about; a payload claiming another
/// machine's id is rejected rather than recorded.
fn subject_machine_id(subject: &str) -> Option<MachineId> {
    let tokens = subject.split('.').collect::<Vec<_>>();
    let ["plz", "v1", "testimony", _scope, machine_id, _leaf] = tokens.as_slice() else {
        return None;
    };
    MachineId::try_new(*machine_id).ok()
}

fn ingest_machine_fact(
    cache: &RoleTestimonyCache,
    throttle: &mut DecodeFailureThrottle,
    subject: &str,
    payload: &[u8],
) {
    let Some(owner) = subject_machine_id(subject) else {
        warn_unowned_subject(subject);
        return;
    };
    if let Ok(facts) = serde_json::from_slice::<MachineFactsSnapshot>(payload) {
        if facts.machine_id() == &owner {
            cache.record_machine_facts(facts);
        } else {
            warn_machine_id_mismatch(subject, facts.machine_id());
        }
    } else if let Ok(delta) = serde_json::from_slice::<MachineContainerFactDelta>(payload) {
        if delta_machine_id(&delta) == &owner {
            cache.record_machine_container_fact(delta);
        } else {
            warn_machine_id_mismatch(subject, delta_machine_id(&delta));
        }
    } else {
        // An undecodable payload usually means schema drift between a
        // machine and this core; dropping it silently would freeze the
        // cached facts while health still looks green.
        if let DecodeFailureReport::Report { suppressed } =
            throttle.observe(subject, Instant::now())
        {
            tracing::warn!(
                phase = "ingest",
                subject,
                payload_bytes = payload.len(),
                suppressed,
                "dropped machine testimony that decodes as neither a facts snapshot nor a container fact delta"
            );
        }
    }
}

fn ingest_gateway_status(
    cache: &RoleTestimonyCache,
    throttle: &mut DecodeFailureThrottle,
    subject: &str,
    payload: &[u8],
) {
    let Some(owner) = subject_machine_id(subject) else {
        warn_unowned_subject(subject);
        return;
    };
    if let Ok(status) = serde_json::from_slice::<GatewayStatusObservation>(payload) {
        if status.machine_id == owner {
            cache.record_gateway_status(status);
        } else {
            warn_machine_id_mismatch(subject, &status.machine_id);
        }
    } else if let DecodeFailureReport::Report { suppressed } =
        throttle.observe(subject, Instant::now())
    {
        tracing::warn!(
            phase = "ingest",
            subject,
            payload_bytes = payload.len(),
            suppressed,
            "dropped gateway testimony that does not decode as a gateway status observation"
        );
    }
}

fn delta_machine_id(delta: &MachineContainerFactDelta) -> &MachineId {
    match delta {
        MachineContainerFactDelta::ContainerObserved { observation, .. } => &observation.machine_id,
        MachineContainerFactDelta::ContainerRemoved { machine_id, .. } => machine_id,
    }
}

fn warn_machine_id_mismatch(subject: &str, claimed: &MachineId) {
    tracing::warn!(
        phase = "ingest",
        subject,
        payload_machine_id = claimed.as_str(),
        "rejected testimony whose payload machine id does not match its subject"
    );
}

fn warn_unowned_subject(subject: &str) {
    tracing::warn!(
        phase = "ingest",
        subject,
        "rejected testimony on a subject with no valid owning machine id"
    );
}

fn empty_machine_facts(
    machine_id: MachineId,
    observed_at_unix_ms: u64,
) -> Result<MachineFactsSnapshot, ployz_core::machine::runtime::MachineFactsSnapshotError> {
    let containers =
        MachineContainerObservationSnapshot::try_new(machine_id.clone(), Vec::new())
            .map_err(ployz_core::machine::runtime::MachineFactsSnapshotError::BuildContainers)?;
    MachineFactsSnapshot::try_new(
        machine_id,
        containers,
        None,
        test_disk_space(),
        None,
        ployz_core::image::OciPlatform::current(),
        observed_at_unix_ms,
    )
}

fn test_disk_space() -> ployz_core::machine::runtime::MachineDiskSpace {
    ployz_core::machine::runtime::MachineDiskSpace {
        available_bytes: 40,
        total_bytes: 100,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoleTestimonyCacheError {
    #[error("subscribe {subject}: {message}")]
    Subscribe { subject: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::{
        ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId,
        StepId,
    };
    use ployz_core::machine::runtime::{
        ContainerRuntimeState, ManagedContainerIdentity, ManagedContainerKind,
    };

    #[test]
    fn full_snapshot_replaces_machine_facts() {
        let cache = RoleTestimonyCache::default();
        cache.record_machine_facts(machine_facts("machine_a", 1, [observation("ctr_1")]));
        cache.record_machine_facts(machine_facts("machine_a", 2, [observation("ctr_2")]));

        let facts = cache
            .machine_facts(&machine_id("machine_a"))
            .expect("facts stored");
        assert_eq!(facts.observed_at_unix_ms(), 2);
        assert!(
            facts
                .containers()
                .container(&container_id("ctr_1"))
                .is_none()
        );
        assert!(
            facts
                .containers()
                .container(&container_id("ctr_2"))
                .is_some()
        );
    }

    #[tokio::test]
    async fn successful_mutations_notify_after_replacement() {
        let cache = RoleTestimonyCache::default();
        let mut changes = cache.subscribe_changes();

        cache.record_machine_facts(machine_facts("machine_a", 1, [observation("ctr_1")]));
        changes.changed().await.expect("machine change");
        assert_eq!(
            cache
                .machine_facts(&machine_id("machine_a"))
                .expect("replacement visible before notification")
                .observed_at_unix_ms(),
            1
        );
    }

    #[test]
    fn full_snapshots_with_equal_timestamps_advance_generation() {
        let cache = RoleTestimonyCache::default();
        cache.record_machine_facts(machine_facts("machine_a", 42, [observation("ctr_1")]));
        cache.record_machine_facts(machine_facts("machine_a", 42, [observation("ctr_2")]));

        let watermarks = cache.machine_fact_watermarks();
        let [watermark] = watermarks.as_slice() else {
            panic!("expected one watermark");
        };
        assert_eq!(watermark.generation.get(), 2);
    }

    #[test]
    fn separate_role_testimony_caches_have_distinct_incarnations() {
        let first = RoleTestimonyCache::default();
        let second = RoleTestimonyCache::default();

        assert_ne!(
            first.resolver_cache_incarnation,
            second.resolver_cache_incarnation
        );
    }

    #[test]
    fn cloned_role_testimony_cache_preserves_incarnation() {
        let cache = RoleTestimonyCache::default();

        assert_eq!(
            cache.resolver_cache_incarnation,
            cache.clone().resolver_cache_incarnation
        );
    }

    #[test]
    fn container_deltas_do_not_advance_full_snapshot_generation() {
        let cache = RoleTestimonyCache::default();
        cache.record_machine_facts(machine_facts("machine_a", 1, [observation("ctr_1")]));
        cache.record_machine_container_fact(MachineContainerFactDelta::ContainerObserved {
            observed_at_unix_ms: 2,
            observation: Box::new(observation("ctr_2")),
        });

        let watermarks = cache.machine_fact_watermarks();
        let [watermark] = watermarks.as_slice() else {
            panic!("expected one watermark");
        };
        assert_eq!(watermark.generation, InternalDnsFactGeneration::first());
    }

    #[test]
    fn observed_delta_replaces_one_container() {
        let cache = RoleTestimonyCache::default();
        cache.record_machine_facts(machine_facts("machine_a", 1, [observation("ctr_1")]));

        cache.record_machine_container_fact(MachineContainerFactDelta::ContainerObserved {
            observed_at_unix_ms: 2,
            observation: Box::new(ManagedContainerObservation {
                state: ContainerRuntimeState::Exited,
                ..observation("ctr_1")
            }),
        });

        let facts = cache
            .machine_facts(&machine_id("machine_a"))
            .expect("facts stored");
        assert_eq!(facts.observed_at_unix_ms(), 2);
        assert_eq!(
            facts
                .containers()
                .container(&container_id("ctr_1"))
                .expect("container stored")
                .state,
            ContainerRuntimeState::Exited
        );
    }

    #[test]
    fn removed_delta_removes_one_container() {
        let cache = RoleTestimonyCache::default();
        cache.record_machine_facts(machine_facts(
            "machine_a",
            1,
            [observation("ctr_1"), observation("ctr_2")],
        ));

        cache.record_machine_container_fact(MachineContainerFactDelta::ContainerRemoved {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_1"),
            observed_at_unix_ms: 2,
        });

        let facts = cache
            .machine_facts(&machine_id("machine_a"))
            .expect("facts stored");
        assert_eq!(facts.observed_at_unix_ms(), 2);
        assert!(
            facts
                .containers()
                .container(&container_id("ctr_1"))
                .is_none()
        );
        assert!(
            facts
                .containers()
                .container(&container_id("ctr_2"))
                .is_some()
        );
    }

    #[test]
    fn observed_delta_before_snapshot_creates_minimal_machine_facts() {
        let cache = RoleTestimonyCache::default();

        cache.record_machine_container_fact(MachineContainerFactDelta::ContainerObserved {
            observed_at_unix_ms: 7,
            observation: Box::new(observation("ctr_1")),
        });

        let facts = cache
            .machine_facts(&machine_id("machine_a"))
            .expect("facts stored");
        assert_eq!(facts.observed_at_unix_ms(), 7);
        assert!(facts.endpoints().is_none());
        assert!(
            facts
                .containers()
                .container(&container_id("ctr_1"))
                .is_some()
        );
    }

    #[test]
    fn snapshot_with_payload_machine_id_matching_subject_is_recorded() {
        let cache = RoleTestimonyCache::default();
        let facts = machine_facts("machine_a", 1, [observation("ctr_1")]);

        ingest_machine_fact(
            &cache,
            &mut DecodeFailureThrottle::default(),
            "plz.v1.testimony.machine.machine_a.snapshot",
            &serde_json::to_vec(&facts).expect("facts serialize"),
        );

        assert!(cache.machine_facts(&machine_id("machine_a")).is_some());
    }

    #[test]
    fn snapshot_with_payload_machine_id_not_matching_subject_is_rejected() {
        let cache = RoleTestimonyCache::default();
        let facts = machine_facts("machine_a", 1, [observation("ctr_1")]);

        ingest_machine_fact(
            &cache,
            &mut DecodeFailureThrottle::default(),
            "plz.v1.testimony.machine.machine_b.snapshot",
            &serde_json::to_vec(&facts).expect("facts serialize"),
        );

        assert!(cache.machine_facts(&machine_id("machine_a")).is_none());
        assert!(cache.machine_facts(&machine_id("machine_b")).is_none());
    }

    #[test]
    fn container_delta_with_payload_machine_id_not_matching_subject_is_rejected() {
        let cache = RoleTestimonyCache::default();
        let delta = MachineContainerFactDelta::ContainerObserved {
            observed_at_unix_ms: 1,
            observation: Box::new(observation("ctr_1")),
        };

        ingest_machine_fact(
            &cache,
            &mut DecodeFailureThrottle::default(),
            "plz.v1.testimony.machine.machine_b.containers",
            &serde_json::to_vec(&delta).expect("delta serializes"),
        );

        assert!(cache.machine_facts(&machine_id("machine_a")).is_none());
        assert!(cache.machine_facts(&machine_id("machine_b")).is_none());
    }

    #[test]
    fn gateway_status_with_payload_machine_id_not_matching_subject_is_rejected() {
        let cache = RoleTestimonyCache::default();
        let status = GatewayStatusObservation {
            machine_id: machine_id("machine_a"),
            listen_addr: "203.0.113.10:443".parse().expect("valid socket addr"),
            serving: ployz_core::machine::GatewayServingStatus::Current,
            route_count: 1,
            process_health: ployz_core::machine::GatewayProcessHealth::default(),
        };

        ingest_gateway_status(
            &cache,
            &mut DecodeFailureThrottle::default(),
            "plz.v1.testimony.gateway.machine_b.status",
            &serde_json::to_vec(&status).expect("status serializes"),
        );

        assert!(cache.gateway_status(&machine_id("machine_a")).is_none());
        assert!(cache.gateway_status(&machine_id("machine_b")).is_none());
    }

    fn machine_facts(
        machine_id_value: &str,
        observed_at_unix_ms: u64,
        containers: impl IntoIterator<Item = ManagedContainerObservation>,
    ) -> MachineFactsSnapshot {
        let machine_id = machine_id(machine_id_value);
        MachineFactsSnapshot::try_new(
            machine_id.clone(),
            MachineContainerObservationSnapshot::try_new(machine_id, containers)
                .expect("valid container snapshot"),
            None,
            ployz_test_support::fixtures::test_disk_space(),
            None,
            ployz_core::image::OciPlatform::current(),
            observed_at_unix_ms,
        )
        .expect("valid machine facts")
    }

    fn observation(container_id_value: &str) -> ManagedContainerObservation {
        ManagedContainerObservation {
            machine_id: machine_id("machine_a"),
            container_id: container_id(container_id_value),
            identity: ManagedContainerIdentity {
                namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
                service_id: ServiceId::try_new("svc_api").expect("valid service id"),
                namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("entry_api")
                    .expect("valid entry id"),
                operation_id: OperationId::try_new("op_123").expect("valid operation id"),
                step_id: StepId::try_new("run_1").expect("valid step id"),
                kind: ManagedContainerKind::Service,
            },
            state: ContainerRuntimeState::running_unroutable(),
            health_status: None,
            resolved_image_identity: None,
            created_at_unix_seconds: None,
            named_volume_names: Default::default(),
        }
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }

    #[test]
    fn decode_failure_throttle_reports_first_then_once_per_interval_with_suppressed_count() {
        let mut throttle = DecodeFailureThrottle::default();
        let start = Instant::now();
        let subject = "plz.v1.testimony.machine.machine_a.snapshot";

        assert!(matches!(
            throttle.observe(subject, start),
            DecodeFailureReport::Report { suppressed: 0 }
        ));
        assert!(matches!(
            throttle.observe(subject, start + Duration::from_secs(1)),
            DecodeFailureReport::Suppress
        ));
        assert!(matches!(
            throttle.observe(subject, start + Duration::from_secs(2)),
            DecodeFailureReport::Suppress
        ));
        assert!(matches!(
            throttle.observe(
                subject,
                start + DECODE_FAILURE_REPORT_INTERVAL + Duration::from_secs(2)
            ),
            DecodeFailureReport::Report { suppressed: 2 }
        ));
        // A different subject reports independently.
        assert!(matches!(
            throttle.observe("plz.v1.testimony.machine.machine_b.snapshot", start),
            DecodeFailureReport::Report { suppressed: 0 }
        ));
    }
}
