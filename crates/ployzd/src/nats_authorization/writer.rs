use std::collections::BTreeSet;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ployz_core::nats_config::{
    AuthorizedUsersParseError, NatsAuthorizedUser, parse_authorized_users, render_authorized_users,
};
use ployz_nats::connect::{NatsConnectConfig, connect_authenticated};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
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
    pub reload: NatsReloadEvidence,
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
    /// The KV authority set could not be read.
    ReadAuthority { message: String },
    /// The current on-disk file could not be read or parsed.
    File { source: AuthorizedUsersFileError },
    /// The render was refused because it would remove these principals
    /// from the recovery-evidence file. Shrinking the set returns with an
    /// explicit machine-remove operation.
    RefusedShrink { missing: Vec<String> },
    /// The atomic file write failed.
    WriteFile { path: PathBuf, message: String },
}

impl fmt::Display for RenderPrepareFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WriterClosed => {
                formatter.write_str("authorization render writer task is no longer running")
            }
            Self::ReadAuthority { message } => {
                write!(
                    formatter,
                    "failed to read authorized principal set: {message}"
                )
            }
            Self::File { source } => source.fmt(formatter),
            Self::RefusedShrink { missing } => write!(
                formatter,
                "refused to shrink authorized user set outside machine-remove (missing: {})",
                missing.join(", ")
            ),
            Self::WriteFile { path, message } => write!(
                formatter,
                "failed to write authorized-users file {}: {message}",
                path.display()
            ),
        }
    }
}

struct RenderRequest {
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
        let (reply, response) = oneshot::channel();
        self.sender
            .send(RenderRequest { verify, reply })
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
pub struct NatsAuthorizationRuntime {
    handle: NatsAuthorizationHandle,
    task: JoinHandle<()>,
}

#[derive(Debug)]
pub enum NatsAuthorizationStartError {
    ReadAuthorizedUsersFile { source: AuthorizedUsersFileError },
    AdoptAuthority { message: String },
}

impl fmt::Display for NatsAuthorizationStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadAuthorizedUsersFile { source } => source.fmt(formatter),
            Self::AdoptAuthority { message } => write!(
                formatter,
                "failed to adopt authorized users into KV authority: {message}"
            ),
        }
    }
}

impl std::error::Error for NatsAuthorizationStartError {}

impl NatsAuthorizationRuntime {
    /// Adopts the existing file into KV authority, then spawns the
    /// single-writer render task.
    pub async fn start(
        authorized_users_file: PathBuf,
        core_state: AsyncNatsCoreStateStore,
        reload: impl NatsReloadRunner,
    ) -> Result<Self, NatsAuthorizationStartError> {
        adopt_authorized_users_from_file(&authorized_users_file, &core_state).await?;

        let (sender, mut receiver) = mpsc::channel(RENDER_QUEUE_DEPTH);
        let task = tokio::spawn(async move {
            let reload = Arc::new(reload);
            while let Some(request) = receiver.recv().await {
                let RenderRequest { verify, reply } = request;
                let result = handle_render_request(
                    &authorized_users_file,
                    &core_state,
                    Arc::clone(&reload),
                    verify,
                )
                .await;
                let _ = reply.send(result);
            }
        });

        Ok(Self {
            handle: NatsAuthorizationHandle { sender },
            task,
        })
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

async fn adopt_authorized_users_from_file(
    path: &Path,
    core_state: &AsyncNatsCoreStateStore,
) -> Result<(), NatsAuthorizationStartError> {
    let users = read_authorized_users_file(path)
        .map_err(|source| NatsAuthorizationStartError::ReadAuthorizedUsersFile { source })?;
    for user in &users {
        core_state
            .adopt_nats_authorized_user_if_absent(user)
            .await
            .map_err(|error| NatsAuthorizationStartError::AdoptAuthority {
                message: error.to_string(),
            })?;
    }
    Ok(())
}

fn read_authorized_users_file(
    path: &Path,
) -> Result<Vec<NatsAuthorizedUser>, AuthorizedUsersFileError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(AuthorizedUsersFileError::Read {
                path: path.to_path_buf(),
                message: error.to_string(),
            });
        }
    };
    parse_authorized_users(&contents).map_err(|source| AuthorizedUsersFileError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

async fn handle_render_request(
    path: &Path,
    core_state: &AsyncNatsCoreStateStore,
    reload: Arc<impl NatsReloadRunner>,
    verify: Option<NatsConnectConfig>,
) -> Result<RenderedAuthorization, RenderFailure> {
    let prepare = |failure: RenderPrepareFailure| RenderFailure::Prepare { failure };
    let desired = core_state.nats_authorized_users().await.map_err(|error| {
        prepare(RenderPrepareFailure::ReadAuthority {
            message: error.to_string(),
        })
    })?;
    let on_disk = read_authorized_users_file(path)
        .map_err(|source| prepare(RenderPrepareFailure::File { source }))?;

    let desired_principals: BTreeSet<String> = desired
        .iter()
        .map(|user| user.principal.authority_key())
        .collect();
    let missing: Vec<String> = on_disk
        .iter()
        .map(|user| user.principal.authority_key())
        .filter(|key| !desired_principals.contains(key))
        .collect();
    if !missing.is_empty() {
        return Err(prepare(RenderPrepareFailure::RefusedShrink { missing }));
    }

    write_file_atomically(path, &render_authorized_users(&desired)).map_err(prepare)?;

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
        reload: evidence,
    })
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

fn write_file_atomically(path: &Path, contents: &str) -> Result<(), RenderPrepareFailure> {
    let write_error = |message: String| RenderPrepareFailure::WriteFile {
        path: path.to_path_buf(),
        message,
    };
    let Some(parent) = path.parent() else {
        return Err(write_error("path has no parent directory".to_owned()));
    };
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| write_error(error.to_string()))?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| write_error(error.to_string()))?;
    file.persist(path)
        .map(|_| ())
        .map_err(|error| write_error(error.error.to_string()))
}
