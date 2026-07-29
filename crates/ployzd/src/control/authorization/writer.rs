use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::adapters::atomic_file::write_file_atomically;
use crate::control::intent::nats_authorizations::{
    CredentialRemoveStoreOutcome, CredentialUpsertStoreOutcome, CredentialUpsertStoreRejection,
    NatsAuthorizationStore, NatsAuthorizationStoreError,
};
use ployz_core::nats_config::{
    CredentialGrant, CredentialRole, NatsAuthorizationGrant, NatsUserPublicKey,
};
use ployz_nats::connect::{NatsConnectConfig, connect_authenticated};
use ployz_nats::permissions::render_authorized_users;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::Instrument;

use super::expiry::{
    AuthorizationWake, NatsAuthorizationHealthReader, RETRY_SCHEDULE, retry_after_failure,
    schedule_after_success,
};
use super::reload::{
    NatsReloadEvidence, NatsReloadFailure, NatsReloadOutcome, NatsReloadRunner,
    RELOAD_COMMAND_TIMEOUT,
};

const RENDER_QUEUE_DEPTH: usize = 16;
const VERIFY_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const VERIFY_ATTEMPTS: u32 = 10;
const VERIFY_RETRY_DELAY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedAuthorization {
    pub user_count: usize,
    pub reload: Option<NatsReloadEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialMutationChange {
    Added,
    Updated,
    Removed,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialMutationResult {
    pub change: CredentialMutationChange,
    pub authorization: RenderedAuthorization,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialMutationRejection {
    #[error("credential role cannot change from {existing:?} to {requested:?}")]
    RoleMismatch {
        existing: CredentialRole,
        requested: CredentialRole,
    },
    #[error(
        "active Build Executor identity {identity:?} is already granted to {existing_public_key:?}"
    )]
    ActiveBuildExecutorIdentity {
        identity: ployz_core::build::BuildExecutorIdentity,
        existing_public_key: NatsUserPublicKey,
    },
    #[error("the final Operator credential cannot be removed")]
    LastOperator,
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialMutationFailure {
    #[error("credential mutation rejected: {reason}")]
    Rejected { reason: CredentialMutationRejection },
    #[error("credential mutation was not committed: {failure}")]
    NotCommitted { failure: RenderFailure },
    #[error("credential mutation committed but authorization projection failed: {failure}")]
    Committed { failure: RenderFailure },
}

/// Why reading the on-disk authorized-users file failed. The file is only read
/// for the no-op comparison (the store is the source of truth); a missing file is
/// not an error — it reads as absent.
#[derive(Debug, thiserror::Error)]
#[error("failed to read authorized-users file {}: {message}", path.display())]
pub struct AuthorizedUsersFileError {
    path: PathBuf,
    message: String,
}

/// Why a render did not complete, shaped by the pipeline phase that failed.
/// The variant names the progress that preceded it: `Reload` means the file
/// rendered first; `Verify` means render and reload both completed.
#[derive(Debug, thiserror::Error)]
pub enum RenderFailure {
    /// Nothing reached the running server.
    #[error("{failure}")]
    Prepare { failure: RenderPrepareFailure },
    /// The file rendered; the nats-server reload failed.
    #[error("{failure}")]
    Reload { failure: NatsReloadFailure },
    /// Render and reload completed; the minted credential never
    /// authenticated within the bounded verify window.
    #[error("minted credential failed verification: {message}")]
    Verify { message: String },
}

/// A render failure before anything changed on the running server.
#[derive(Debug, thiserror::Error)]
pub enum RenderPrepareFailure {
    /// The single-writer render task is no longer running.
    #[error("authorization render writer task is no longer running")]
    WriterClosed,
    /// The grant store could not be read or written.
    #[error("grant store: {message}")]
    Store { message: String },
    /// The current on-disk file could not be read (for the no-op comparison).
    #[error("{source}")]
    File { source: AuthorizedUsersFileError },
    /// The atomic file write failed.
    #[error("failed to write authorized-users file {}: {message}", path.display())]
    WriteFile { path: PathBuf, message: String },
}

enum AuthorizationRequest {
    Render {
        verify: Option<NatsConnectConfig>,
        reply: oneshot::Sender<Result<RenderedAuthorization, RenderFailure>>,
    },
    Authorize {
        grant: NatsAuthorizationGrant,
        verify: Option<NatsConnectConfig>,
        reply: oneshot::Sender<Result<RenderedAuthorization, RenderFailure>>,
    },
    AddCredential {
        grant: CredentialGrant,
        reply: oneshot::Sender<Result<CredentialMutationResult, CredentialMutationFailure>>,
    },
    RemoveCredential {
        public_key: NatsUserPublicKey,
        reply: oneshot::Sender<Result<CredentialMutationResult, CredentialMutationFailure>>,
    },
}

/// Clonable submitter into the single-writer render task.
#[derive(Debug, Clone)]
pub struct NatsAuthorizationHandle {
    sender: mpsc::Sender<AuthorizationRequest>,
}

impl NatsAuthorizationHandle {
    pub async fn render(
        &self,
        verify: Option<NatsConnectConfig>,
    ) -> Result<RenderedAuthorization, RenderFailure> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(AuthorizationRequest::Render { verify, reply })
            .await
            .map_err(|_| writer_closed())?;
        response.await.map_err(|_| writer_closed())?
    }

    pub async fn authorize_and_render(
        &self,
        grant: NatsAuthorizationGrant,
        verify: Option<NatsConnectConfig>,
    ) -> Result<RenderedAuthorization, RenderFailure> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(AuthorizationRequest::Authorize {
                grant,
                verify,
                reply,
            })
            .await
            .map_err(|_| writer_closed())?;
        response.await.map_err(|_| writer_closed())?
    }

    pub async fn add_credential(
        &self,
        grant: CredentialGrant,
    ) -> Result<CredentialMutationResult, CredentialMutationFailure> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(AuthorizationRequest::AddCredential { grant, reply })
            .await
            .map_err(|_| CredentialMutationFailure::NotCommitted {
                failure: writer_closed(),
            })?;
        response
            .await
            .map_err(|_| CredentialMutationFailure::NotCommitted {
                failure: writer_closed(),
            })?
    }

    pub async fn remove_credential(
        &self,
        public_key: NatsUserPublicKey,
    ) -> Result<CredentialMutationResult, CredentialMutationFailure> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(AuthorizationRequest::RemoveCredential { public_key, reply })
            .await
            .map_err(|_| CredentialMutationFailure::NotCommitted {
                failure: writer_closed(),
            })?;
        response
            .await
            .map_err(|_| CredentialMutationFailure::NotCommitted {
                failure: writer_closed(),
            })?
    }
}

