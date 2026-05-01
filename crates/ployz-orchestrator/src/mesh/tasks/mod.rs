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
}

impl TaskSet {
    pub(crate) fn new() -> (Self, CancellationToken) {
        let cancel = CancellationToken::new();
        let set = Self {
            tasks: JoinSet::new(),
            cancel: cancel.clone(),
        };
        (set, cancel)
    }

    pub(crate) fn spawn(&mut self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let cancel = self.cancel.clone();
        self.tasks.spawn(async move {
            future.await;
            if !cancel.is_cancelled() {
                warn!("mesh background task exited; cancelling task set");
                cancel.cancel();
            }
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unexpected_task_exit_cancels_task_set() {
        let (mut task_set, cancel) = TaskSet::new();

        task_set.spawn(async {});

        tokio::time::timeout(std::time::Duration::from_secs(1), cancel.cancelled())
            .await
            .expect("task set should cancel when a task exits unexpectedly");
        task_set.stop().await.expect("task set should stop cleanly");
    }
}
