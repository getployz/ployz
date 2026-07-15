//! Owned background tasks with an admission fence and bounded shutdown drain.

use std::sync::{Arc, Mutex, Weak};
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

/// Sole strong owner of a supervised task set.
#[derive(Debug)]
pub struct TaskRegistry {
    state: Arc<Mutex<TaskRegistryState>>,
}

/// Admission handle that cannot keep its task supervisor alive.
#[derive(Debug, Clone)]
pub struct TaskSpawner {
    state: Weak<Mutex<TaskRegistryState>>,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(TaskRegistryState::accepting())),
        }
    }
}

impl TaskRegistry {
    #[must_use]
    pub fn spawner(&self) -> TaskSpawner {
        TaskSpawner {
            state: Arc::downgrade(&self.state),
        }
    }

    pub fn spawn<Build, Future>(&self, build: Build) -> Result<(), TaskAdmissionError>
    where
        Build: FnOnce() -> Future,
        Future: std::future::Future<Output = ()> + Send + 'static,
    {
        spawn(&self.state, build)
    }

    fn close_and_abort(&self) -> JoinSet<()> {
        let mut state = self
            .state
            .lock()
            .expect("task registry lock is not poisoned");
        state.accepting = false;
        let mut handles = std::mem::take(&mut state.handles);
        handles.abort_all();
        handles
    }

    pub async fn abort_and_join(&self, timeout: Duration) -> Result<(), TaskRegistryQuiesceError> {
        let mut handles = self.close_and_abort();
        tokio::time::timeout(timeout, async {
            while handles.join_next().await.is_some() {}
        })
        .await
        .map_err(|_| TaskRegistryQuiesceError { timeout })
    }
}

impl TaskSpawner {
    pub fn spawn<Build, Future>(&self, build: Build) -> Result<(), TaskAdmissionError>
    where
        Build: FnOnce() -> Future,
        Future: std::future::Future<Output = ()> + Send + 'static,
    {
        let Some(state) = self.state.upgrade() else {
            return Err(TaskAdmissionError::SupervisorStopped);
        };
        spawn(&state, build)
    }
}

fn spawn<Build, Future>(
    state: &Mutex<TaskRegistryState>,
    build: Build,
) -> Result<(), TaskAdmissionError>
where
    Build: FnOnce() -> Future,
    Future: std::future::Future<Output = ()> + Send + 'static,
{
    let mut state = state.lock().expect("task registry lock is not poisoned");
    if !state.accepting {
        return Err(TaskAdmissionError::Quiescing);
    }
    state.handles.spawn(build());
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TaskAdmissionError {
    #[error("task registry is quiescing")]
    Quiescing,
    #[error("task supervisor has stopped")]
    SupervisorStopped,
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
    async fn abort_and_join_cancels_running_tasks_and_stops_admission() {
        let registry = TaskRegistry::default();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        registry
            .spawn(|| async move {
                let _drop_signal = DropSignal(Some(dropped_tx));
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
            })
            .expect("worker admits");
        started_rx.await.expect("worker starts");

        registry
            .abort_and_join(Duration::from_secs(1))
            .await
            .expect("worker quiesces");
        dropped_rx.await.expect("worker future is dropped");

        let built = Arc::new(AtomicBool::new(false));
        let built_by_builder = built.clone();
        let admitted = Arc::new(AtomicBool::new(false));
        let admitted_by_task = admitted.clone();
        let admission = registry.spawn(|| {
            built_by_builder.store(true, Ordering::SeqCst);
            async move {
                admitted_by_task.store(true, Ordering::SeqCst);
            }
        });
        assert_eq!(admission, Err(TaskAdmissionError::Quiescing));
        assert!(!built.load(Ordering::SeqCst));
        tokio::task::yield_now().await;
        assert!(!admitted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn failed_start_drops_supervisor_and_cancels_self_referencing_worker() {
        let registry = TaskRegistry::default();
        let spawner = registry.spawner();
        let worker_spawner = spawner.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        spawner
            .spawn(|| async move {
                let _worker_spawner = worker_spawner;
                let _drop_signal = DropSignal(Some(dropped_tx));
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
            })
            .expect("startup worker admits");
        started_rx.await.expect("startup worker starts");

        drop(registry);

        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("failed startup cancels worker")
            .expect("worker future drops");
        assert_eq!(
            spawner.spawn(|| async {}),
            Err(TaskAdmissionError::SupervisorStopped)
        );
    }
}
