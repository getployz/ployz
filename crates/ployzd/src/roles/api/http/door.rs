//! Bounded TLS transport for the public, join-only door.

use std::convert::Infallible;
use std::future::pending;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use ployz_core::join::{
    JoinDoorCertFingerprint, JoinDoorCertificatePem, JoinDoorMaterial, JoinDoorPrivateKeyPem,
};
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, watch};
use tokio_rustls::TlsAcceptor;

use super::server::ApiService;

const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TLS_MATERIAL_BYTES: usize = 64 * 1024;

/// A bound join-door listener with one immutable TLS identity.
pub(super) struct JoinDoorListener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    material: Arc<JoinDoorMaterial>,
}

impl JoinDoorListener {
    pub(super) async fn bind(
        listen_addr: SocketAddr,
        private_key_path: &Path,
        certificate_path: &Path,
        fingerprint_path: &Path,
    ) -> Result<Self, JoinDoorBindError> {
        let (acceptor, material) =
            load_listener_identity(private_key_path, certificate_path, fingerprint_path).await?;
        let listener =
            TcpListener::bind(listen_addr)
                .await
                .map_err(|source| JoinDoorBindError::Bind {
                    listen_addr,
                    source,
                })?;
        Ok(Self {
            listener,
            acceptor,
            material: Arc::new(material),
        })
    }

    pub(super) async fn accept(&self) -> Result<(TcpStream, SocketAddr), std::io::Error> {
        self.listener.accept().await
    }

    pub(super) fn acceptor(&self) -> TlsAcceptor {
        self.acceptor.clone()
    }

    pub(super) fn material(&self) -> Arc<JoinDoorMaterial> {
        Arc::clone(&self.material)
    }
}

pub(super) async fn accept_join_connection(
    door: &Option<JoinDoorListener>,
) -> Result<(TcpStream, SocketAddr), std::io::Error> {
    match door {
        Some(door) => door.accept().await,
        None => pending().await,
    }
}

pub(super) async fn load_listener_identity(
    private_key_path: &Path,
    certificate_path: &Path,
    fingerprint_path: &Path,
) -> Result<(TlsAcceptor, JoinDoorMaterial), JoinDoorBindError> {
    let certificate_pem = read_bounded_material(certificate_path)
        .await
        .map_err(|error| error.into_certificate_error())?;
    let private_key_pem = read_bounded_material(private_key_path)
        .await
        .map_err(|error| error.into_private_key_error())?;
    let fingerprint = read_bounded_material(fingerprint_path)
        .await
        .map_err(|error| error.into_fingerprint_error())?;
    let acceptor = tls_acceptor_from_pem(
        certificate_path,
        &certificate_pem,
        private_key_path,
        &private_key_pem,
    )?;

    let mut certificates = CertificateDer::pem_slice_iter(&certificate_pem);
    let certificate = certificates
        .next()
        .transpose()
        .map_err(|source| JoinDoorBindError::ParseCertificate {
            path: certificate_path.to_path_buf(),
            detail: source.to_string(),
        })?
        .ok_or_else(|| JoinDoorBindError::NoCertificates {
            path: certificate_path.to_path_buf(),
        })?;
    let expected = JoinDoorCertFingerprint::for_certificate_der(certificate.as_ref());
    let fingerprint_text = std::str::from_utf8(&fingerprint)
        .map_err(|_| JoinDoorBindError::InvalidFingerprint {
            path: fingerprint_path.to_path_buf(),
        })?
        .trim();
    let fingerprint =
        JoinDoorCertFingerprint::try_new(fingerprint_text.to_owned()).map_err(|_| {
            JoinDoorBindError::InvalidFingerprint {
                path: fingerprint_path.to_path_buf(),
            }
        })?;
    if fingerprint != expected {
        return Err(JoinDoorBindError::FingerprintMismatch {
            path: fingerprint_path.to_path_buf(),
        });
    }

    let private_key_text =
        String::from_utf8(private_key_pem).map_err(|_| JoinDoorBindError::ParsePrivateKey {
            path: private_key_path.to_path_buf(),
            detail: "private-key PEM is not UTF-8".to_owned(),
        })?;
    let certificate_pem =
        String::from_utf8(certificate_pem).map_err(|_| JoinDoorBindError::ParseCertificate {
            path: certificate_path.to_path_buf(),
            detail: "certificate PEM is not UTF-8".to_owned(),
        })?;
    let private_key = JoinDoorPrivateKeyPem::try_new(private_key_text).map_err(|error| {
        JoinDoorBindError::ParsePrivateKey {
            path: private_key_path.to_path_buf(),
            detail: error.to_string(),
        }
    })?;
    let certificate_pem = JoinDoorCertificatePem::try_new(certificate_pem).map_err(|error| {
        JoinDoorBindError::ParseCertificate {
            path: certificate_path.to_path_buf(),
            detail: error.to_string(),
        }
    })?;
    Ok((
        acceptor,
        JoinDoorMaterial {
            private_key_pem: private_key,
            certificate_pem,
            fingerprint,
        },
    ))
}

