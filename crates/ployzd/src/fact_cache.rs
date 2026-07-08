//! Plain-subject fact fanout consumed by role processes.

use futures_util::StreamExt;
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::machine_runtime::{
    MachineContainerFactDelta, MachineContainerObservationSnapshot, MachineFactsSnapshot,
    ManagedContainerObservation,
};
use ployz_core::state::{GatewayStatusObservation, MachineEndpointObservation};
use ployz_core::subjects::{gateway_status_scope, machine_facts_scope};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Default)]
pub struct FactCache {
    state: Arc<RwLock<RuntimeFactsState>>,
}

#[derive(Debug, Default)]
struct RuntimeFactsState {
    machine_facts: BTreeMap<MachineId, MachineFactsSnapshot>,
    gateway_statuses: BTreeMap<MachineId, GatewayStatusObservation>,
}

impl FactCache {
    pub fn record_machine_facts(&self, facts: MachineFactsSnapshot) {
        self.state
            .write()
            .expect("runtime facts cache lock is not poisoned")
            .machine_facts
            .insert(facts.machine_id().clone(), facts);
    }

    pub fn record_machine_container_fact(&self, delta: MachineContainerFactDelta) {
        let mut state = self
            .state
            .write()
            .expect("runtime facts cache lock is not poisoned");
        match delta {
            MachineContainerFactDelta::ContainerObserved {
                observed_at_unix_ms,
                observation,
            } => {
                let machine_id = observation.machine_id.clone();
                let next = match state.machine_facts.get(&machine_id) {
                    Some(facts) => facts.with_container_replaced(observation, observed_at_unix_ms),
                    None => empty_machine_facts(machine_id.clone(), observed_at_unix_ms).and_then(
                        |facts| facts.with_container_replaced(observation, observed_at_unix_ms),
                    ),
                };
                if let Ok(facts) = next {
                    state.machine_facts.insert(machine_id, facts);
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
                if let Ok(next) = facts.with_container_removed(&container_id, observed_at_unix_ms) {
                    state.machine_facts.insert(machine_id, next);
                }
            }
        }
    }

    pub fn record_gateway_status(&self, status: GatewayStatusObservation) {
        self.state
            .write()
            .expect("runtime facts cache lock is not poisoned")
            .gateway_statuses
            .insert(status.machine_id.clone(), status);
    }

    #[must_use]
    pub fn machine_facts(&self, machine_id: &MachineId) -> Option<MachineFactsSnapshot> {
        self.state
            .read()
            .expect("runtime facts cache lock is not poisoned")
            .machine_facts
            .get(machine_id)
            .cloned()
    }

    #[must_use]
    pub fn machine_facts_all(&self) -> Vec<MachineFactsSnapshot> {
        self.state
            .read()
            .expect("runtime facts cache lock is not poisoned")
            .machine_facts
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
    pub fn container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Option<ManagedContainerObservation> {
        self.machine_facts(machine_id)
            .and_then(|facts| facts.containers().container(container_id).cloned())
    }

    #[must_use]
    pub fn gateway_status(&self, machine_id: &MachineId) -> Option<GatewayStatusObservation> {
        self.state
            .read()
            .expect("runtime facts cache lock is not poisoned")
            .gateway_statuses
            .get(machine_id)
            .cloned()
    }

    #[must_use]
    pub fn gateway_statuses(&self) -> Vec<GatewayStatusObservation> {
        self.state
            .read()
            .expect("runtime facts cache lock is not poisoned")
            .gateway_statuses
            .values()
            .cloned()
            .collect()
    }
}

pub struct RunningFactCache {
    cache: FactCache,
    task: JoinHandle<()>,
}

impl RunningFactCache {
    #[must_use]
    pub fn cache(&self) -> FactCache {
        self.cache.clone()
    }

    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }

    #[must_use]
    pub fn into_task(self) -> JoinHandle<()> {
        self.task
    }
}

pub async fn start_fact_cache(
    client: async_nats::Client,
) -> Result<RunningFactCache, FactCacheError> {
    let machine_subject = machine_facts_scope();
    let gateway_subject = gateway_status_scope();
    let machine_facts = client
        .subscribe(machine_subject.clone())
        .await
        .map_err(|error| FactCacheError::Subscribe {
            subject: machine_subject,
            message: error.to_string(),
        })?;
    let gateway_statuses = client
        .subscribe(gateway_subject.clone())
        .await
        .map_err(|error| FactCacheError::Subscribe {
            subject: gateway_subject,
            message: error.to_string(),
        })?;
    let cache = FactCache::default();
    let task_cache = cache.clone();
    let task = tokio::spawn(async move {
        consume_facts(task_cache, machine_facts, gateway_statuses).await;
    });

    Ok(RunningFactCache { cache, task })
}

async fn consume_facts(
    cache: FactCache,
    mut machine_facts: async_nats::Subscriber,
    mut gateway_statuses: async_nats::Subscriber,
) {
    loop {
        tokio::select! {
            Some(message) = machine_facts.next() => {
                ingest_machine_fact(&cache, message.subject.as_str(), &message.payload);
            }
            Some(message) = gateway_statuses.next() => {
                ingest_gateway_status(&cache, message.subject.as_str(), &message.payload);
            }
            else => break,
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

fn ingest_machine_fact(cache: &FactCache, subject: &str, payload: &[u8]) {
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
    }
}

fn ingest_gateway_status(cache: &FactCache, subject: &str, payload: &[u8]) {
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
    }
}

fn delta_machine_id(delta: &MachineContainerFactDelta) -> &MachineId {
    match delta {
        MachineContainerFactDelta::ContainerObserved { observation, .. } => &observation.machine_id,
        MachineContainerFactDelta::ContainerRemoved { machine_id, .. } => machine_id,
    }
}

fn warn_machine_id_mismatch(subject: &str, claimed: &MachineId) {
    eprintln!(
        "ployzd fact cache warning: phase=ingest rejected testimony whose payload machine id does not match its subject subject={subject} payload_machine_id={}",
        claimed.as_str()
    );
}

fn warn_unowned_subject(subject: &str) {
    eprintln!(
        "ployzd fact cache warning: phase=ingest rejected testimony on a subject with no valid owning machine id subject={subject}"
    );
}

fn empty_machine_facts(
    machine_id: MachineId,
    observed_at_unix_ms: u64,
) -> Result<MachineFactsSnapshot, ployz_core::machine_runtime::MachineFactsSnapshotError> {
    let containers =
        MachineContainerObservationSnapshot::try_new(machine_id.clone(), Vec::new())
            .map_err(ployz_core::machine_runtime::MachineFactsSnapshotError::BuildContainers)?;
    MachineFactsSnapshot::try_new(machine_id, containers, None, observed_at_unix_ms)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactCacheError {
    Subscribe { subject: String, message: String },
}

impl fmt::Display for FactCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Subscribe { subject, message } => {
                write!(formatter, "subscribe {subject}: {message}")
            }
        }
    }
}

