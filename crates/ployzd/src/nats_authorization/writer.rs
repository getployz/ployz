use std::collections::BTreeSet;
use std::fmt;
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
    NatsReloadEvidence, NatsReloadOutcome, NatsReloadRunner, RELOAD_COMMAND_TIMEOUT,
};

const RENDER_QUEUE_DEPTH: usize = 16;
const VERIFY_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const VERIFY_ATTEMPTS: u32 = 10;
const VERIFY_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Whether a render may drop principals present in the current file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// A render that would shrink the principal set relative to the on-disk
    /// file is refused.
    PreserveUsers,
    /// Only an explicit machine-remove operation may shrink the set.
    MachineRemove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedAuthorization {
    pub user_count: usize,
    pub reload: NatsReloadEvidence,
}

#[derive(Debug)]
pub enum NatsAuthorizationError {
    ReadAuthority {
        message: String,
    },
    ReadFile {
        path: PathBuf,
        message: String,
    },
    ParseFile {
        path: PathBuf,
        source: AuthorizedUsersParseError,
    },
    /// The render was refused because it would remove these principals
    /// from the recovery-evidence file outside a machine-remove operation.
    RefusedShrink {
        missing: Vec<String>,
    },
    WriteFile {
        path: PathBuf,
        message: String,
    },
    Reload {
        evidence: NatsReloadEvidence,
    },
    VerifyConnect {
        message: String,
    },
    WriterClosed,
}

impl fmt::Display for NatsAuthorizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadAuthority { message } => {
                write!(
                    formatter,
                    "failed to read authorized principal set: {message}"
                )
            }
            Self::ReadFile { path, message } => write!(
                formatter,
                "failed to read authorized-users file {}: {message}",
                path.display()
            ),
            Self::ParseFile { path, source } => write!(
                formatter,
                "failed to parse authorized-users file {}: {source}",
                path.display()
            ),
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
            Self::Reload { evidence } => write!(
                formatter,
                "nats-server reload failed: {} -> {}",
                evidence.command, evidence.output
            ),
            Self::VerifyConnect { message } => {
                write!(
                    formatter,
                    "minted credential failed verification: {message}"
                )
            }
            Self::WriterClosed => {
                formatter.write_str("authorization render writer task is no longer running")
            }
        }
    }
}

impl std::error::Error for NatsAuthorizationError {}

struct RenderRequest {
    mode: RenderMode,
    verify: Option<NatsConnectConfig>,
    reply: oneshot::Sender<Result<RenderedAuthorization, NatsAuthorizationError>>,
}

/// Clonable submitter into the single-writer render task.
#[derive(Clone)]
pub struct NatsAuthorizationHandle {
    sender: mpsc::Sender<RenderRequest>,
}

impl NatsAuthorizationHandle {
    pub async fn render(
        &self,
        mode: RenderMode,
        verify: Option<NatsConnectConfig>,
    ) -> Result<RenderedAuthorization, NatsAuthorizationError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(RenderRequest {
                mode,
                verify,
                reply,
            })
            .await
            .map_err(|_| NatsAuthorizationError::WriterClosed)?;
        response
            .await
            .map_err(|_| NatsAuthorizationError::WriterClosed)?
    }
}

/// The running single-writer that owns `authorized-users.conf`.
pub struct NatsAuthorizationRuntime {
    handle: NatsAuthorizationHandle,
    task: JoinHandle<()>,
}

#[derive(Debug)]
pub enum NatsAuthorizationStartError {
    ReadFile {
        path: PathBuf,
        message: String,
    },
    ParseFile {
        path: PathBuf,
        source: AuthorizedUsersParseError,
    },
    AdoptAuthority {
        message: String,
    },
}

