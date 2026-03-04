mod peer_sync;

pub(crate) use peer_sync::run_peer_sync_task;

use crate::dataplane::traits::PortError;
use thiserror::Error;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::warn;

#[derive(Debug, Error)]
pub enum TaskSetError {
    #[error("task subscribe failed: {0}")]
    Subscribe(PortError),
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
