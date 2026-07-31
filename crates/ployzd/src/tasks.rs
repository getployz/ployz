//! Owned background tasks with an admission fence and bounded shutdown drain.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures_util::FutureExt;
use tokio::task::AbortHandle;
use tracing::Instrument;

type TaskId = u64;

#[derive(Debug)]
struct ActiveTask {
    abort: AbortHandle,
    completion: tokio::sync::oneshot::Receiver<()>,
}

#[derive(Debug)]
struct TaskRegistryState {
    accepting: bool,
    next_task_id: TaskId,
    active: BTreeMap<TaskId, ActiveTask>,
    panicked_tasks: u64,
    last_failure: Option<TaskSupervisorFailure>,
}

impl TaskRegistryState {
    fn accepting() -> Self {
        Self {
            accepting: true,
            next_task_id: 1,
            active: BTreeMap::new(),
            panicked_tasks: 0,
            last_failure: None,
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

/// Read-only task supervisor health that cannot keep the supervisor alive.
#[derive(Debug, Clone)]
pub struct TaskSupervisorHealthReader {
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

    #[must_use]
    pub fn health_reader(&self) -> TaskSupervisorHealthReader {
        TaskSupervisorHealthReader {
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

    #[must_use]
    pub fn health(&self) -> TaskSupervisorHealth {
        let state = self
            .state
            .lock()
            .expect("task registry lock is not poisoned");
        state.health()
    }

    fn close_and_abort(&self) -> Vec<tokio::sync::oneshot::Receiver<()>> {
        let mut state = self
            .state
            .lock()
            .expect("task registry lock is not poisoned");
        state.accepting = false;
        std::mem::take(&mut state.active)
            .into_values()
            .map(|task| {
                task.abort.abort();
                task.completion
            })
            .collect()
    }

    pub async fn abort_and_join(&self, timeout: Duration) -> Result<(), TaskRegistryQuiesceError> {
        let completions = self.close_and_abort();
        tokio::time::timeout(timeout, async {
            for completion in completions {
                let _ = completion.await;
            }
        })
        .await
        .map_err(|_| TaskRegistryQuiesceError::Timeout { timeout })?;
        let health = self.health();
        match health.last_failure {
            Some(failure) => Err(TaskRegistryQuiesceError::TaskPanicked { failure }),
            None => Ok(()),
        }
    }
}

impl Drop for TaskRegistry {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .expect("task registry lock is not poisoned");
        state.accepting = false;
        for task in state.active.values() {
            task.abort.abort();
        }
    }
}

impl TaskRegistryState {
    fn health(&self) -> TaskSupervisorHealth {
        TaskSupervisorHealth {
            active_tasks: self.active.len(),
            panicked_tasks: self.panicked_tasks,
            last_failure: self.last_failure.clone(),
        }
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

impl TaskSupervisorHealthReader {
    #[must_use]
    pub fn snapshot(&self) -> Option<TaskSupervisorHealth> {
        let state = self.state.upgrade()?;
        let state = state.lock().expect("task registry lock is not poisoned");
        Some(state.health())
    }
}

fn spawn<Build, Future>(
    state: &Arc<Mutex<TaskRegistryState>>,
    build: Build,
) -> Result<(), TaskAdmissionError>
where
    Build: FnOnce() -> Future,
    Future: std::future::Future<Output = ()> + Send + 'static,
{
    let mut registry = state.lock().expect("task registry lock is not poisoned");
    if !registry.accepting {
        return Err(TaskAdmissionError::Quiescing);
    }
    let task_id = registry.next_task_id;
    registry.next_task_id = registry.next_task_id.saturating_add(1);
    let weak_state = Arc::downgrade(state);
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
    let future = build();
    // Spawned tasks inherit the caller's span so operation attribution
    // (`kind`, `operation_id`) survives the spawn: the constant `process`
    // field is stamped by the subscriber, but span fields do not cross
    // `tokio::spawn` on their own.
    let task = tokio::spawn(
        async move {
            let outcome = AssertUnwindSafe(future).catch_unwind().await;
            if let Some(state) = weak_state.upgrade() {
                let mut state = state.lock().expect("task registry lock is not poisoned");
                state.active.remove(&task_id);
                if let Err(payload) = outcome {
                    state.panicked_tasks = state.panicked_tasks.saturating_add(1);
                    state.last_failure = Some(TaskSupervisorFailure {
                        task_id,
                        message: panic_message(payload),
                    });
                }
            }
            let _ = completion_tx.send(());
        }
        .instrument(tracing::Span::current()),
    );
    registry.active.insert(
        task_id,
        ActiveTask {
            abort: task.abort_handle(),
            completion: completion_rx,
        },
    );
    Ok(())
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "task panicked with a non-string payload".to_owned(),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TaskAdmissionError {
    #[error("task registry is quiescing")]
    Quiescing,
    #[error("task supervisor has stopped")]
    SupervisorStopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSupervisorFailure {
    pub task_id: u64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSupervisorHealth {
    pub active_tasks: usize,
    pub panicked_tasks: u64,
    pub last_failure: Option<TaskSupervisorFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskRegistryQuiesceError {
    #[error("task registry did not quiesce within {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("supervised task {} panicked: {}", .failure.task_id, .failure.message)]
    TaskPanicked { failure: TaskSupervisorFailure },
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

    #[tokio::test]
    async fn completed_tasks_are_reaped_continuously() {
        let registry = TaskRegistry::default();
        for _ in 0..1_000 {
            registry.spawn(|| async {}).expect("task admits");
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            while registry.health().active_tasks != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("completed tasks reap");
        assert_eq!(
            registry.health(),
            TaskSupervisorHealth {
                active_tasks: 0,
                panicked_tasks: 0,
                last_failure: None,
            }
        );
    }

    #[tokio::test]
    async fn task_panic_is_retained_in_health_and_shutdown_error() {
        let registry = TaskRegistry::default();
        registry
            .spawn(|| async { panic!("supervised worker exploded") })
            .expect("task admits");
        tokio::time::timeout(Duration::from_secs(1), async {
            while registry.health().panicked_tasks == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("panic reaches supervisor health");

        let health = registry.health();
        assert_eq!(health.active_tasks, 0);
        assert_eq!(health.panicked_tasks, 1);
        assert_eq!(
            health
                .last_failure
                .as_ref()
                .map(|failure| failure.message.as_str()),
            Some("supervised worker exploded")
        );
        assert!(matches!(
            registry.abort_and_join(Duration::from_secs(1)).await,
            Err(TaskRegistryQuiesceError::TaskPanicked { failure })
                if failure.message == "supervised worker exploded"
        ));
    }
}
