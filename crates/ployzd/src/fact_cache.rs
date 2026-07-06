//! Plain-subject fact fanout consumed by role runtimes.

use futures_util::StreamExt;
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::machine_runtime::{
    MachineContainerObservationSnapshot, MachineFactsSnapshot, ManagedContainerObservation,
};
use ployz_core::state::{GatewayStatusObservation, MachinePublicIpObservation};
use ployz_core::subjects::{gateway_status_scope, machine_facts_scope};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Default)]
pub struct RuntimeFactsCache {
    state: Arc<RwLock<RuntimeFactsState>>,
}

#[derive(Debug, Default)]
struct RuntimeFactsState {
    machine_facts: BTreeMap<MachineId, MachineFactsSnapshot>,
    gateway_statuses: BTreeMap<MachineId, GatewayStatusObservation>,
}

impl RuntimeFactsCache {
    pub fn record_machine_facts(&self, facts: MachineFactsSnapshot) {
        self.state
            .write()
            .expect("runtime facts cache lock is not poisoned")
            .machine_facts
            .insert(facts.machine_id().clone(), facts);
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
    pub fn machine_public_ips(&self) -> Vec<MachinePublicIpObservation> {
        self.machine_facts_all()
            .into_iter()
            .filter_map(|facts| facts.public_ip().cloned())
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

pub struct RunningRuntimeFactsCache {
    cache: RuntimeFactsCache,
    task: JoinHandle<()>,
}

impl RunningRuntimeFactsCache {
    #[must_use]
    pub fn cache(&self) -> RuntimeFactsCache {
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

pub async fn start_runtime_facts_cache(
    client: async_nats::Client,
) -> Result<RunningRuntimeFactsCache, RuntimeFactsCacheError> {
    let machine_subject = machine_facts_scope();
    let gateway_subject = gateway_status_scope();
    let machine_facts = client
        .subscribe(machine_subject.clone())
        .await
        .map_err(|error| RuntimeFactsCacheError::Subscribe {
            subject: machine_subject,
            message: error.to_string(),
        })?;
    let gateway_statuses = client
        .subscribe(gateway_subject.clone())
        .await
        .map_err(|error| RuntimeFactsCacheError::Subscribe {
            subject: gateway_subject,
            message: error.to_string(),
        })?;
    let cache = RuntimeFactsCache::default();
    let task_cache = cache.clone();
    let task = tokio::spawn(async move {
        consume_runtime_facts(task_cache, machine_facts, gateway_statuses).await;
    });

    Ok(RunningRuntimeFactsCache { cache, task })
}

async fn consume_runtime_facts(
    cache: RuntimeFactsCache,
    mut machine_facts: async_nats::Subscriber,
    mut gateway_statuses: async_nats::Subscriber,
) {
    loop {
        tokio::select! {
            Some(message) = machine_facts.next() => {
                if let Ok(facts) = serde_json::from_slice::<MachineFactsSnapshot>(&message.payload) {
                    cache.record_machine_facts(facts);
                }
            }
            Some(message) = gateway_statuses.next() => {
                if let Ok(status) = serde_json::from_slice::<GatewayStatusObservation>(&message.payload) {
                    cache.record_gateway_status(status);
                }
            }
            else => break,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeFactsCacheError {
    Subscribe { subject: String, message: String },
}

impl fmt::Display for RuntimeFactsCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Subscribe { subject, message } => {
                write!(formatter, "subscribe {subject}: {message}")
            }
        }
    }
}

impl std::error::Error for RuntimeFactsCacheError {}
