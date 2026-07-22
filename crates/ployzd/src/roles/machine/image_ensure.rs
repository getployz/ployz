use super::execution::docker::runner::DockerManagedContainerRunner;
use ployz_core::deploy::ImageReference;
use ployz_core::image::{
    ImageEnsureFailure, ImageEnsureSource, ImageEnsureStatus, ImageRpcDomainError,
};
use ployz_core::machine::runtime::ManagedContainerIdentity;
use ployz_core::operation::FailureMessage;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;

const IMAGE_PULL_STALL_TIMEOUT: Duration = Duration::from_secs(30);
const IMAGE_ENSURE_TERMINAL_RETENTION: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImagePullProgress {
    pub(crate) layer: String,
    pub(crate) state: String,
    pub(crate) current_bytes: u64,
}

struct ImageEnsureTask {
    source_fingerprint: [u8; 32],
    status: Mutex<ImageEnsureStatus>,
    cancel: watch::Sender<bool>,
    supervisor: Mutex<Option<JoinHandle<()>>>,
    terminal_at: Mutex<Option<tokio::time::Instant>>,
}

impl ImageEnsureTask {
    async fn terminalize(&self, candidate: ImageEnsureStatus) {
        let mut status = self.status.lock().await;
        if !matches!(
            *status,
            ImageEnsureStatus::Completed { .. }
                | ImageEnsureStatus::Failed { .. }
                | ImageEnsureStatus::Cancelled
        ) {
            *status = candidate;
            *self.terminal_at.lock().await = Some(tokio::time::Instant::now());
        }
    }

    async fn record_progress(&self, sequence: u64) -> bool {
        let mut status = self.status.lock().await;
        if matches!(
            *status,
            ImageEnsureStatus::Completed { .. }
                | ImageEnsureStatus::Failed { .. }
                | ImageEnsureStatus::Cancelled
        ) {
            return false;
        }
        *status = ImageEnsureStatus::Running {
            progress_sequence: sequence,
        };
        true
    }
}

struct ImageEnsureState {
    accepting: bool,
    tasks: HashMap<ManagedContainerIdentity, Arc<ImageEnsureTask>>,
}

#[derive(Clone)]
pub(crate) struct ImageEnsureRuntime {
    machine: ImageEnsureMachine,
    state: Arc<Mutex<ImageEnsureState>>,
}

impl ImageEnsureRuntime {
    #[must_use]
    pub(crate) fn new(machine: DockerManagedContainerRunner) -> Self {
        Self {
            machine: ImageEnsureMachine::Docker(machine),
            state: Arc::new(Mutex::new(ImageEnsureState {
                accepting: true,
                tasks: HashMap::new(),
            })),
        }
    }

    pub(crate) async fn start(
        &self,
        owner: ManagedContainerIdentity,
        source: ImageEnsureSource,
    ) -> Result<ImageEnsureStatus, ImageRpcDomainError> {
        let fingerprint = source_fingerprint(&source);
        let mut state = self.state.lock().await;
        sweep_expired(&mut state).await;
        if !state.accepting {
            return Err(ImageRpcDomainError::ServiceUnavailable {
                message: failure_message("image ensure runtime is shutting down"),
            });
        }
        if let Some(existing) = state.tasks.get(&owner) {
            if existing.source_fingerprint != fingerprint {
                return Err(ImageRpcDomainError::ImageEnsureConflict { owner });
            }
            return Ok(existing.status.lock().await.clone());
        }
        let (cancel, cancel_rx) = watch::channel(false);
        let task = Arc::new(ImageEnsureTask {
            source_fingerprint: fingerprint,
            status: Mutex::new(ImageEnsureStatus::Accepted),
            cancel,
            supervisor: Mutex::new(None),
            terminal_at: Mutex::new(None),
        });
        state.tasks.insert(owner, Arc::clone(&task));
        let machine = self.machine.clone();
        let task_for_supervisor = Arc::clone(&task);
        let supervisor = tokio::spawn(async move {
            supervise_pull(machine, task_for_supervisor, source, cancel_rx).await;
        });
        *task.supervisor.lock().await = Some(supervisor);
        drop(state);
        Ok(ImageEnsureStatus::Accepted)
    }

    pub(crate) async fn status(
        &self,
        owner: &ManagedContainerIdentity,
    ) -> Result<ImageEnsureStatus, ImageRpcDomainError> {
        let task = {
            let mut state = self.state.lock().await;
            sweep_expired(&mut state).await;
            state.tasks.get(owner).cloned()
        }
        .ok_or_else(|| ImageRpcDomainError::ImageEnsureNotFound {
            owner: owner.clone(),
        })?;
        Ok(task.status.lock().await.clone())
    }

