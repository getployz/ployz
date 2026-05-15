use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComponentHealth {
    pub updated_at_unix_secs: u64,
    pub state: ComponentHealthState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ComponentHealthState {
    Healthy,
    Stale {
        stale_since_unix_secs: u64,
        consecutive_failures: u64,
        last_error: String,
    },
}

impl ComponentHealth {
    #[must_use]
    pub fn healthy(updated_at_unix_secs: u64) -> Self {
        Self {
            updated_at_unix_secs,
            state: ComponentHealthState::Healthy,
        }
    }

    #[must_use]
    pub fn stale(
        updated_at_unix_secs: u64,
        previous: Option<&Self>,
        last_error: impl Into<String>,
    ) -> Self {
        let (stale_since_unix_secs, consecutive_failures) = match previous {
            Some(Self {
                state:
                    ComponentHealthState::Stale {
                        stale_since_unix_secs,
                        consecutive_failures,
                        ..
                    },
                ..
            }) => (
                *stale_since_unix_secs,
                consecutive_failures.saturating_add(1),
            ),
            _ => (updated_at_unix_secs, 1),
        };
        Self {
            updated_at_unix_secs,
            state: ComponentHealthState::Stale {
                stale_since_unix_secs,
                consecutive_failures,
                last_error: last_error.into(),
            },
        }
    }
}

#[derive(Debug)]
pub struct FileHealthRecorder {
    path: PathBuf,
    state: AsyncMutex<Option<ComponentHealth>>,
}

impl FileHealthRecorder {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            state: AsyncMutex::new(None),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn force_healthy(&self) -> std::io::Result<()> {
        write_component_health(&self.path, &healthy_component_health()).await?;
        let mut state = self.state.lock().await;
        *state = None;
        Ok(())
    }

    pub async fn record_healthy_if_stale(&self) -> std::io::Result<()> {
        let mut state = self.state.lock().await;
        if state.is_none() {
            return Ok(());
        }
        write_component_health(&self.path, &healthy_component_health()).await?;
        *state = None;
        Ok(())
    }

    pub async fn record_unhealthy(&self, error: impl Into<String>) -> std::io::Result<()> {
        let mut state = self.state.lock().await;
        let next = ComponentHealth::stale(now_unix_secs(), state.as_ref(), error);
        write_component_health(&self.path, &next).await?;
        *state = Some(next.clone());
        Ok(())
    }

    #[cfg(test)]
    async fn health_for_tests(&self) -> Option<ComponentHealth> {
        self.state.lock().await.clone()
    }
}

#[must_use]
pub fn healthy_component_health() -> ComponentHealth {
    ComponentHealth::healthy(now_unix_secs())
}

pub async fn load_component_health(path: impl AsRef<Path>) -> std::io::Result<ComponentHealth> {
    let bytes = tokio::fs::read(path).await?;
    decode_component_health(&bytes)
}

pub async fn write_component_health(
    path: impl AsRef<Path>,
    health: &ComponentHealth,
) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let payload = serde_json::to_vec_pretty(health)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    tokio::fs::write(path, payload).await
}

pub fn load_component_health_sync(path: impl AsRef<Path>) -> std::io::Result<ComponentHealth> {
    let bytes = std::fs::read(path)?;
    decode_component_health(&bytes)
}

pub fn write_component_health_sync(
    path: impl AsRef<Path>,
    health: &ComponentHealth,
) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_vec_pretty(health)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(path, payload)
}

fn decode_component_health(bytes: &[u8]) -> std::io::Result<ComponentHealth> {
    serde_json::from_slice(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

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

    #[test]
    fn component_health_preserves_original_stale_since() {
        let first = ComponentHealth::stale(100, None, "first");
        let second = ComponentHealth::stale(200, Some(&first), "second");

        let ComponentHealthState::Stale {
            stale_since_unix_secs,
            consecutive_failures,
            last_error,
        } = second.state
        else {
            panic!("expected stale health");
        };
        assert_eq!(stale_since_unix_secs, 100);
        assert_eq!(consecutive_failures, 2);
        assert_eq!(last_error, "second");
    }

    #[tokio::test]
    async fn file_health_recorder_clears_stale_health_after_success() {
        let path = temp_path("file-health-recorder").join("health.json");
        let recorder = FileHealthRecorder::new(path.clone());

        recorder.force_healthy().await.expect("record healthy");
        recorder
            .record_unhealthy("subscription closed")
            .await
            .expect("record stale");
        let stale = load_component_health(&path).await.expect("load stale");
        assert!(matches!(stale.state, ComponentHealthState::Stale { .. }));

        recorder
            .record_healthy_if_stale()
            .await
            .expect("record recovered");
        let healthy = load_component_health(path).await.expect("load healthy");
        assert_eq!(healthy.state, ComponentHealthState::Healthy);
    }

    #[tokio::test]
    async fn file_health_recorder_keeps_stale_memory_when_recovery_write_fails() {
        let root = temp_path("file-health-recorder-recovery-fail");
        std::fs::create_dir_all(&root).expect("create root");
        let blocked_parent = root.join("not-a-directory");
        std::fs::write(&blocked_parent, b"file").expect("write blocker");
        let recorder = FileHealthRecorder::new(root.join("health.json"));

        recorder
            .record_unhealthy("subscription closed")
            .await
            .expect("record stale");
        let bad_path = FileHealthRecorder::new(blocked_parent.join("health.json"));
        let stale = recorder.health_for_tests().await.expect("stale");
        *bad_path.state.lock().await = Some(stale.clone());

        assert!(bad_path.record_healthy_if_stale().await.is_err());

        assert_eq!(bad_path.health_for_tests().await, Some(stale));
    }

    #[tokio::test]
    async fn file_health_recorder_does_not_count_failed_unhealthy_write() {
        let root = temp_path("file-health-recorder-unhealthy-fail");
        std::fs::create_dir_all(&root).expect("create root");
        let blocked_parent = root.join("not-a-directory");
        std::fs::write(&blocked_parent, b"file").expect("write blocker");
        let recorder = FileHealthRecorder::new(blocked_parent.join("health.json"));

        assert!(recorder.record_unhealthy("first").await.is_err());

        assert_eq!(recorder.health_for_tests().await, None);
    }

    #[test]
    fn sync_health_io_roundtrips() {
        let path = temp_path("sync-health-io").join("health.json");
        let health = ComponentHealth::stale(100, None, "watch failed");

        write_component_health_sync(&path, &health).expect("write health");

        assert_eq!(
            load_component_health_sync(path).expect("load health"),
            health
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

    fn temp_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }
}