impl fmt::Display for NatsAuthorizationStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFile { path, message } => write!(
                formatter,
                "failed to read authorized-users file {}: {message}",
                path.display()
            ),
            Self::ParseFile { path, source } => write!(
                formatter,
                "failed to parse authorized-users file {}: {source}",
                path.display()
            ),
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
                let RenderRequest {
                    mode,
                    verify,
                    reply,
                } = request;
                let result = handle_render_request(
                    &authorized_users_file,
                    &core_state,
                    Arc::clone(&reload),
                    mode,
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
    let users = match read_authorized_users_file(path) {
        Ok(users) => users,
        Err(NatsAuthorizationError::ReadFile { path, message }) => {
            return Err(NatsAuthorizationStartError::ReadFile { path, message });
        }
        Err(NatsAuthorizationError::ParseFile { path, source }) => {
            return Err(NatsAuthorizationStartError::ParseFile { path, source });
        }
        Err(
            error @ (NatsAuthorizationError::ReadAuthority { .. }
            | NatsAuthorizationError::RefusedShrink { .. }
            | NatsAuthorizationError::WriteFile { .. }
            | NatsAuthorizationError::Reload { .. }
            | NatsAuthorizationError::VerifyConnect { .. }
            | NatsAuthorizationError::WriterClosed),
        ) => {
            return Err(NatsAuthorizationStartError::AdoptAuthority {
                message: error.to_string(),
            });
        }
    };
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
) -> Result<Vec<NatsAuthorizedUser>, NatsAuthorizationError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(NatsAuthorizationError::ReadFile {
                path: path.to_path_buf(),
                message: error.to_string(),
            });
        }
    };
    parse_authorized_users(&contents).map_err(|source| NatsAuthorizationError::ParseFile {
        path: path.to_path_buf(),
        source,
    })
}

async fn handle_render_request(
    path: &Path,
    core_state: &AsyncNatsCoreStateStore,
    reload: Arc<impl NatsReloadRunner>,
    mode: RenderMode,
    verify: Option<NatsConnectConfig>,
) -> Result<RenderedAuthorization, NatsAuthorizationError> {
    let desired = core_state.nats_authorized_users().await.map_err(|error| {
        NatsAuthorizationError::ReadAuthority {
            message: error.to_string(),
        }
    })?;
    let on_disk = read_authorized_users_file(path)?;

    let desired_principals: BTreeSet<String> = desired
        .iter()
        .map(|user| user.principal.authority_key())
        .collect();
    let missing: Vec<String> = on_disk
        .iter()
        .map(|user| user.principal.authority_key())
        .filter(|key| !desired_principals.contains(key))
        .collect();
    match (mode, missing.is_empty()) {
        (RenderMode::PreserveUsers, false) => {
            return Err(NatsAuthorizationError::RefusedShrink { missing });
        }
        (RenderMode::PreserveUsers, true) | (RenderMode::MachineRemove, true | false) => {}
    }

    write_file_atomically(path, &render_authorized_users(&desired))?;

    let reload_outcome = tokio::time::timeout(
        RELOAD_COMMAND_TIMEOUT,
        tokio::task::spawn_blocking(move || reload.reload()),
    )
    .await;
    let evidence = match reload_outcome {
        Ok(Ok(NatsReloadOutcome::Reloaded(evidence))) => evidence,
        Ok(Ok(NatsReloadOutcome::Failed(evidence))) => {
            return Err(NatsAuthorizationError::Reload { evidence });
        }
        Ok(Err(join_error)) => {
            return Err(NatsAuthorizationError::Reload {
                evidence: NatsReloadEvidence {
                    command: "<reload runner>".to_owned(),
                    output: format!("reload task failed: {join_error}"),
                },
            });
        }
        Err(_) => {
            return Err(NatsAuthorizationError::Reload {
                evidence: NatsReloadEvidence {
                    command: "<reload runner>".to_owned(),
                    output: format!(
                        "reload did not finish within {}s",
                        RELOAD_COMMAND_TIMEOUT.as_secs()
                    ),
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

async fn verify_credential(config: &NatsConnectConfig) -> Result<(), NatsAuthorizationError> {
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
    Err(NatsAuthorizationError::VerifyConnect {
        message: last_error,
    })
}

fn write_file_atomically(path: &Path, contents: &str) -> Result<(), NatsAuthorizationError> {
    let write_error = |message: String| NatsAuthorizationError::WriteFile {
        path: path.to_path_buf(),
        message,
    };
    let Some(parent) = path.parent() else {
        return Err(write_error("path has no parent directory".to_owned()));
    };
    let temp_path = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "authorized-users.conf".to_owned())
    ));
    std::fs::write(&temp_path, contents).map_err(|error| write_error(error.to_string()))?;
    std::fs::rename(&temp_path, path).map_err(|error| write_error(error.to_string()))
}