    pub(crate) async fn cancel(
        &self,
        owner: &ManagedContainerIdentity,
    ) -> Result<ImageEnsureStatus, ImageRpcDomainError> {
        let task = {
            let mut state = self.state.lock().await;
            sweep_expired(&mut state).await;
            state.tasks.get(owner).cloned()
        }
        .ok_or_else(|| ImageRpcDomainError::ImageEnsureNotFound {
            owner: owner.clone(),
        })?;
        let _ = task.cancel.send(true);
        task.terminalize(ImageEnsureStatus::Cancelled).await;
        Ok(task.status.lock().await.clone())
    }

    pub(crate) async fn shutdown(self) {
        let tasks = {
            let mut state = self.state.lock().await;
            state.accepting = false;
            state.tasks.values().cloned().collect::<Vec<_>>()
        };
        for task in &tasks {
            let _ = task.cancel.send(true);
            task.terminalize(ImageEnsureStatus::Cancelled).await;
        }
        for task in tasks {
            if let Some(supervisor) = task.supervisor.lock().await.take() {
                let _ = supervisor.await;
            }
        }
    }
}

#[derive(Clone)]
enum ImageEnsureMachine {
    Docker(DockerManagedContainerRunner),
    #[cfg(test)]
    Test(TestImageEnsureMachine),
}

impl ImageEnsureMachine {
    async fn ensure_image(
        &self,
        source: &ImageEnsureSource,
        progress: mpsc::UnboundedSender<ImagePullProgress>,
    ) -> Result<(), String> {
        match self {
            Self::Docker(machine) => machine.ensure_machine_image(source, progress).await,
            #[cfg(test)]
            Self::Test(machine) => machine.ensure_image(progress).await,
        }
    }
}

async fn supervise_pull(
    machine: ImageEnsureMachine,
    task: Arc<ImageEnsureTask>,
    source: ImageEnsureSource,
    mut cancel: watch::Receiver<bool>,
) {
    let reference = source.reference();
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    let pull = machine.ensure_image(&source, progress_tx);
    tokio::pin!(pull);
    let stall = tokio::time::sleep(IMAGE_PULL_STALL_TIMEOUT);
    tokio::pin!(stall);
    let mut sequence = 0_u64;
    let mut observed = HashMap::<String, (String, u64)>::new();
    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() { task.terminalize(ImageEnsureStatus::Cancelled).await; return; }
            }
            result = &mut pull => {
                let status = match result {
                    Ok(()) => match ImageReference::try_new(reference) {
                        Ok(reference) => ImageEnsureStatus::Completed { reference },
                        Err(error) => ImageEnsureStatus::Failed { failure: ImageEnsureFailure::PullFailed { message: failure_message(error.to_string()) } },
                    },
                    Err(message) => ImageEnsureStatus::Failed { failure: ImageEnsureFailure::PullFailed { message: failure_message(message) } },
                };
                task.terminalize(status).await;
                return;
            }
            progress = progress_rx.recv() => {
                let Some(progress) = progress else { continue };
                let verified = observed.get(&progress.layer).is_none_or(|(state, current)| state != &progress.state || progress.current_bytes > *current);
                if verified {
                    observed.insert(progress.layer, (progress.state, progress.current_bytes));
                    sequence = sequence.saturating_add(1);
                    if task.record_progress(sequence).await {
                        stall.as_mut().reset(tokio::time::Instant::now() + IMAGE_PULL_STALL_TIMEOUT);
                    }
                }
            }
            () = &mut stall => {
                task.terminalize(ImageEnsureStatus::Failed { failure: ImageEnsureFailure::Stalled { timeout_millis: IMAGE_PULL_STALL_TIMEOUT.as_millis() as u64 } }).await;
                return;
            }
        }
    }
}

fn source_fingerprint(source: &ImageEnsureSource) -> [u8; 32] {
    Sha256::digest(serde_json::to_vec(source).expect("image ensure source serializes")).into()
}

async fn sweep_expired(state: &mut ImageEnsureState) {
    let now = tokio::time::Instant::now();
    let entries = state
        .tasks
        .iter()
        .map(|(owner, task)| (owner.clone(), Arc::clone(task)))
        .collect::<Vec<_>>();
    for (owner, task) in entries {
        if task
            .terminal_at
            .lock()
            .await
            .is_some_and(|at| now.duration_since(at) >= IMAGE_ENSURE_TERMINAL_RETENTION)
        {
            state.tasks.remove(&owner);
        }
    }
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message.into()).expect("image ensure failure message is non-empty")
}