fn writer_closed() -> RenderFailure {
    RenderFailure::Prepare {
        failure: RenderPrepareFailure::WriterClosed,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthorizationProjectionState {
    ReloadRequired,
    Current,
}

/// The running single-writer that owns `authorized-users.conf`.
pub struct NatsAuthorizationWriter {
    handle: NatsAuthorizationHandle,
    health: NatsAuthorizationHealthReader,
    task: JoinHandle<()>,
}

impl NatsAuthorizationWriter {
    /// Spawns the single-writer render task. `authorized-users.conf` is a rendered
    /// projection of the grant `store`; the task renders from the store and reloads
    /// nats-server whenever a render is requested.
    pub fn start(
        authorized_users_file: PathBuf,
        store: NatsAuthorizationStore,
        reload: impl NatsReloadRunner,
    ) -> Self {
        Self::start_with_now(
            authorized_users_file,
            store,
            reload,
            Arc::new(current_unix_seconds),
        )
    }

    fn start_with_now(
        authorized_users_file: PathBuf,
        store: NatsAuthorizationStore,
        reload: impl NatsReloadRunner,
        now: NowSource,
    ) -> Self {
        let (sender, mut receiver) = mpsc::channel(RENDER_QUEUE_DEPTH);
        let health = NatsAuthorizationHealthReader::default();
        let task_health = health.clone();
        let writer_span = tracing::Span::current();
        let task = tokio::spawn(
            async move {
                let reload = Arc::new(reload);
                run_authorization_loop(
                    &authorized_users_file,
                    &store,
                    reload,
                    &mut receiver,
                    &task_health,
                    now,
                )
                .await;
            }
            .instrument(writer_span),
        );

        Self {
            handle: NatsAuthorizationHandle { sender },
            health,
            task,
        }
    }

    #[must_use]
    pub fn handle(&self) -> NatsAuthorizationHandle {
        self.handle.clone()
    }

    #[must_use]
    pub(crate) fn health_reader(&self) -> NatsAuthorizationHealthReader {
        self.health.clone()
    }

    pub fn shutdown(self) {
        drop(self.handle);
        self.task.abort();
    }
}

type NowSource = Arc<dyn Fn() -> Result<u64, String> + Send + Sync>;

struct AuthorizationLoopState {
    projection: AuthorizationProjectionState,
    wake: Option<AuthorizationWake>,
    retry_delay: Duration,
}

impl AuthorizationLoopState {
    fn new() -> Self {
        // A restarted control process cannot know whether the independently
        // supervised nats-server applied the current file before the restart.
        Self {
            projection: AuthorizationProjectionState::ReloadRequired,
            wake: None,
            retry_delay: RETRY_SCHEDULE.interval,
        }
    }

    async fn finish(
        &mut self,
        outcome: AuthorizationIterationOutcome,
        store: &NatsAuthorizationStore,
        health: &NatsAuthorizationHealthReader,
        now_unix_seconds: u64,
    ) {
        self.projection = outcome.projection;
        if let Some(failure) = outcome.failure {
            health.record_failure(failure);
            (self.wake, self.retry_delay) = retry_after_failure(self.retry_delay);
        } else if matches!(self.projection, AuthorizationProjectionState::Current) {
            (self.wake, self.retry_delay) =
                schedule_after_success(store, now_unix_seconds, health, self.retry_delay).await;
        }
    }

    fn clock_failed(&mut self, health: &NatsAuthorizationHealthReader, message: &str) {
        health.record_failure(message);
        (self.wake, self.retry_delay) = retry_after_failure(self.retry_delay);
    }
}

struct AuthorizationIterationOutcome {
    projection: AuthorizationProjectionState,
    failure: Option<String>,
}

async fn run_authorization_loop<R: NatsReloadRunner>(
    path: &Path,
    store: &NatsAuthorizationStore,
    reload: Arc<R>,
    receiver: &mut mpsc::Receiver<AuthorizationRequest>,
    health: &NatsAuthorizationHealthReader,
    now: NowSource,
) {
    let mut state = AuthorizationLoopState::new();
    loop {
        let Some(event) = next_authorization_event(receiver, state.wake).await else {
            break;
        };
        let now_unix_seconds = match now() {
            Ok(now_unix_seconds) => now_unix_seconds,
            Err(message) => {
                if let AuthorizationEvent::Request(request) = event {
                    reply_with_clock_failure(*request, message.clone());
                }
                state.clock_failed(health, &message);
                continue;
            }
        };
        let outcome = match event {
            AuthorizationEvent::Wake => {
                handle_wake(
                    path,
                    store,
                    Arc::clone(&reload),
                    state.projection,
                    now_unix_seconds,
                )
                .await
            }
            AuthorizationEvent::Request(request) => {
                handle_authorization_request(
                    path,
                    store,
                    Arc::clone(&reload),
                    *request,
                    state.projection,
                    now_unix_seconds,
                )
                .await
            }
        };
        state.finish(outcome, store, health, now_unix_seconds).await;
    }
}

async fn next_authorization_event(
    receiver: &mut mpsc::Receiver<AuthorizationRequest>,
    wake: Option<AuthorizationWake>,
) -> Option<AuthorizationEvent> {
    match wake {
        Some(scheduled) => tokio::select! {
            request = receiver.recv() => request.map(|request| AuthorizationEvent::Request(Box::new(request))),
            () = tokio::time::sleep(scheduled.wait_duration()) => Some(AuthorizationEvent::Wake),
        },
        None => receiver
            .recv()
            .await
            .map(|request| AuthorizationEvent::Request(Box::new(request))),
    }
}

async fn handle_wake<R: NatsReloadRunner>(
    path: &Path,
    store: &NatsAuthorizationStore,
    reload: Arc<R>,
    projection: AuthorizationProjectionState,
    now_unix_seconds: u64,
) -> AuthorizationIterationOutcome {
    let result = handle_render_request_at(
        path,
        store,
        reload,
        None,
        None,
        projection,
        now_unix_seconds,
    )
    .await;
    AuthorizationIterationOutcome {
        projection: if result.is_ok() {
            AuthorizationProjectionState::Current
        } else {
            AuthorizationProjectionState::ReloadRequired
        },
        failure: result.err().map(|failure| failure.to_string()),
    }
}

async fn handle_authorization_request<R: NatsReloadRunner>(
    path: &Path,
    store: &NatsAuthorizationStore,
    reload: Arc<R>,
    request: AuthorizationRequest,
    projection: AuthorizationProjectionState,
    now_unix_seconds: u64,
) -> AuthorizationIterationOutcome {
    match request {
        AuthorizationRequest::Render { verify, reply } => {
            let result = handle_render_request_at(
                path,
                store,
                reload,
                None,
                verify,
                projection,
                now_unix_seconds,
            )
            .await;
            let outcome = render_request_outcome(&result, projection, false);
            let _ = reply.send(result);
            outcome
        }
        AuthorizationRequest::Authorize {
            grant,
            verify,
            reply,
        } => {
            let result = handle_render_request_at(
                path,
                store,
                reload,
                Some(grant),
                verify,
                projection,
                now_unix_seconds,
            )
            .await;
            let outcome = render_request_outcome(&result, projection, true);
            let _ = reply.send(result);
            outcome
        }
        AuthorizationRequest::AddCredential { grant, reply } => {
            let result =
                handle_add_credential_at(path, store, reload, grant, projection, now_unix_seconds)
                    .await;
            let outcome = mutation_request_outcome(&result, projection);
            let _ = reply.send(result);
            outcome
        }
        AuthorizationRequest::RemoveCredential { public_key, reply } => {
            let result = handle_remove_credential_at(
                path,
                store,
                reload,
                &public_key,
                projection,
                now_unix_seconds,
            )
            .await;
            let outcome = mutation_request_outcome(&result, projection);
            let _ = reply.send(result);
            outcome
        }
    }
}

fn render_request_outcome(
    result: &Result<RenderedAuthorization, RenderFailure>,
    projection: AuthorizationProjectionState,
    authorize: bool,
) -> AuthorizationIterationOutcome {
    match result {
        Ok(_) => AuthorizationIterationOutcome {
            projection: AuthorizationProjectionState::Current,
            failure: None,
        },
        Err(failure) => AuthorizationIterationOutcome {
            projection: if authorize || matches!(failure, RenderFailure::Reload { .. }) {
                AuthorizationProjectionState::ReloadRequired
            } else {
                projection
            },
            failure: Some(failure.to_string()),
        },
    }
}

fn mutation_request_outcome(
    result: &Result<CredentialMutationResult, CredentialMutationFailure>,
    projection: AuthorizationProjectionState,
) -> AuthorizationIterationOutcome {
    match result {
        Ok(_) => AuthorizationIterationOutcome {
            projection: AuthorizationProjectionState::Current,
            failure: None,
        },
        Err(failure @ CredentialMutationFailure::Committed { .. }) => {
            AuthorizationIterationOutcome {
                projection: AuthorizationProjectionState::ReloadRequired,
                failure: Some(failure.to_string()),
            }
        }
        Err(
            CredentialMutationFailure::Rejected { .. }
            | CredentialMutationFailure::NotCommitted { .. },
        ) => AuthorizationIterationOutcome {
            projection,
            failure: None,
        },
    }
}

enum AuthorizationEvent {
    Request(Box<AuthorizationRequest>),
    Wake,
}

fn reply_with_clock_failure(request: AuthorizationRequest, message: String) {
    match request {
        AuthorizationRequest::Render { reply, .. }
        | AuthorizationRequest::Authorize { reply, .. } => {
            let _ = reply.send(Err(clock_render_failure(message)));
        }
        AuthorizationRequest::AddCredential { reply, .. }
        | AuthorizationRequest::RemoveCredential { reply, .. } => {
            let _ = reply.send(Err(CredentialMutationFailure::NotCommitted {
                failure: clock_render_failure(message),
            }));
        }
    }
}

fn clock_render_failure(message: String) -> RenderFailure {
    RenderFailure::Prepare {
        failure: RenderPrepareFailure::Store { message },
    }
}

fn read_authorized_users_contents(path: &Path) -> Result<Option<String>, AuthorizedUsersFileError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AuthorizedUsersFileError {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_render_request_at(
    path: &Path,
    store: &NatsAuthorizationStore,
    reload: Arc<impl NatsReloadRunner>,
    authorize: Option<NatsAuthorizationGrant>,
    verify: Option<NatsConnectConfig>,
    projection: AuthorizationProjectionState,
    now_unix_seconds: u64,
) -> Result<RenderedAuthorization, RenderFailure> {
    let prepare = |failure: RenderPrepareFailure| RenderFailure::Prepare { failure };
    let store_prepare = store_failure;
    // Render from the store PLUS the pending grant, held in memory: the grant is only
    // persisted after the render and reload succeed (see the tail), so a failed
    // machine-add leaves no durable grant behind. The on-disk file is read only to
    // skip a needless reload.
    let mut desired = store.list().await.map_err(store_prepare)?;
    if let Some(user) = &authorize {
        upsert_in_place(&mut desired, user.clone());
    }
    desired.retain(|grant| authorization_is_active_at(grant, now_unix_seconds));
    let current_contents = read_authorized_users_contents(path)
        .map_err(|source| prepare(RenderPrepareFailure::File { source }))?;

    let rendered = render_authorized_users(&desired);
    let file_is_current = current_contents.as_deref() == Some(rendered.as_str());
    if matches!(projection, AuthorizationProjectionState::Current) && file_is_current {
        // The conf already reflects the desired set, so no reload — but the grant
        // must still be persisted: this is the path a resumed mint takes when a crash
        // landed between a prior render's reload and its store write, and skipping it
        // here would leave the machine authorized on the server yet absent from the
        // store, so the next full render would strip it.
        if let Some(grant) = &authorize {
            store.upsert(grant).await.map_err(store_prepare)?;
        }
        return Ok(RenderedAuthorization {
            user_count: desired.len(),
            reload: None,
        });
    }

    if !file_is_current {
        write_file_atomically(path, rendered.as_bytes())
            .map_err(|error| RenderPrepareFailure::WriteFile {
                path: path.to_path_buf(),
                message: error.to_string(),
            })
            .map_err(prepare)?;
    }

    let reload_outcome = tokio::time::timeout(
        RELOAD_COMMAND_TIMEOUT,
        tokio::task::spawn_blocking(move || reload.reload()),
    )
    .await;
    let evidence = match reload_outcome {
        Ok(Ok(NatsReloadOutcome::Reloaded(evidence))) => evidence,
        Ok(Ok(NatsReloadOutcome::Failed(evidence))) => {
            return Err(RenderFailure::Reload {
                failure: NatsReloadFailure::CommandFailed { evidence },
            });
        }
        Ok(Err(join_error)) => {
            return Err(RenderFailure::Reload {
                failure: NatsReloadFailure::RunnerPanicked {
                    message: join_error.to_string(),
                },
            });
        }
        Err(_) => {
            return Err(RenderFailure::Reload {
                failure: NatsReloadFailure::TimedOut {
                    limit: RELOAD_COMMAND_TIMEOUT,
                },
            });
        }
    };

    if let Some(config) = verify {
        verify_credential(&config).await?;
    }

    // The render + reload (+ verify) succeeded — only now make the grant durable, so
    // a grant that never reached the running server is never left in the store.
    if let Some(grant) = authorize {
        store.upsert(&grant).await.map_err(store_prepare)?;
    }

    Ok(RenderedAuthorization {
        user_count: desired.len(),
        reload: Some(evidence),
    })
}

fn authorization_is_active_at(grant: &NatsAuthorizationGrant, now_unix_seconds: u64) -> bool {
    match grant {
        NatsAuthorizationGrant::Credential(CredentialGrant { role, .. }) => {
            role.is_active_at(now_unix_seconds)
        }
        NatsAuthorizationGrant::Internal { .. } => true,
    }
}

async fn handle_add_credential_at(
    path: &Path,
    store: &NatsAuthorizationStore,
    reload: Arc<impl NatsReloadRunner>,
    grant: CredentialGrant,
    projection: AuthorizationProjectionState,
    now_unix_seconds: u64,
) -> Result<CredentialMutationResult, CredentialMutationFailure> {
    let outcome = store
        .upsert_credential_at(&grant, now_unix_seconds)
        .await
        .map_err(|error| CredentialMutationFailure::NotCommitted {
            failure: store_failure(error),
        })?;
    let change = match outcome {
        CredentialUpsertStoreOutcome::Rejected(
            CredentialUpsertStoreRejection::AuthorityMismatch {
                existing,
                requested,
            },
        ) => {
            return Err(CredentialMutationFailure::Rejected {
                reason: CredentialMutationRejection::RoleMismatch {
                    existing,
                    requested,
                },
            });
        }
        CredentialUpsertStoreOutcome::Rejected(
            CredentialUpsertStoreRejection::ActiveBuildExecutorIdentity {
                identity,
                existing_public_key,
            },
        ) => {
            return Err(CredentialMutationFailure::Rejected {
                reason: CredentialMutationRejection::ActiveBuildExecutorIdentity {
                    identity,
                    existing_public_key,
                },
            });
        }
        CredentialUpsertStoreOutcome::Unchanged => CredentialMutationChange::Unchanged,
        CredentialUpsertStoreOutcome::Updated => CredentialMutationChange::Updated,
        CredentialUpsertStoreOutcome::Added => CredentialMutationChange::Added,
    };

    let authorization = match change {
        CredentialMutationChange::Unchanged => handle_render_request_at(
            path,
            store,
            reload,
            None,
            None,
            projection,
            now_unix_seconds,
        )
        .await
        .map_err(|failure| CredentialMutationFailure::Committed { failure })?,
        CredentialMutationChange::Added | CredentialMutationChange::Updated => {
            handle_render_request_at(
                path,
                store,
                reload,
                None,
                None,
                AuthorizationProjectionState::ReloadRequired,
                now_unix_seconds,
            )
            .await
            .map_err(|failure| CredentialMutationFailure::Committed { failure })?
        }
        CredentialMutationChange::Removed => unreachable!("add cannot remove a credential"),
    };
    Ok(CredentialMutationResult {
        change,
        authorization,
    })
}

fn current_unix_seconds() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))
}