pub(super) fn tls_acceptor_from_pem(
    certificate_path: &Path,
    certificate_pem: &[u8],
    private_key_path: &Path,
    private_key_pem: &[u8],
) -> Result<TlsAcceptor, JoinDoorBindError> {
    let certificates = CertificateDer::pem_slice_iter(certificate_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| JoinDoorBindError::ParseCertificate {
            path: certificate_path.to_path_buf(),
            detail: source.to_string(),
        })?;
    if certificates.is_empty() {
        return Err(JoinDoorBindError::NoCertificates {
            path: certificate_path.to_path_buf(),
        });
    }
    let private_key = PrivateKeyDer::from_pem_slice(private_key_pem).map_err(|source| {
        JoinDoorBindError::ParsePrivateKey {
            path: private_key_path.to_path_buf(),
            detail: source.to_string(),
        }
    })?;
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|source| JoinDoorBindError::TlsConfiguration {
                detail: source.to_string(),
            })?
            .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .map_err(|source| JoinDoorBindError::TlsConfiguration {
                detail: source.to_string(),
            })?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

async fn read_bounded_material(path: &Path) -> Result<Vec<u8>, MaterialReadError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|source| MaterialReadError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.len() > MAX_TLS_MATERIAL_BYTES as u64 {
        return Err(MaterialReadError::TooLarge {
            path: path.to_path_buf(),
            limit: MAX_TLS_MATERIAL_BYTES,
        });
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|source| MaterialReadError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > MAX_TLS_MATERIAL_BYTES {
        return Err(MaterialReadError::TooLarge {
            path: path.to_path_buf(),
            limit: MAX_TLS_MATERIAL_BYTES,
        });
    }
    Ok(bytes)
}

#[derive(Debug)]
enum MaterialReadError {
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    TooLarge {
        path: std::path::PathBuf,
        limit: usize,
    },
}

impl MaterialReadError {
    fn into_certificate_error(self) -> JoinDoorBindError {
        match self {
            Self::Read { path, source } => JoinDoorBindError::ReadCertificate { path, source },
            Self::TooLarge { path, limit } => {
                JoinDoorBindError::CertificateTooLarge { path, limit }
            }
        }
    }

    fn into_private_key_error(self) -> JoinDoorBindError {
        match self {
            Self::Read { path, source } => JoinDoorBindError::ReadPrivateKey { path, source },
            Self::TooLarge { path, limit } => JoinDoorBindError::PrivateKeyTooLarge { path, limit },
        }
    }

    fn into_fingerprint_error(self) -> JoinDoorBindError {
        match self {
            Self::Read { path, source } => JoinDoorBindError::ReadFingerprint { path, source },
            Self::TooLarge { path, limit } => {
                JoinDoorBindError::FingerprintTooLarge { path, limit }
            }
        }
    }
}

pub(super) async fn serve_join_connection(
    stream: TcpStream,
    peer: SocketAddr,
    acceptor: TlsAcceptor,
    service: Arc<ApiService>,
    mut shutdown: watch::Receiver<bool>,
    _connection_permit: OwnedSemaphorePermit,
) {
    let tls = match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(error)) => {
            tracing::debug!(error = %error, "join door TLS handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!("join door TLS handshake timed out");
            return;
        }
    };

    let service_for_requests = Arc::clone(&service);
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HTTP_HEADER_READ_TIMEOUT)
        .keep_alive(false);
    let connection = builder.serve_connection(
        TokioIo::new(tls),
        service_fn(move |request| {
            let service = Arc::clone(&service_for_requests);
            async move { Ok::<_, Infallible>(service.handle_join_door(peer, request).await) }
        }),
    );
    tokio::pin!(connection);
    let deadline = tokio::time::sleep(CONNECTION_TIMEOUT);
    tokio::pin!(deadline);
    tokio::select! {
        result = &mut connection => log_join_connection_result(result),
        () = &mut deadline => tracing::debug!("join door connection reached its deadline"),
        changed = shutdown.changed() => {
            if changed.is_ok() && *shutdown.borrow() {
                connection.as_mut().graceful_shutdown();
                log_join_connection_result(connection.await);
            }
        }
    }
}

fn log_join_connection_result(result: Result<(), hyper::Error>) {
    if let Err(error) = result {
        tracing::debug!(error = %error, "join door HTTP/1 connection ended");
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JoinDoorBindError {
    #[error("could not read join door certificate {path:?}: {source}")]
    ReadCertificate {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("join door certificate {path:?} exceeds the {limit}-byte limit")]
    CertificateTooLarge {
        path: std::path::PathBuf,
        limit: usize,
    },
    #[error("could not parse join door certificate {path:?}: {detail}")]
    ParseCertificate {
        path: std::path::PathBuf,
        detail: String,
    },
    #[error("join door certificate file {path:?} contains no certificates")]
    NoCertificates { path: std::path::PathBuf },
    #[error("could not read join door private key {path:?}: {source}")]
    ReadPrivateKey {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("join door private key {path:?} exceeds the {limit}-byte limit")]
    PrivateKeyTooLarge {
        path: std::path::PathBuf,
        limit: usize,
    },
    #[error("could not read join door fingerprint {path:?}: {source}")]
    ReadFingerprint {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("join door fingerprint {path:?} exceeds the {limit}-byte limit")]
    FingerprintTooLarge {
        path: std::path::PathBuf,
        limit: usize,
    },
    #[error("join door fingerprint {path:?} is not canonical SHA-256 hex")]
    InvalidFingerprint { path: std::path::PathBuf },
    #[error("join door fingerprint {path:?} does not match the configured certificate")]
    FingerprintMismatch { path: std::path::PathBuf },
    #[error("could not parse join door private key {path:?}: {detail}")]
    ParsePrivateKey {
        path: std::path::PathBuf,
        detail: String,
    },
    #[error("could not configure join door TLS: {detail}")]
    TlsConfiguration { detail: String },
    #[error("could not bind join door listener {listen_addr}: {source}")]
    Bind {
        listen_addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}