#[cfg(test)]
#[derive(Clone)]
struct TestImageEnsureMachine {
    behavior: TestPullBehavior,
    starts: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
#[derive(Clone)]
enum TestPullBehavior {
    Blocked,
    VerifiedSlow,
    DuplicateFrames,
    Complete,
    Fail,
    WaitForRelease(Arc<tokio::sync::Notify>),
    TransientRetry,
}

#[cfg(test)]
impl TestImageEnsureMachine {
    async fn ensure_image(
        &self,
        progress: mpsc::UnboundedSender<ImagePullProgress>,
    ) -> Result<(), String> {
        self.starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let frame = |current_bytes| ImagePullProgress {
            layer: "layer-a".to_owned(),
            state: "downloading".to_owned(),
            current_bytes,
        };
        match &self.behavior {
            TestPullBehavior::Blocked => std::future::pending().await,
            TestPullBehavior::VerifiedSlow => {
                let _ = progress.send(frame(1));
                tokio::time::sleep(Duration::from_secs(20)).await;
                let _ = progress.send(frame(2));
                tokio::time::sleep(Duration::from_secs(15)).await;
                let _ = progress.send(frame(2));
                tokio::time::sleep(Duration::from_secs(5)).await;
                let _ = progress.send(frame(3));
                tokio::time::sleep(Duration::from_secs(15)).await;
                Ok(())
            }
            TestPullBehavior::DuplicateFrames => {
                let _ = progress.send(frame(1));
                tokio::time::sleep(Duration::from_secs(20)).await;
                let _ = progress.send(frame(1));
                std::future::pending().await
            }
            TestPullBehavior::Complete => Ok(()),
            TestPullBehavior::Fail => Err("registry rejected the image".to_owned()),
            TestPullBehavior::WaitForRelease(release) => {
                release.notified().await;
                Ok(())
            }
            TestPullBehavior::TransientRetry => loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::{NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId};
    use ployz_core::machine::runtime::ManagedContainerKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn owner() -> ManagedContainerIdentity {
        ManagedContainerIdentity {
            namespace_id: NamespaceId::try_new("default").expect("namespace"),
            service_id: ServiceId::try_new("api").expect("service"),
            namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("entry-a")
                .expect("entry"),
            operation_id: OperationId::try_new("operation_a").expect("operation"),
            step_id: StepId::try_new("run_1").expect("step"),
            kind: ManagedContainerKind::Service,
        }
    }

    fn source(tag: &str) -> ImageEnsureSource {
        ImageEnsureSource::Registry {
            reference: ImageReference::try_new(format!("registry.example/api:{tag}"))
                .expect("image"),
            credential: None,
        }
    }

    fn runtime(behavior: TestPullBehavior) -> (ImageEnsureRuntime, Arc<AtomicUsize>) {
        let starts = Arc::new(AtomicUsize::new(0));
        (
            ImageEnsureRuntime {
                machine: ImageEnsureMachine::Test(TestImageEnsureMachine {
                    behavior,
                    starts: Arc::clone(&starts),
                }),
                state: Arc::new(Mutex::new(ImageEnsureState {
                    accepting: true,
                    tasks: HashMap::new(),
                })),
            },
            starts,
        )
    }

    #[tokio::test]
    async fn start_acknowledges_before_a_blocked_pull_finishes_and_identical_start_spawns_once() {
        let (runtime, starts) = runtime(TestPullBehavior::Blocked);
        assert_eq!(
            runtime
                .start(owner(), source("one"))
                .await
                .expect("accepted"),
            ImageEnsureStatus::Accepted
        );
        tokio::task::yield_now().await;
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(matches!(
            runtime
                .start(owner(), source("one"))
                .await
                .expect("idempotent"),
            ImageEnsureStatus::Accepted | ImageEnsureStatus::Running { .. }
        ));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(matches!(
            runtime.start(owner(), source("two")).await,
            Err(ImageRpcDomainError::ImageEnsureConflict { .. })
        ));
        runtime.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn verified_progress_can_exceed_the_old_total_timeout_and_complete() {
        let (runtime, _) = runtime(TestPullBehavior::VerifiedSlow);
        runtime
            .start(owner(), source("one"))
            .await
            .expect("accepted");
        tokio::task::yield_now().await;
        for seconds in [20, 15, 5, 15, 1] {
            tokio::time::advance(Duration::from_secs(seconds)).await;
            tokio::task::yield_now().await;
        }
        tokio::task::yield_now().await;
        assert!(matches!(
            runtime.status(&owner()).await.expect("status"),
            ImageEnsureStatus::Completed { .. }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_frames_do_not_renew_the_stall_deadline() {
        let (runtime, _) = runtime(TestPullBehavior::DuplicateFrames);
        runtime
            .start(owner(), source("one"))
            .await
            .expect("accepted");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(11)).await;
        tokio::task::yield_now().await;
        assert!(matches!(
            runtime.status(&owner()).await.expect("status"),
            ImageEnsureStatus::Failed {
                failure: ImageEnsureFailure::Stalled { .. }
            }
        ));
    }

    #[tokio::test]
    async fn cancel_is_idempotent() {
        let (runtime, _) = runtime(TestPullBehavior::Blocked);
        runtime
            .start(owner(), source("one"))
            .await
            .expect("accepted");
        assert_eq!(
            runtime.cancel(&owner()).await.expect("cancel"),
            ImageEnsureStatus::Cancelled
        );
        assert_eq!(
            runtime.cancel(&owner()).await.expect("repeat cancel"),
            ImageEnsureStatus::Cancelled
        );
        assert_eq!(
            runtime.status(&owner()).await.expect("status"),
            ImageEnsureStatus::Cancelled
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_pull_failure_is_retained_and_queryable() {
        let (runtime, _) = runtime(TestPullBehavior::Fail);
        runtime
            .start(owner(), source("one"))
            .await
            .expect("accepted");
        tokio::task::yield_now().await;
        let first = runtime.status(&owner()).await.expect("terminal failure");
        assert!(
            matches!(&first, ImageEnsureStatus::Failed { failure: ImageEnsureFailure::PullFailed { message } } if message.as_str() == "registry rejected the image")
        );
        assert_eq!(
            runtime.status(&owner()).await.expect("retained failure"),
            first
        );
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_transient_retries_without_progress_stall() {
        let (runtime, _) = runtime(TestPullBehavior::TransientRetry);
        runtime
            .start(owner(), source("one"))
            .await
            .expect("accepted");
        tokio::task::yield_now().await;
        for _ in 0..7 {
            tokio::time::advance(Duration::from_secs(5)).await;
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            runtime.status(&owner()).await.expect("status"),
            ImageEnsureStatus::Failed {
                failure: ImageEnsureFailure::Stalled { .. }
            }
        ));
    }

    #[tokio::test]
    async fn cancel_and_completion_race_has_one_stable_terminal_winner() {
        let release = Arc::new(tokio::sync::Notify::new());
        let (runtime, _) = runtime(TestPullBehavior::WaitForRelease(Arc::clone(&release)));
        runtime
            .start(owner(), source("one"))
            .await
            .expect("accepted");
        tokio::task::yield_now().await;
        release.notify_one();
        let cancelled = runtime.cancel(&owner()).await.expect("cancel outcome");
        tokio::task::yield_now().await;
        assert_eq!(
            runtime.status(&owner()).await.expect("stable terminal"),
            cancelled
        );
        assert!(matches!(
            cancelled,
            ImageEnsureStatus::Completed { .. } | ImageEnsureStatus::Cancelled
        ));
    }

    #[tokio::test]
    async fn shutdown_cancels_and_joins_active_supervisor() {
        let (runtime, _) = runtime(TestPullBehavior::Blocked);
        let observer = runtime.clone();
        runtime
            .start(owner(), source("one"))
            .await
            .expect("accepted");
        runtime.shutdown().await;
        assert_eq!(
            observer.status(&owner()).await.expect("terminal status"),
            ImageEnsureStatus::Cancelled
        );
        let task = observer
            .state
            .lock()
            .await
            .tasks
            .get(&owner())
            .cloned()
            .expect("task retained");
        assert!(task.supervisor.lock().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn expired_terminal_tombstone_is_swept_opportunistically() {
        let (runtime, starts) = runtime(TestPullBehavior::Complete);
        runtime
            .start(owner(), source("one"))
            .await
            .expect("accepted");
        tokio::task::yield_now().await;
        assert!(matches!(
            runtime.status(&owner()).await.expect("retained"),
            ImageEnsureStatus::Completed { .. }
        ));
        tokio::time::advance(IMAGE_ENSURE_TERMINAL_RETENTION + Duration::from_secs(1)).await;
        assert!(matches!(
            runtime.status(&owner()).await,
            Err(ImageRpcDomainError::ImageEnsureNotFound { .. })
        ));
        runtime
            .start(owner(), source("one"))
            .await
            .expect("new attempt");
        tokio::task::yield_now().await;
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }
}