async fn handle_remove_credential_at(
    path: &Path,
    store: &NatsAuthorizationStore,
    reload: Arc<impl NatsReloadRunner>,
    public_key: &NatsUserPublicKey,
    projection: AuthorizationProjectionState,
    now_unix_seconds: u64,
) -> Result<CredentialMutationResult, CredentialMutationFailure> {
    let outcome = store.remove_credential(public_key).await.map_err(|error| {
        CredentialMutationFailure::NotCommitted {
            failure: store_failure(error),
        }
    })?;
    let (change, render_projection) = match outcome {
        CredentialRemoveStoreOutcome::Removed => (
            CredentialMutationChange::Removed,
            AuthorizationProjectionState::ReloadRequired,
        ),
        CredentialRemoveStoreOutcome::Absent => (CredentialMutationChange::Unchanged, projection),
        CredentialRemoveStoreOutcome::RejectedLastOperator => {
            return Err(CredentialMutationFailure::Rejected {
                reason: CredentialMutationRejection::LastOperator,
            });
        }
    };
    let authorization = handle_render_request_at(
        path,
        store,
        reload,
        None,
        None,
        render_projection,
        now_unix_seconds,
    )
    .await
    .map_err(|failure| CredentialMutationFailure::Committed { failure })?;
    Ok(CredentialMutationResult {
        change,
        authorization,
    })
}

