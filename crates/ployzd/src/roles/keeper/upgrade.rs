//! Keeper-owned local swap and restart effects.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;

use ployz_core::roles::PloyzdRole;
use ployz_host_runner::{PloyzdArtifactStore, role_unit_name};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use crate::roles::upgrade::{UpgradeRequest, UpgradeResponse, read_request, write_response};

const UPGRADE_SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct KeeperUpgradeSocket {
    listener: UnixListener,
    path: PathBuf,
}

impl KeeperUpgradeSocket {
    pub(super) async fn bind(path: &Path) -> Result<Self, KeeperUpgradeSocketError> {
        let parent = path
            .parent()
            .ok_or_else(|| KeeperUpgradeSocketError::MissingParent {
                path: path.to_path_buf(),
            })?;
        tokio::fs::create_dir_all(parent).await.map_err(|source| {
            KeeperUpgradeSocketError::CreateParent {
                path: parent.to_path_buf(),
                message: source.to_string(),
            }
        })?;
        remove_stale_socket(path).await?;
        let listener =
            UnixListener::bind(path).map_err(|source| KeeperUpgradeSocketError::Bind {
                path: path.to_path_buf(),
                message: source.to_string(),
            })?;
        set_socket_permissions(path)?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    pub(super) async fn serve(
        self,
        store: PloyzdArtifactStore,
        command_timeout: Duration,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), KeeperUpgradeSocketError> {
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted.map_err(|source| KeeperUpgradeSocketError::Accept {
                        path: self.path.clone(),
                        message: source.to_string(),
                    })?;
                    handle_connection(stream, &store, command_timeout).await;
                }
            }
        }
    }
}

impl Drop for KeeperUpgradeSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn remove_stale_socket(path: &Path) -> Result<(), KeeperUpgradeSocketError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_socket() => tokio::fs::remove_file(path)
            .await
            .map_err(|source| KeeperUpgradeSocketError::RemoveStaleSocket {
                path: path.to_path_buf(),
                message: source.to_string(),
            }),
        Ok(_) => Err(KeeperUpgradeSocketError::ExistingNonSocket {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(KeeperUpgradeSocketError::Inspect {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

#[cfg(unix)]
fn set_socket_permissions(path: &Path) -> Result<(), KeeperUpgradeSocketError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .map_err(|source| KeeperUpgradeSocketError::Inspect {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?
        .permissions();
    permissions.set_mode(0o660);
    std::fs::set_permissions(path, permissions).map_err(|source| {
        KeeperUpgradeSocketError::SetPermissions {
            path: path.to_path_buf(),
            message: source.to_string(),
        }
    })
}

#[cfg(not(unix))]
fn set_socket_permissions(_path: &Path) -> Result<(), KeeperUpgradeSocketError> {
    Ok(())
}

async fn handle_connection(
    mut stream: UnixStream,
    store: &PloyzdArtifactStore,
    command_timeout: Duration,
) {
    let response =
        match tokio::time::timeout(UPGRADE_SOCKET_IO_TIMEOUT, read_request(&mut stream)).await {
            Ok(Ok(request)) => handle_request(request, store, command_timeout).await,
            Ok(Err(error)) => UpgradeResponse::Refused {
                message: error.to_string(),
            },
            Err(_) => UpgradeResponse::Refused {
                message: "Keeper upgrade request timed out".to_owned(),
            },
        };
    if let Err(error) = tokio::time::timeout(
        UPGRADE_SOCKET_IO_TIMEOUT,
        write_response(&mut stream, &response),
    )
    .await
    {
        tracing::warn!(error = ?error, "could not return Keeper upgrade response");
    }
}

async fn handle_request(
    request: UpgradeRequest,
    store: &PloyzdArtifactStore,
    command_timeout: Duration,
) -> UpgradeResponse {
    match request {
        UpgradeRequest::Arm { sha256 } => match store.arm_staged(&sha256) {
            Ok(_) => UpgradeResponse::Armed,
            Err(error) => UpgradeResponse::Refused {
                message: error.to_string(),
            },
        },
        UpgradeRequest::Commit => match store.pending_upgrade() {
            Ok(Some(_)) => match restart_systemd_role(PloyzdRole::Keeper, command_timeout).await {
                Ok(()) => UpgradeResponse::Committed,
                Err(message) => UpgradeResponse::Refused { message },
            },
            Ok(None) => UpgradeResponse::Refused {
                message: "no armed ployzd upgrade is pending".to_owned(),
            },
            Err(error) => UpgradeResponse::Refused {
                message: error.to_string(),
            },
        },
    }
}

pub(super) async fn restart_systemd_role(
    role: PloyzdRole,
    timeout: Duration,
) -> Result<(), String> {
    let unit = role_unit_name(&role);
    let mut command = tokio::process::Command::new("/usr/bin/systemctl");
    command
        .args(["--no-block", "restart", unit.as_str()])
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| format!("systemctl restart {unit} timed out after {timeout:?}"))?
        .map_err(|error| format!("could not run systemctl restart {unit}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl restart {unit} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum KeeperUpgradeSocketError {
    #[error("Keeper upgrade socket path {path} has no parent directory")]
    MissingParent { path: PathBuf },
    #[error("could not create Keeper upgrade socket directory {path}: {message}")]
    CreateParent { path: PathBuf, message: String },
    #[error("could not inspect Keeper upgrade socket {path}: {message}")]
    Inspect { path: PathBuf, message: String },
    #[error("Keeper upgrade socket path {path} already exists and is not a socket")]
    ExistingNonSocket { path: PathBuf },
    #[error("could not remove stale Keeper upgrade socket {path}: {message}")]
    RemoveStaleSocket { path: PathBuf, message: String },
    #[error("could not bind Keeper upgrade socket {path}: {message}")]
    Bind { path: PathBuf, message: String },
    #[error("could not set Keeper upgrade socket permissions {path}: {message}")]
    SetPermissions { path: PathBuf, message: String },
    #[error("could not accept Keeper upgrade socket connection {path}: {message}")]
    Accept { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeper_restart_uses_the_systemd_role_unit() {
        assert_eq!(role_unit_name(&PloyzdRole::Keeper), "ployzd-keeper.service");
    }
}
