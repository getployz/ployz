//! Root Keeper endpoint for bounded, read-only control observations.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use crate::roles::handshake_control::{
    HandshakeControlRequest, HandshakeControlResponse, HandshakeControlUnavailable, read_request,
    write_response,
};

use super::provider::{KeeperHandshakeObservation, KeeperMeshProvider, KeeperProviderError};

const CONTROL_SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROL_PROVIDER_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct KeeperControlSocket {
    listener: UnixListener,
    path: PathBuf,
}

impl KeeperControlSocket {
    pub(super) async fn bind(path: &Path) -> Result<Self, KeeperControlSocketError> {
        let parent = path
            .parent()
            .ok_or_else(|| KeeperControlSocketError::MissingParent {
                path: path.to_path_buf(),
            })?;
        let parent_metadata = tokio::fs::metadata(parent).await.map_err(|source| {
            KeeperControlSocketError::RuntimeDirectory {
                path: parent.to_path_buf(),
                message: source.to_string(),
            }
        })?;
        if !parent_metadata.is_dir() {
            return Err(KeeperControlSocketError::RuntimeDirectory {
                path: parent.to_path_buf(),
                message: "path is not a directory".to_owned(),
            });
        }
        remove_stale_socket(path).await?;
        let listener =
            UnixListener::bind(path).map_err(|source| KeeperControlSocketError::Bind {
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
        provider: KeeperMeshProvider,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), KeeperControlSocketError> {
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted.map_err(|source| KeeperControlSocketError::Accept {
                        path: self.path.clone(),
                        message: source.to_string(),
                    })?;
                    tokio::select! {
                        () = handle_connection(stream, &provider) => {}
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Drop for KeeperControlSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn handle_connection(mut stream: UnixStream, provider: &KeeperMeshProvider) {
    let response = match tokio::time::timeout(CONTROL_SOCKET_IO_TIMEOUT, read_request(&mut stream))
        .await
    {
        Ok(Ok(request)) => {
            match tokio::time::timeout(CONTROL_PROVIDER_TIMEOUT, handle_request(request, provider))
                .await
            {
                Ok(response) => response,
                Err(_) => HandshakeControlResponse::Unavailable {
                    reason: HandshakeControlUnavailable::ProviderTimedOut,
                },
            }
        }
        Ok(Err(_)) | Err(_) => HandshakeControlResponse::Unavailable {
            reason: HandshakeControlUnavailable::Protocol,
        },
    };
    match tokio::time::timeout(
        CONTROL_SOCKET_IO_TIMEOUT,
        write_response(&mut stream, &response),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "could not return Keeper control response");
        }
        Err(_) => {
            tracing::warn!("returning Keeper control response timed out");
        }
    }
}

async fn handle_request(
    request: HandshakeControlRequest,
    provider: &KeeperMeshProvider,
) -> HandshakeControlResponse {
    match request {
        HandshakeControlRequest::ObserveHandshake { public_key } => {
            match provider.observe_handshake(public_key).await {
                Ok(KeeperHandshakeObservation::Observed {
                    observed_at,
                    age_seconds,
                }) => HandshakeControlResponse::Observed {
                    observed_at,
                    age_seconds,
                },
                Ok(KeeperHandshakeObservation::PeerAbsent) => {
                    HandshakeControlResponse::Unavailable {
                        reason: HandshakeControlUnavailable::PeerAbsent,
                    }
                }
                Err(error @ KeeperProviderError::TimedOut { .. }) => {
                    tracing::warn!(error = %error, "Keeper handshake observation timed out");
                    HandshakeControlResponse::Unavailable {
                        reason: HandshakeControlUnavailable::ProviderTimedOut,
                    }
                }
                Err(
                    error @ (KeeperProviderError::Host(_)
                    | KeeperProviderError::Poisoned
                    | KeeperProviderError::Task { .. }),
                ) => {
                    tracing::warn!(error = %error, "Keeper handshake observation failed");
                    HandshakeControlResponse::Unavailable {
                        reason: HandshakeControlUnavailable::ProviderFailed,
                    }
                }
            }
        }
    }
}

async fn remove_stale_socket(path: &Path) -> Result<(), KeeperControlSocketError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_socket() => tokio::fs::remove_file(path)
            .await
            .map_err(|source| KeeperControlSocketError::RemoveStaleSocket {
                path: path.to_path_buf(),
                message: source.to_string(),
            }),
        Ok(_) => Err(KeeperControlSocketError::ExistingNonSocket {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(KeeperControlSocketError::Inspect {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

#[cfg(unix)]
fn set_socket_permissions(path: &Path) -> Result<(), KeeperControlSocketError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .map_err(|source| KeeperControlSocketError::Inspect {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?
        .permissions();
    permissions.set_mode(0o660);
    std::fs::set_permissions(path, permissions).map_err(|source| {
        KeeperControlSocketError::SetPermissions {
            path: path.to_path_buf(),
            message: source.to_string(),
        }
    })
}

#[cfg(not(unix))]
fn set_socket_permissions(_path: &Path) -> Result<(), KeeperControlSocketError> {
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum KeeperControlSocketError {
    #[error("Keeper control socket path {path} has no parent directory")]
    MissingParent { path: PathBuf },
    #[error("Keeper control runtime directory {path} is unavailable: {message}")]
    RuntimeDirectory { path: PathBuf, message: String },
    #[error("could not inspect Keeper control socket {path}: {message}")]
    Inspect { path: PathBuf, message: String },
    #[error("Keeper control socket path {path} already exists and is not a socket")]
    ExistingNonSocket { path: PathBuf },
    #[error("could not remove stale Keeper control socket {path}: {message}")]
    RemoveStaleSocket { path: PathBuf, message: String },
    #[error("could not bind Keeper control socket {path}: {message}")]
    Bind { path: PathBuf, message: String },
    #[error("could not set Keeper control socket permissions {path}: {message}")]
    SetPermissions { path: PathBuf, message: String },
    #[error("could not accept Keeper control socket connection {path}: {message}")]
    Accept { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_host_runner::SupervisorBackend;
    use ployz_host_runner::builtin_wireguard::{
        BuiltinWireguardEbpfConfig, BuiltinWireguardHostConfig, BuiltinWireguardPorts,
    };
    use std::os::unix::fs::PermissionsExt;

    fn provider() -> KeeperMeshProvider {
        let ebpf = BuiltinWireguardEbpfConfig::try_new(
            "br-ployz".to_owned(),
            "/usr/local/bin/ployz-ebpf-ctl".into(),
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
            "/sys/fs/bpf/ployz".into(),
        )
        .expect("eBPF config");
        let host = BuiltinWireguardHostConfig::try_new(
            "/etc/ployz/wireguard.key".into(),
            "ployz0".to_owned(),
            BuiltinWireguardPorts::try_new(51_820, 8_787, 2_020, 2_021).expect("ports"),
            1_420,
            ebpf,
            SupervisorBackend::Systemd,
            Duration::from_millis(20),
        )
        .expect("host config");
        KeeperMeshProvider::for_test(host, Duration::from_millis(20))
    }

    #[tokio::test]
    async fn control_socket_is_distinct_has_bounded_permissions_and_honors_shutdown() {
        assert_ne!(
            ployz_host_runner::CONTROL_SOCKET_PATH,
            ployz_host_runner::UPGRADE_SOCKET_PATH
        );
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("keeper-control.sock");
        let socket = KeeperControlSocket::bind(&path).await.expect("bind socket");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("socket metadata")
                .permissions()
                .mode()
                & 0o777,
            0o660
        );
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(socket.serve(provider(), shutdown_rx));
        shutdown_tx.send(true).expect("shutdown");
        server.await.expect("server task").expect("clean shutdown");
        assert!(!path.exists());

        let missing_parent = directory.path().join("missing/keeper-control.sock");
        assert!(matches!(
            KeeperControlSocket::bind(&missing_parent).await,
            Err(KeeperControlSocketError::RuntimeDirectory { path, .. })
                if path == directory.path().join("missing")
        ));
    }
}
