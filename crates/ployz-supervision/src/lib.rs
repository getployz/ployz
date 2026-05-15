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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComponentId(String);

impl ComponentId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ComponentId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ComponentId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedComponentHealth {
    pub name: String,
    pub updated_at_unix_secs: u64,
    pub state: ComponentHealthState,
}

impl NamedComponentHealth {
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self.state, ComponentHealthState::Healthy)
    }

    #[must_use]
    pub fn stale_since_unix_secs(&self) -> Option<u64> {
        match self.state {
            ComponentHealthState::Healthy => None,
            ComponentHealthState::Stale {
                stale_since_unix_secs,
                ..
            } => Some(stale_since_unix_secs),
        }
    }

    #[must_use]
    pub fn consecutive_failures(&self) -> u64 {
        match self.state {
            ComponentHealthState::Healthy => 0,
            ComponentHealthState::Stale {
                consecutive_failures,
                ..
            } => consecutive_failures,
        }
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        match self.state {
            ComponentHealthState::Healthy => None,
            ComponentHealthState::Stale { ref last_error, .. } => Some(last_error),
        }
    }

    fn as_component_health(&self) -> ComponentHealth {
        ComponentHealth {
            updated_at_unix_secs: self.updated_at_unix_secs,
            state: self.state.clone(),
        }
    }

    fn from_component_health(id: &ComponentId, health: ComponentHealth) -> Self {
        Self {
            name: id.as_str().to_string(),
            updated_at_unix_secs: health.updated_at_unix_secs,
            state: health.state,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ComponentHealthRegistry {
    components: Arc<Mutex<BTreeMap<ComponentId, NamedComponentHealth>>>,
}

impl ComponentHealthRegistry {
    pub fn mark_healthy(&self, id: impl Into<ComponentId>) {
        let id = id.into();
        let health = healthy_component_health();
        self.components
            .lock()
            .expect("component health registry")
            .insert(
                id.clone(),
                NamedComponentHealth::from_component_health(&id, health),
            );
    }

    pub fn mark_stale(&self, id: impl Into<ComponentId>, error: impl Into<String>) {
        let id = id.into();
        let mut components = self.components.lock().expect("component health registry");
        let previous = components
            .get(&id)
            .map(NamedComponentHealth::as_component_health);
        let health = stale_component_health(previous.as_ref(), error);
        components.insert(
            id.clone(),
            NamedComponentHealth::from_component_health(&id, health),
        );
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<NamedComponentHealth> {
        self.components
            .lock()
            .expect("component health registry")
            .values()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn get(&self, id: &ComponentId) -> Option<NamedComponentHealth> {
        self.components
            .lock()
            .expect("component health registry")
            .get(id)
            .cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    initial_delay: Duration,
    max_delay: Duration,
    max_attempts: Option<u64>,
}

impl RetryPolicy {
    #[must_use]
    pub fn new(initial_delay: Duration, max_delay: Duration) -> Self {
        Self {
            initial_delay,
            max_delay,
            max_attempts: None,
        }
    }

    #[must_use]
    pub fn with_max_attempts(mut self, max_attempts: u64) -> Self {
        self.max_attempts = Some(max_attempts);
        self
    }

    #[must_use]
    pub fn record_failure(
        &self,
        previous: Option<&RetryState>,
        error: impl Into<String>,
    ) -> RetryDecision {
        let consecutive_failures =
            previous.map_or(1, |state| state.consecutive_failures.saturating_add(1));
        let exhausted = self
            .max_attempts
            .is_some_and(|max_attempts| consecutive_failures >= max_attempts);
        RetryDecision {
            state: RetryState {
                consecutive_failures,
                last_error: error.into(),
            },
            next_delay: self.delay_for_failure(consecutive_failures),
            exhausted,
        }
    }

    #[must_use]
    pub fn delay_for_failure(&self, consecutive_failures: u64) -> Duration {
        let exponent = consecutive_failures.saturating_sub(1).min(31);
        let multiplier = 1_u32 << exponent;
        self.initial_delay
            .saturating_mul(multiplier)
            .min(self.max_delay)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryState {
    pub consecutive_failures: u64,
    pub last_error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryDecision {
    pub state: RetryState,
    pub next_delay: Duration,
    pub exhausted: bool,
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

    pub async fn force_healthy_forgetting_stale(&self) -> std::io::Result<()> {
        let mut state = self.state.lock().await;
        *state = None;
        write_component_health(&self.path, &healthy_component_health()).await
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

    pub async fn record_unhealthy_remembering_write_failure(
        &self,
        error: impl Into<String>,
    ) -> std::io::Result<()> {
        let mut state = self.state.lock().await;
        let next = ComponentHealth::stale(now_unix_secs(), state.as_ref(), error);
        *state = Some(next.clone());
        write_component_health(&self.path, &next).await
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

#[must_use]
pub fn stale_component_health(
    previous: Option<&ComponentHealth>,
    last_error: impl Into<String>,
) -> ComponentHealth {
    ComponentHealth::stale(now_unix_secs(), previous, last_error)
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

    #[test]
    fn stale_component_health_preserves_previous_stale_state() {
        let first = stale_component_health(None, "first");
        let second = stale_component_health(Some(&first), "second");

        let ComponentHealthState::Stale {
            stale_since_unix_secs,
            consecutive_failures,
            last_error,
        } = second.state
        else {
            panic!("expected stale health");
        };
        assert_eq!(stale_since_unix_secs, first.updated_at_unix_secs);
        assert_eq!(consecutive_failures, 2);
        assert_eq!(last_error, "second");
    }

    #[test]
    fn component_health_registry_marks_component_healthy() {
        let registry = ComponentHealthRegistry::default();

        registry.mark_healthy("mesh_peer_sync");

        let health = registry
            .get(&ComponentId::from("mesh_peer_sync"))
            .expect("component health");
        assert_eq!(health.name, "mesh_peer_sync");
        assert_eq!(health.state, ComponentHealthState::Healthy);
        assert_eq!(registry.snapshot(), vec![health]);
    }

    #[test]
    fn component_health_registry_preserves_stale_since_and_counts_failures() {
        let registry = ComponentHealthRegistry::default();

        registry.mark_stale("mesh_peer_sync", "first");
        let first = registry
            .get(&ComponentId::from("mesh_peer_sync"))
            .expect("first stale health");
        registry.mark_stale("mesh_peer_sync", "second");

        let second = registry
            .get(&ComponentId::from("mesh_peer_sync"))
            .expect("second stale health");
        let ComponentHealthState::Stale {
            stale_since_unix_secs,
            consecutive_failures,
            last_error,
        } = second.state
        else {
            panic!("expected stale health");
        };
        assert_eq!(stale_since_unix_secs, first.updated_at_unix_secs);
        assert_eq!(consecutive_failures, 2);
        assert_eq!(last_error, "second");
    }

    #[test]
    fn retry_policy_backs_off_to_cap_and_preserves_last_failure() {
        let policy = RetryPolicy::new(Duration::from_secs(1), Duration::from_secs(5));

        let first = policy.record_failure(None, "subscribe failed");
        let second = policy.record_failure(Some(&first.state), "publish failed");
        let third = policy.record_failure(Some(&second.state), "flush failed");
        let fourth = policy.record_failure(Some(&third.state), "still down");

        assert_eq!(first.next_delay, Duration::from_secs(1));
        assert_eq!(second.next_delay, Duration::from_secs(2));
        assert_eq!(third.next_delay, Duration::from_secs(4));
        assert_eq!(fourth.next_delay, Duration::from_secs(5));
        assert_eq!(fourth.state.consecutive_failures, 4);
        assert_eq!(fourth.state.last_error, "still down");
        assert!(!fourth.exhausted);
    }

    #[test]
    fn retry_policy_marks_attempt_limit_exhausted() {
        let policy =
            RetryPolicy::new(Duration::from_secs(1), Duration::from_secs(30)).with_max_attempts(2);

        let first = policy.record_failure(None, "first");
        let second = policy.record_failure(Some(&first.state), "second");

        assert!(!first.exhausted);
        assert!(second.exhausted);
        assert_eq!(second.state.last_error, "second");
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
    async fn file_health_recorder_can_forget_stale_when_recovery_write_fails() {
        let root = temp_path("file-health-recorder-forget-recovery-fail");
        std::fs::create_dir_all(&root).expect("create root");
        let blocked_parent = root.join("not-a-directory");
        std::fs::write(&blocked_parent, b"file").expect("write blocker");
        let recorder = FileHealthRecorder::new(root.join("health.json"));

        recorder
            .record_unhealthy("subscription closed")
            .await
            .expect("record stale");
        let bad_path = FileHealthRecorder::new(blocked_parent.join("health.json"));
        *bad_path.state.lock().await = recorder.health_for_tests().await;

        assert!(bad_path.force_healthy_forgetting_stale().await.is_err());

        assert_eq!(bad_path.health_for_tests().await, None);
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

    #[tokio::test]
    async fn file_health_recorder_can_count_failed_unhealthy_write() {
        let root = temp_path("file-health-recorder-count-unhealthy-fail");
        std::fs::create_dir_all(&root).expect("create root");
        let blocked_parent = root.join("not-a-directory");
        std::fs::write(&blocked_parent, b"file").expect("write blocker");
        let bad_path = FileHealthRecorder::new(blocked_parent.join("health.json"));

        assert!(
            bad_path
                .record_unhealthy_remembering_write_failure("first")
                .await
                .is_err()
        );
        let first = bad_path.health_for_tests().await.expect("first state");
        let recorder = FileHealthRecorder::new(root.join("health.json"));
        *recorder.state.lock().await = Some(first.clone());
        recorder
            .record_unhealthy_remembering_write_failure("second")
            .await
            .expect("record second");
        let second = load_component_health(recorder.path())
            .await
            .expect("load second");

        let ComponentHealthState::Stale {
            stale_since_unix_secs: first_stale_since,
            ..
        } = first.state
        else {
            panic!("first health should be stale");
        };
        let ComponentHealthState::Stale {
            stale_since_unix_secs: second_stale_since,
            consecutive_failures,
            last_error,
        } = second.state
        else {
            panic!("second health should be stale");
        };
        assert_eq!(first_stale_since, second_stale_since);
        assert_eq!(consecutive_failures, 2);
        assert_eq!(last_error, "second");
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
