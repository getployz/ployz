use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Starting,
    Running,
    Succeeded,
    Failed { error: String },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskHealth {
    pub name: String,
    pub state: TaskState,
    pub updated_at_unix_secs: u64,
}

#[derive(Debug, Clone, Default)]
pub struct HealthRegistry {
    tasks: Arc<Mutex<BTreeMap<String, TaskHealth>>>,
}

impl HealthRegistry {
    #[must_use]
    pub fn snapshot(&self) -> Vec<TaskHealth> {
        self.tasks
            .lock()
            .expect("task health")
            .values()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<TaskHealth> {
        self.tasks.lock().expect("task health").get(name).cloned()
    }

    fn set(&self, name: &str, state: TaskState) {
        self.tasks.lock().expect("task health").insert(
            name.to_string(),
            TaskHealth {
                name: name.to_string(),
                state,
                updated_at_unix_secs: now_unix_secs(),
            },
        );
    }
}

#[derive(Debug, Clone)]
pub struct Supervisor {
    health: HealthRegistry,
    shutdown: CancellationToken,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            health: HealthRegistry::default(),
            shutdown: CancellationToken::new(),
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[must_use]
    pub fn health(&self) -> HealthRegistry {
        self.health.clone()
    }

    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub fn spawn<F>(&self, name: impl Into<String>, future: F)
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        let name = name.into();
        self.health.set(&name, TaskState::Starting);
        let health = self.health.clone();
        let shutdown = self.shutdown.clone();
        let task_name = name.clone();
        let handle = tokio::spawn(async move {
            health.set(&task_name, TaskState::Running);
            let result = future.await;
            let state = match result {
                Ok(()) if shutdown.is_cancelled() => TaskState::Cancelled,
                Ok(()) => TaskState::Succeeded,
                Err(error) => TaskState::Failed { error },
            };
            health.set(&task_name, state);
        });
        self.tasks.lock().expect("supervisor tasks").push(handle);
    }

    pub async fn shutdown(self, deadline: Duration) -> Result<(), String> {
        self.shutdown.cancel();
        let handles = {
            let mut guard = self.tasks.lock().expect("supervisor tasks");
            std::mem::take(&mut *guard)
        };
        let wait = async move {
            for handle in handles {
                handle
                    .await
                    .map_err(|error| format!("supervised task join failed: {error}"))?;
            }
            Ok(())
        };
        tokio::time::timeout(deadline, wait).await.map_err(|_| {
            format!(
                "supervisor shutdown timed out after {}s",
                deadline.as_secs()
            )
        })?
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_successful_task_health() {
        let supervisor = Supervisor::new();
        let health = supervisor.health();

        supervisor.spawn("success", async { Ok(()) });
        wait_for_terminal_state(&health, "success").await;
        supervisor
            .shutdown(Duration::from_secs(1))
            .await
            .expect("shutdown");

        assert_eq!(
            health.get("success").expect("task").state,
            TaskState::Succeeded
        );
    }

    #[tokio::test]
    async fn records_failed_task_health() {
        let supervisor = Supervisor::new();
        let health = supervisor.health();

        supervisor.spawn("failure", async { Err("boom".to_string()) });
        wait_for_terminal_state(&health, "failure").await;
        supervisor
            .shutdown(Duration::from_secs(1))
            .await
            .expect("shutdown");

        assert_eq!(
            health.get("failure").expect("task").state,
            TaskState::Failed {
                error: "boom".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn shutdown_waits_for_task_cleanup_after_cancellation() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let supervisor = Supervisor::new();
        let token = supervisor.shutdown_token();
        let health = supervisor.health();
        let cleaned = Arc::new(AtomicBool::new(false));
        let task_cleaned = cleaned.clone();

        supervisor.spawn("cleanup", async move {
            token.cancelled().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
            task_cleaned.store(true, Ordering::SeqCst);
            Ok(())
        });

        supervisor
            .shutdown(Duration::from_secs(1))
            .await
            .expect("shutdown");

        assert!(cleaned.load(Ordering::SeqCst));
        assert_eq!(
            health.get("cleanup").expect("task").state,
            TaskState::Cancelled
        );
    }

    async fn wait_for_terminal_state(health: &HealthRegistry, name: &str) {
        for _ in 0..20 {
            if health.get(name).is_some_and(|task| {
                matches!(
                    task.state,
                    TaskState::Succeeded | TaskState::Failed { .. } | TaskState::Cancelled
                )
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
