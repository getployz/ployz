mod ebpf_sync;
mod endpoint_refresh;
mod heartbeat;
mod participation;
mod peer_sync;
mod self_liveness;
mod self_record;
mod subnet_claim_monitor;

pub(crate) use ebpf_sync::run_ebpf_sync_task;
pub(crate) use endpoint_refresh::run_endpoint_refresh_task;
pub use heartbeat::HeartbeatCommand;
pub(crate) use heartbeat::run_heartbeat_task;
pub use participation::ParticipationCommand;
pub(crate) use participation::run_participation_task;
pub use peer_sync::PeerSyncCommand;
pub(crate) use peer_sync::run_peer_sync_task;
pub use self_liveness::SelfLivenessCommand;
pub(crate) use self_liveness::run_self_liveness_task;
pub(crate) use self_record::SelfRecordMutation;
pub(crate) use self_record::SelfRecordCommand;
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
        self.tasks.spawn(future);
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
