use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::adapters::atomic_file::write_file_atomically;
use crate::control::intent::nats_authorizations::{
    CredentialRemoveStoreOutcome, NatsAuthorizationStore, NatsAuthorizationStoreError,
};
use ployz_core::nats_config::{
    CredentialGrant, CredentialRole, NatsAuthorizationGrant, NatsUserPublicKey,
};
use ployz_nats::connect::{NatsConnectConfig, connect_authenticated};
use ployz_nats::permissions::render_authorized_users;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

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
        let (sender, mut receiver) = mpsc::channel(RENDER_QUEUE_DEPTH);
        let task = tokio::spawn(async move {
            let reload = Arc::new(reload);
            // A restarted control process cannot know whether the independently
            // supervised nats-server applied the current file before the restart.
            let mut projection = AuthorizationProjectionState::ReloadRequired;
            while let Some(request) = receiver.recv().await {
                match request {
                    AuthorizationRequest::Render { verify, reply } => {
                        let result = handle_render_request(
                            &authorized_users_file,
                            &store,
                            Arc::clone(&reload),
                            None,
                            verify,
                            projection,
                        )
                        .await;
                        match &result {
                            Ok(_) => projection = AuthorizationProjectionState::Current,
                            Err(RenderFailure::Reload { .. }) => {
                                projection = AuthorizationProjectionState::ReloadRequired;
                            }
                            Err(RenderFailure::Prepare { .. } | RenderFailure::Verify { .. }) => {}
                        }
                        let _ = reply.send(result);
                    }
                    AuthorizationRequest::Authorize {
                        grant,
                        verify,
                        reply,
                    } => {
                        let result = handle_render_request(
                            &authorized_users_file,
                            &store,
                            Arc::clone(&reload),
                            Some(grant),
                            verify,
                            projection,
                        )
                        .await;
                        projection = if result.is_ok() {
                            AuthorizationProjectionState::Current
                        } else {
                            AuthorizationProjectionState::ReloadRequired
                        };
                        let _ = reply.send(result);
                    }
                    AuthorizationRequest::AddCredential { grant, reply } => {
                        let result = handle_add_credential(
                            &authorized_users_file,
                            &store,
                            Arc::clone(&reload),
                            grant,
                            projection,
                        )
                        .await;
                        match &result {
                            Ok(_) => projection = AuthorizationProjectionState::Current,
                            Err(CredentialMutationFailure::Committed { .. }) => {
                                projection = AuthorizationProjectionState::ReloadRequired;
                            }
                            Err(
                                CredentialMutationFailure::Rejected { .. }
                                | CredentialMutationFailure::NotCommitted { .. },
                            ) => {}
                        }
                        let _ = reply.send(result);
                    }
                    AuthorizationRequest::RemoveCredential { public_key, reply } => {
                        let result = handle_remove_credential(
                            &authorized_users_file,
                            &store,
                            Arc::clone(&reload),
                            &public_key,
                            projection,
                        )
                        .await;
                        match &result {
                            Ok(_) => projection = AuthorizationProjectionState::Current,
                            Err(CredentialMutationFailure::Committed { .. }) => {
                                projection = AuthorizationProjectionState::ReloadRequired;
                            }
                            Err(
                                CredentialMutationFailure::Rejected { .. }
                                | CredentialMutationFailure::NotCommitted { .. },
                            ) => {}
                        }
                        let _ = reply.send(result);
                    }
                }
            }
        });

        Self {
            handle: NatsAuthorizationHandle { sender },
            task,
        }
    }

    #[must_use]
    pub fn handle(&self) -> NatsAuthorizationHandle {
        self.handle.clone()
    }

    pub fn shutdown(self) {
        drop(self.handle);
        self.task.abort();
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

async fn handle_render_request(
    path: &Path,
    store: &NatsAuthorizationStore,
    reload: Arc<impl NatsReloadRunner>,
    authorize: Option<NatsAuthorizationGrant>,
    verify: Option<NatsConnectConfig>,
    projection: AuthorizationProjectionState,
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

async fn handle_add_credential(
    path: &Path,
    store: &NatsAuthorizationStore,
    reload: Arc<impl NatsReloadRunner>,
    grant: CredentialGrant,
    projection: AuthorizationProjectionState,
) -> Result<CredentialMutationResult, CredentialMutationFailure> {
    let existing = store
        .list_credentials()
        .await
        .map_err(|error| CredentialMutationFailure::NotCommitted {
            failure: store_failure(error),
        })?
        .into_iter()
        .find(|existing| existing.public_key == grant.public_key);
    let change = match existing {
        Some(existing) if existing.role != grant.role => {
            return Err(CredentialMutationFailure::Rejected {
                reason: CredentialMutationRejection::RoleMismatch {
                    existing: existing.role,
                    requested: grant.role,
                },
            });
        }
        Some(existing) if existing == grant => CredentialMutationChange::Unchanged,
        Some(_) => CredentialMutationChange::Updated,
        None => CredentialMutationChange::Added,
    };

    let authorization = match change {
        CredentialMutationChange::Unchanged => {
            handle_render_request(path, store, reload, None, None, projection)
                .await
                .map_err(|failure| CredentialMutationFailure::Committed { failure })?
        }
        CredentialMutationChange::Added | CredentialMutationChange::Updated => {
            store
                .upsert(&NatsAuthorizationGrant::Credential(grant))
                .await
                .map_err(|error| CredentialMutationFailure::NotCommitted {
                    failure: store_failure(error),
                })?;
            handle_render_request(
                path,
                store,
                reload,
                None,
                None,
                AuthorizationProjectionState::ReloadRequired,
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

async fn handle_remove_credential(
    path: &Path,
    store: &NatsAuthorizationStore,
    reload: Arc<impl NatsReloadRunner>,
    public_key: &NatsUserPublicKey,
    projection: AuthorizationProjectionState,
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
    let authorization = handle_render_request(path, store, reload, None, None, render_projection)
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
                drop(client);
                return Ok(());
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ployz_core::nats_config::{CredentialName, MintedNatsUser};

    use super::*;
    use crate::control::store::CoreStore;

    #[derive(Clone)]
    struct ScriptedReload {
        outcomes: Arc<Mutex<VecDeque<NatsReloadOutcome>>>,
        calls: Arc<AtomicUsize>,
    }

    impl ScriptedReload {
        fn new(outcomes: impl IntoIterator<Item = NatsReloadOutcome>) -> Self {
            Self {
                outcomes: Arc::new(Mutex::new(outcomes.into_iter().collect())),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl NatsReloadRunner for ScriptedReload {
        fn reload(&self) -> NatsReloadOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.outcomes
                .lock()
                .expect("reload outcomes")
                .pop_front()
                .expect("scripted reload outcome")
        }
    }

    fn credential(name: &str) -> CredentialGrant {
        CredentialGrant {
            public_key: MintedNatsUser::generate().expect("mint credential").public,
            name: CredentialName::try_new(name).expect("credential name"),
            role: CredentialRole::Operator,
        }
    }

    fn reloaded() -> NatsReloadOutcome {
        NatsReloadOutcome::Reloaded(NatsReloadEvidence {
            command: "reload".to_owned(),
            output: "ok".to_owned(),
        })
    }

    fn failed_reload() -> NatsReloadOutcome {
        NatsReloadOutcome::Failed(NatsReloadEvidence {
            command: "reload".to_owned(),
            output: "failed".to_owned(),
        })
    }

    async fn store_with(grants: &[CredentialGrant]) -> NatsAuthorizationStore {
        let store = NatsAuthorizationStore::new(CoreStore::open_in_memory().await.expect("store"));
        for grant in grants {
            store
                .upsert(&NatsAuthorizationGrant::Credential(grant.clone()))
                .await
                .expect("seed credential");
        }
        store
    }

    async fn write_current(path: &Path, store: &NatsAuthorizationStore) {
        std::fs::write(
            path,
            render_authorized_users(&store.list().await.expect("list grants")),
        )
        .expect("write authorization file");
    }

    #[tokio::test]
    async fn adding_identical_credential_is_a_no_op() {
        let credential = credential("Founder");
        let store = store_with(std::slice::from_ref(&credential)).await;
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("authorized-users.conf");
        write_current(&path, &store).await;
        let reload = ScriptedReload::new([reloaded()]);
        let writer = NatsAuthorizationWriter::start(path, store, reload.clone());
        writer.handle().render(None).await.expect("startup render");

        let result = writer
            .handle()
            .add_credential(credential)
            .await
            .expect("no-op add");

        assert_eq!(
            (result.change, result.authorization.reload, reload.calls()),
            (CredentialMutationChange::Unchanged, None, 1)
        );
        writer.shutdown();
    }

    #[tokio::test]
    async fn readding_public_key_updates_its_name() {
        let old = credential("Founder");
        let store = store_with(std::slice::from_ref(&old)).await;
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("authorized-users.conf");
        write_current(&path, &store).await;
        let reload = ScriptedReload::new([reloaded(), reloaded()]);
        let writer = NatsAuthorizationWriter::start(path, store.clone(), reload.clone());
        writer.handle().render(None).await.expect("startup render");
        let updated = CredentialGrant {
            name: CredentialName::try_new("Nick's laptop").expect("credential name"),
            ..old
        };

        let result = writer
            .handle()
            .add_credential(updated.clone())
            .await
            .expect("update credential");

        assert_eq!(
            (
                result.change,
                result.authorization.reload.is_some(),
                store.list_credentials().await.expect("credentials"),
                reload.calls(),
            ),
            (CredentialMutationChange::Updated, true, vec![updated], 2)
        );
        writer.shutdown();
    }

    #[tokio::test]
    async fn removing_absent_credential_is_a_no_op() {
        let founder = credential("Founder");
        let absent = credential("Absent");
        let store = store_with(std::slice::from_ref(&founder)).await;
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("authorized-users.conf");
        write_current(&path, &store).await;
        let reload = ScriptedReload::new([reloaded()]);
        let writer = NatsAuthorizationWriter::start(path, store, reload.clone());
        writer.handle().render(None).await.expect("startup render");

        let result = writer
            .handle()
            .remove_credential(absent.public_key)
            .await
            .expect("no-op remove");

        assert_eq!(
            (result.change, result.authorization.reload, reload.calls()),
            (CredentialMutationChange::Unchanged, None, 1)
        );
        writer.shutdown();
    }

    #[tokio::test]
    async fn failed_add_reload_keeps_grant_durable_across_writer_restart() {
        let first = credential("First");
        let second = credential("Second");
        let store = store_with(std::slice::from_ref(&first)).await;
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("authorized-users.conf");
        write_current(&path, &store).await;
        let reload = ScriptedReload::new([reloaded(), failed_reload()]);
        let writer = NatsAuthorizationWriter::start(path, store.clone(), reload.clone());
        let handle = writer.handle();
        handle.render(None).await.expect("startup render");

        let failure = handle
            .add_credential(second.clone())
            .await
            .expect_err("reload fails");
        writer.shutdown();
        let restart_reload = ScriptedReload::new([reloaded()]);
        let restarted = NatsAuthorizationWriter::start(
            directory.path().join("authorized-users.conf"),
            store.clone(),
            restart_reload.clone(),
        );
        let restart_render = restarted
            .handle()
            .render(None)
            .await
            .expect("restart projection");

        assert!(matches!(
            failure,
            CredentialMutationFailure::Committed {
                failure: RenderFailure::Reload { .. }
            }
        ));
        assert_eq!(
            (
                store.list_credentials().await.expect("credentials"),
                restart_render.reload.is_some(),
                reload.calls(),
                restart_reload.calls(),
            ),
            (vec![first, second], true, 2, 1)
        );
        restarted.shutdown();
    }

    #[tokio::test]
    async fn failed_remove_reload_keeps_revocation_durable_across_writer_restart() {
        let first = credential("First");
        let second = credential("Second");
        let store = store_with(&[first.clone(), second.clone()]).await;
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("authorized-users.conf");
        write_current(&path, &store).await;
        let reload = ScriptedReload::new([reloaded(), failed_reload()]);
        let writer = NatsAuthorizationWriter::start(path, store.clone(), reload.clone());
        let handle = writer.handle();
        handle.render(None).await.expect("startup render");

        let failure = handle
            .remove_credential(first.public_key)
            .await
            .expect_err("reload fails");
        writer.shutdown();
        let restart_reload = ScriptedReload::new([reloaded()]);
        let restarted = NatsAuthorizationWriter::start(
            directory.path().join("authorized-users.conf"),
            store.clone(),
            restart_reload.clone(),
        );
        let restart_render = restarted
            .handle()
            .render(None)
            .await
            .expect("restart projection");

        assert!(matches!(
            failure,
            CredentialMutationFailure::Committed {
                failure: RenderFailure::Reload { .. }
            }
        ));
        assert_eq!(
            (
                store.list_credentials().await.expect("credentials"),
                restart_render.reload.is_some(),
                reload.calls(),
                restart_reload.calls(),
            ),
            (vec![second], true, 2, 1)
        );
        restarted.shutdown();
    }

    #[tokio::test]
    async fn queued_removes_cannot_delete_the_final_operator() {
        let first = credential("First");
        let second = credential("Second");
        let store = store_with(&[first.clone(), second.clone()]).await;
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("authorized-users.conf");
        write_current(&path, &store).await;
        let reload = ScriptedReload::new([reloaded(), reloaded()]);
        let writer = NatsAuthorizationWriter::start(path, store.clone(), reload);
        let handle = writer.handle();
        handle.render(None).await.expect("startup render");

        let first_remove = handle.remove_credential(first.public_key);
        let second_remove = handle.remove_credential(second.public_key);
        let outcomes = tokio::join!(first_remove, second_remove);

        assert!(matches!(
            outcomes,
            (
                Ok(CredentialMutationResult {
                    change: CredentialMutationChange::Removed,
                    ..
                }),
                Err(CredentialMutationFailure::Rejected {
                    reason: CredentialMutationRejection::LastOperator
                })
            ) | (
                Err(CredentialMutationFailure::Rejected {
                    reason: CredentialMutationRejection::LastOperator
                }),
                Ok(CredentialMutationResult {
                    change: CredentialMutationChange::Removed,
                    ..
                })
            )
        ));
        assert_eq!(
            store.list_credentials().await.expect("credentials").len(),
            1
        );
        writer.shutdown();
    }
}
