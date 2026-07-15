//! Owned background tasks with an admission fence and bounded shutdown drain.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinSet;

#[derive(Debug)]
struct TaskRegistryState {
    accepting: bool,
    handles: JoinSet<()>,
}

impl TaskRegistryState {
    fn accepting() -> Self {
        Self {
            accepting: true,
            handles: JoinSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskRegistry {
    state: Arc<Mutex<TaskRegistryState>>,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(TaskRegistryState::accepting())),
        }
    }
}

impl TaskRegistry {
    pub fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut state = self
            .state
            .lock()
            .expect("task registry lock is not poisoned");
        if state.accepting {
            state.handles.spawn(future);
        }
    }

    fn begin_quiesce(&self) -> JoinSet<()> {
        let mut state = self
            .state
            .lock()
            .expect("task registry lock is not poisoned");
        state.accepting = false;
        let mut handles = std::mem::take(&mut state.handles);
        handles.abort_all();
        handles
    }

    pub async fn quiesce_all(
        registries: &[&Self],
        timeout: Duration,
    ) -> Result<(), TaskRegistryQuiesceError> {
        let mut handles = registries
            .iter()
            .map(|registry| registry.begin_quiesce())
            .collect::<Vec<_>>();
        tokio::time::timeout(timeout, async {
            for registry_handles in &mut handles {
                while registry_handles.join_next().await.is_some() {}
            }
        })
        .await
        .map_err(|_| TaskRegistryQuiesceError { timeout })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("task registry did not quiesce within {timeout:?}")]
pub struct TaskRegistryQuiesceError {
    timeout: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[tokio::test]
    async fn quiesce_drains_running_tasks_and_stops_admission() {
        let registry = TaskRegistry::default();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        registry.spawn(async move {
            let _drop_signal = DropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        started_rx.await.expect("worker starts");

        TaskRegistry::quiesce_all(&[&registry], Duration::from_secs(1))
            .await
            .expect("worker quiesces");
        dropped_rx.await.expect("worker future is dropped");

        let admitted = Arc::new(AtomicBool::new(false));
        let admitted_by_task = admitted.clone();
        registry.spawn(async move {
            admitted_by_task.store(true, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
        assert!(!admitted.load(Ordering::SeqCst));
    }
}