impl std::error::Error for FactCacheError {}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::{
        ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId,
        StepId,
    };
    use ployz_core::machine_runtime::{
        ContainerRuntimeState, ManagedContainerIdentity, ManagedContainerKind,
    };

    #[test]
    fn full_snapshot_replaces_machine_facts() {
        let cache = FactCache::default();
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

    #[test]
    fn observed_delta_replaces_one_container() {
        let cache = FactCache::default();
        cache.record_machine_facts(machine_facts("machine_a", 1, [observation("ctr_1")]));

        cache.record_machine_container_fact(MachineContainerFactDelta::ContainerObserved {
            observed_at_unix_ms: 2,
            observation: ManagedContainerObservation {
                state: ContainerRuntimeState::Exited,
                ..observation("ctr_1")
            },
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
        let cache = FactCache::default();
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
        let cache = FactCache::default();

        cache.record_machine_container_fact(MachineContainerFactDelta::ContainerObserved {
            observed_at_unix_ms: 7,
            observation: observation("ctr_1"),
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
        let cache = FactCache::default();
        let facts = machine_facts("machine_a", 1, [observation("ctr_1")]);

        ingest_machine_fact(
            &cache,
            "plz.v1.testimony.machine.machine_a.snapshot",
            &serde_json::to_vec(&facts).expect("facts serialize"),
        );

        assert!(cache.machine_facts(&machine_id("machine_a")).is_some());
    }

    #[test]
    fn snapshot_with_payload_machine_id_not_matching_subject_is_rejected() {
        let cache = FactCache::default();
        let facts = machine_facts("machine_a", 1, [observation("ctr_1")]);

        ingest_machine_fact(
            &cache,
            "plz.v1.testimony.machine.machine_b.snapshot",
            &serde_json::to_vec(&facts).expect("facts serialize"),
        );

        assert!(cache.machine_facts(&machine_id("machine_a")).is_none());
        assert!(cache.machine_facts(&machine_id("machine_b")).is_none());
    }

    #[test]
    fn container_delta_with_payload_machine_id_not_matching_subject_is_rejected() {
        let cache = FactCache::default();
        let delta = MachineContainerFactDelta::ContainerObserved {
            observed_at_unix_ms: 1,
            observation: observation("ctr_1"),
        };

        ingest_machine_fact(
            &cache,
            "plz.v1.testimony.machine.machine_b.containers",
            &serde_json::to_vec(&delta).expect("delta serializes"),
        );

        assert!(cache.machine_facts(&machine_id("machine_a")).is_none());
        assert!(cache.machine_facts(&machine_id("machine_b")).is_none());
    }

    #[test]
    fn gateway_status_with_payload_machine_id_not_matching_subject_is_rejected() {
        let cache = FactCache::default();
        let status = GatewayStatusObservation {
            machine_id: machine_id("machine_a"),
            listen_addr: "203.0.113.10:443".parse().expect("valid socket addr"),
            serving: ployz_core::state::GatewayServingStatus::Current,
            route_count: 1,
        };

        ingest_gateway_status(
            &cache,
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
        }
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }
}
