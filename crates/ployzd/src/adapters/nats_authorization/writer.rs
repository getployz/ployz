use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::adapters::atomic_file::write_file_atomically;
use ployz_core::nats_config::{
    AuthorizedUsersParseError, NatsAuthorizedUser, parse_authorized_users, render_authorized_users,
};
use ployz_nats::connect::{NatsConnectConfig, connect_authenticated};
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

/// Why reading the on-disk authorized-users file failed. A missing file is
/// not an error: it reads as the empty set.
#[derive(Debug)]
pub enum AuthorizedUsersFileError {
    Read {
        path: PathBuf,
        message: String,
    },
    Parse {
        path: PathBuf,
        source: AuthorizedUsersParseError,
    },
}

impl fmt::Display for AuthorizedUsersFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, message } => write!(
                formatter,
                "failed to read authorized-users file {}: {message}",
                path.display()
            ),
            Self::Parse { path, source } => write!(
                formatter,
                "failed to parse authorized-users file {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for AuthorizedUsersFileError {}

/// Why a render did not complete, shaped by the pipeline phase that failed.
/// The variant names the progress that preceded it: `Reload` means the file
/// rendered first; `Verify` means render and reload both completed.
#[derive(Debug)]
pub enum RenderFailure {
    /// Nothing reached the running server.
    Prepare { failure: RenderPrepareFailure },
    /// The file rendered; the nats-server reload failed.
    Reload { failure: NatsReloadFailure },
    /// Render and reload completed; the minted credential never
    /// authenticated within the bounded verify window.
    Verify { message: String },
}

impl fmt::Display for RenderFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prepare { failure } => failure.fmt(formatter),
            Self::Reload { failure } => failure.fmt(formatter),
            Self::Verify { message } => {
                write!(
                    formatter,
                    "minted credential failed verification: {message}"
                )
            }
        }
    }
}

impl std::error::Error for RenderFailure {}

/// A render failure before anything changed on the running server.
#[derive(Debug)]
pub enum RenderPrepareFailure {
    /// The single-writer render task is no longer running.
    WriterClosed,
    /// The current on-disk file could not be read or parsed.
    File { source: AuthorizedUsersFileError },
    /// The atomic file write failed.
    WriteFile { path: PathBuf, message: String },
}

impl fmt::Display for RenderPrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WriterClosed => {
                formatter.write_str("authorization render writer task is no longer running")
            }
            Self::File { source } => source.fmt(formatter),
            Self::WriteFile { path, message } => write!(
                formatter,
                "failed to write authorized-users file {}: {message}",
                path.display()
            ),
        }
    }
}

struct RenderRequest {
    authorize: Option<NatsAuthorizedUser>,
    verify: Option<NatsConnectConfig>,
    reply: oneshot::Sender<Result<RenderedAuthorization, RenderFailure>>,
}

/// Clonable submitter into the single-writer render task.
#[derive(Clone)]
pub struct NatsAuthorizationHandle {
    sender: mpsc::Sender<RenderRequest>,
}

impl NatsAuthorizationHandle {
    pub async fn render(
        &self,
        verify: Option<NatsConnectConfig>,
    ) -> Result<RenderedAuthorization, RenderFailure> {
        self.submit(None, verify).await
    }

    pub async fn authorize_and_render(
        &self,
        user: NatsAuthorizedUser,
        verify: Option<NatsConnectConfig>,
    ) -> Result<RenderedAuthorization, RenderFailure> {
        self.submit(Some(user), verify).await
    }

    async fn submit(
        &self,
        authorize: Option<NatsAuthorizedUser>,
        verify: Option<NatsConnectConfig>,
    ) -> Result<RenderedAuthorization, RenderFailure> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(RenderRequest {
                authorize,
                verify,
                reply,
            })
            .await
            .map_err(|_| RenderFailure::Prepare {
                failure: RenderPrepareFailure::WriterClosed,
            })?;
        response.await.map_err(|_| RenderFailure::Prepare {
            failure: RenderPrepareFailure::WriterClosed,
        })?
    }
}

/// The running single-writer that owns `authorized-users.conf`.
pub struct NatsAuthorizationWriter {
    handle: NatsAuthorizationHandle,
    task: JoinHandle<()>,
}

impl NatsAuthorizationWriter {
    /// Spawns the single-writer render task that owns the authorized-users
    /// authority file.
    pub fn start(authorized_users_file: PathBuf, reload: impl NatsReloadRunner) -> Self {
        let (sender, mut receiver) = mpsc::channel(RENDER_QUEUE_DEPTH);
        let task = tokio::spawn(async move {
            let reload = Arc::new(reload);
            while let Some(request) = receiver.recv().await {
                let RenderRequest {
                    authorize,
                    verify,
                    reply,
                } = request;
                let result = handle_render_request(
                    &authorized_users_file,
                    Arc::clone(&reload),
                    authorize,
                    verify,
                )
                .await;
                let _ = reply.send(result);
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

fn parse_authorized_users_contents(
    path: &Path,
    contents: Option<&str>,
) -> Result<Vec<NatsAuthorizedUser>, AuthorizedUsersFileError> {
    let Some(contents) = contents else {
        return Ok(Vec::new());
    };
    parse_authorized_users(contents).map_err(|source| AuthorizedUsersFileError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn read_authorized_users_contents(path: &Path) -> Result<Option<String>, AuthorizedUsersFileError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AuthorizedUsersFileError::Read {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

async fn handle_render_request(
    path: &Path,
    reload: Arc<impl NatsReloadRunner>,
    authorize: Option<NatsAuthorizedUser>,
    verify: Option<NatsConnectConfig>,
) -> Result<RenderedAuthorization, RenderFailure> {
    let prepare = |failure: RenderPrepareFailure| RenderFailure::Prepare { failure };
    let current_contents = read_authorized_users_contents(path)
        .map_err(|source| prepare(RenderPrepareFailure::File { source }))?;
    let mut desired = parse_authorized_users_contents(path, current_contents.as_deref())
        .map_err(|source| prepare(RenderPrepareFailure::File { source }))?;
    if let Some(user) = authorize {
        replace_authorized_user(&mut desired, user);
    }

    let rendered = render_authorized_users(&desired);
    if current_contents.as_deref() == Some(rendered.as_str()) {
        return Ok(RenderedAuthorization {
            user_count: desired.len(),
            reload: None,
        });
    }

    write_file_atomically(path, rendered.as_bytes())
        .map_err(|error| RenderPrepareFailure::WriteFile {
            path: path.to_path_buf(),
            message: error.to_string(),
        })
        .map_err(prepare)?;

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

    Ok(RenderedAuthorization {
        user_count: desired.len(),
        reload: Some(evidence),
    })
}

fn replace_authorized_user(users: &mut Vec<NatsAuthorizedUser>, user: NatsAuthorizedUser) {
    let authority_key = user.authority_record_key();
    if let Some(existing) = users
        .iter_mut()
        .find(|existing| existing.authority_record_key() == authority_key)
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
