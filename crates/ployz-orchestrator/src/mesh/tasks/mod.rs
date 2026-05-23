mod ebpf_sync;
mod endpoint_maintainer;
mod peer_sync;
mod self_record;
mod subnet_claim_monitor;

pub(crate) use ebpf_sync::run_ebpf_sync_task;
pub(crate) use endpoint_maintainer::{
    EndpointMaintainerCommand, EndpointMaintainerTask, EndpointSelectionMap,
    build_initial_endpoint_selections, run_endpoint_maintainer_task,
};
pub(crate) use peer_sync::{PeerSyncTask, run_peer_sync_task};
pub(crate) use self_record::SelfRecordCommand;
pub(crate) use self_record::SelfRecordMutation;
pub(crate) use self_record::apply_self_record_mutation;
pub(crate) use self_record::run_self_record_writer_task;
pub(crate) use subnet_claim_monitor::run_subnet_claim_monitor_task;

use crate::error::Error;
use ployz_types::time::now_unix_secs;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::warn;

#[derive(Debug, Error)]
pub enum TaskSetError {
    #[error("task subscribe failed: {0}")]
    Subscribe(Error),
    #[error("task panicked: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub(crate) struct TaskSet {
    tasks: JoinSet<()>,
    cancel: CancellationToken,
    health: Arc<Mutex<BTreeMap<String, MeshTaskHealth>>>,
}

impl TaskSet {
    pub(crate) fn new() -> (Self, CancellationToken) {
        let cancel = CancellationToken::new();
        let set = Self {
            tasks: JoinSet::new(),
            cancel: cancel.clone(),
            health: Arc::new(Mutex::new(BTreeMap::new())),
        };
        (set, cancel)
    }

    pub(crate) fn spawn_named(
        &mut self,
        name: &'static str,
        future: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let cancel = self.cancel.clone();
        mark_task_healthy(&self.health, name);
        let health = self.health.clone();
        self.tasks.spawn(async move {
            future.await;
            if !cancel.is_cancelled() {
                warn!(
                    task = name,
                    "mesh background task exited; cancelling task set"
                );
                mark_task_unhealthy(&health, name, "task exited unexpectedly");
                cancel.cancel();
            }
        });
    }

    pub(crate) fn health(&self) -> Vec<MeshTaskHealth> {
        self.health
            .lock()
            .expect("mesh task health lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    pub(crate) async fn stop(&mut self) -> Result<(), TaskSetError> {
        self.cancel.cancel();
        let mut first_err: Option<TaskSetError> = None;
        while let Some(result) = self.tasks.join_next().await {
            if let Err(e) = result {
                warn!(?e, "task join failed");
                first_err.get_or_insert(e.into());
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshTaskHealth {
    pub name: String,
    pub updated_at_unix_secs: u64,
    pub state: MeshTaskHealthState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshTaskHealthState {
    Healthy,
    Stale {
        stale_since_unix_secs: u64,
        consecutive_failures: u64,
        last_error: String,
    },
}

impl MeshTaskHealth {
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self.state, MeshTaskHealthState::Healthy)
    }

    #[must_use]
    pub fn stale_since_unix_secs(&self) -> Option<u64> {
        match self.state {
            MeshTaskHealthState::Healthy => None,
            MeshTaskHealthState::Stale {
                stale_since_unix_secs,
                ..
            } => Some(stale_since_unix_secs),
        }
    }

    #[must_use]
    pub fn consecutive_failures(&self) -> u64 {
        match self.state {
            MeshTaskHealthState::Healthy => 0,
            MeshTaskHealthState::Stale {
                consecutive_failures,
                ..
            } => consecutive_failures,
        }
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        match self.state {
            MeshTaskHealthState::Healthy => None,
            MeshTaskHealthState::Stale { ref last_error, .. } => Some(last_error),
        }
    }
}

fn mark_task_healthy(health: &Arc<Mutex<BTreeMap<String, MeshTaskHealth>>>, name: &str) {
    let now = now_unix_secs();
    health
        .lock()
        .expect("mesh task health lock poisoned")
        .insert(
            name.to_string(),
            MeshTaskHealth {
                name: name.to_string(),
                updated_at_unix_secs: now,
                state: MeshTaskHealthState::Healthy,
            },
        );
}

fn mark_task_unhealthy(
    health: &Arc<Mutex<BTreeMap<String, MeshTaskHealth>>>,
    name: &str,
    error: impl Into<String>,
) {
    let now = now_unix_secs();
    let mut health = health.lock().expect("mesh task health lock poisoned");
    let current = health.entry(name.to_string()).or_insert(MeshTaskHealth {
        name: name.to_string(),
        updated_at_unix_secs: now,
        state: MeshTaskHealthState::Healthy,
    });
    let (stale_since_unix_secs, consecutive_failures) = match current.state {
        MeshTaskHealthState::Healthy => (now, 1),
        MeshTaskHealthState::Stale {
            stale_since_unix_secs,
            consecutive_failures,
            ..
        } => (
            stale_since_unix_secs,
            consecutive_failures.saturating_add(1),
        ),
    };
    current.updated_at_unix_secs = now;
    current.state = MeshTaskHealthState::Stale {
        stale_since_unix_secs,
        consecutive_failures,
        last_error: error.into(),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unexpected_task_exit_cancels_task_set() {
        let (mut task_set, cancel) = TaskSet::new();

        task_set.spawn_named("test_task", async {});

        tokio::time::timeout(std::time::Duration::from_secs(1), cancel.cancelled())
            .await
            .expect("task set should cancel when a task exits unexpectedly");
        task_set.stop().await.expect("task set should stop cleanly");
    }

    #[tokio::test]
    async fn unexpected_named_task_exit_marks_task_unhealthy() {
        let (mut task_set, cancel) = TaskSet::new();

        task_set.spawn_named("peer_sync", async {});

        tokio::time::timeout(std::time::Duration::from_secs(1), cancel.cancelled())
            .await
            .expect("task set should cancel when a task exits unexpectedly");
        let health = task_set.health();
        task_set.stop().await.expect("task set should stop cleanly");

        let [peer_sync] = health.as_slice() else {
            panic!("expected one health row, got {health:?}");
        };
        assert_eq!(peer_sync.name, "peer_sync");
        let MeshTaskHealthState::Stale {
            consecutive_failures,
            last_error,
            ..
        } = &peer_sync.state
        else {
            panic!("expected stale health, got {peer_sync:?}");
        };
        assert_eq!(*consecutive_failures, 1);
        assert_eq!(last_error, "task exited unexpectedly");
    }
}