fn store_failure(error: NatsAuthorizationStoreError) -> RenderFailure {
    RenderFailure::Prepare {
        failure: RenderPrepareFailure::Store {
            message: error.to_string(),
        },
    }
}

/// In-memory upsert by authority key: the pending grant is folded into the rendered
/// set before it is persisted, so a failed render never leaves it in the store.
fn upsert_in_place(users: &mut Vec<NatsAuthorizationGrant>, user: NatsAuthorizationGrant) {
    let key = user.authority_record_key();
    if let Some(existing) = users
        .iter_mut()
        .find(|existing| existing.authority_record_key() == key)
    {
        *existing = user;
    } else {
        users.push(user);
    }
}

async fn verify_credential(config: &NatsConnectConfig) -> Result<(), RenderFailure> {
    let mut last_error = "no connection attempt made".to_owned();
    for _ in 0..VERIFY_ATTEMPTS {
        match connect_authenticated(config, VERIFY_CONNECT_TIMEOUT).await {
            Ok(client) => {
                if connection_remains_authenticated(|| {
                    client.connection_state() == async_nats::connection::State::Connected
                })
                .await
                {
                    return Ok(());
                }
                last_error = "credential connection did not remain authenticated".to_owned();
            }
            Err(error) => {
                last_error = error.to_string();
                tokio::time::sleep(VERIFY_RETRY_DELAY).await;
            }
        }
    }
    Err(RenderFailure::Verify {
        message: last_error,
    })
}

async fn connection_remains_authenticated(is_connected: impl FnOnce() -> bool) -> bool {
    // async-nats can expose Connected after the first non-error server frame,
    // before a following authorization violation changes the connection state.
    // Keep the verifier's client alive for one propagation interval so that
    // delayed rejection is observed before join material becomes available.
    tokio::time::sleep(VERIFY_RETRY_DELAY).await;
    is_connected()
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
