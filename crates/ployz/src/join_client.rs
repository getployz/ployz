//! Public join-door client with certificate-fingerprint pinning.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hyper::Method;
use ployz_core::JOIN_ROUTE;
use ployz_core::join::{
    JoinAcceptanceValidationError, JoinAdmissionAccepted, JoinAdmissionReply, JoinAdmissionRequest,
    JoinBlob, JoinDoorRefusal, JoinMemberRequest, MachineJoinAccepted, MachineJoinRequest,
    PeerJoinRequest, ValidatedMachineJoinAccepted, ValidatedPeerJoinAccepted,
};
use ployz_core::operation::FailureMessage;
use ployz_host_runner::lifecycle::machine_join::MachineJoinDoor;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest as _, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::mesh::http::{MAX_MESH_API_RESPONSE_BYTES, MeshApiClient, MeshApiClientError};

pub const DEFAULT_JOIN_ENDPOINT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_JOIN_OVERALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
pub struct JoinDoorClient {
    endpoint_timeout: Duration,
    overall_timeout: Duration,
    max_response_bytes: usize,
}

impl JoinDoorClient {
    #[must_use]
    pub const fn new(
        endpoint_timeout: Duration,
        overall_timeout: Duration,
        max_response_bytes: usize,
    ) -> Self {
        Self {
            endpoint_timeout,
            overall_timeout,
            max_response_bytes,
        }
    }

    /// Admits and validates one machine through the fingerprint-pinned public door.
    pub async fn admit_machine(
        &self,
        blob: &JoinBlob,
        request: MachineJoinRequest,
    ) -> Result<ValidatedMachineJoinAccepted, JoinDoorClientError> {
        let reply = self
            .request_admission_pinned(
                blob.endpoints(),
                blob.door_cert_fingerprint().decoded_bytes(),
                &JoinAdmissionRequest {
                    token: blob.token_proof(),
                    member: JoinMemberRequest::Machine {
                        request: request.clone(),
                    },
                },
            )
            .await?;
        match reply {
            JoinAdmissionReply::Accepted { admission } => match *admission {
                JoinAdmissionAccepted::Machine { accepted } => accepted
                    .try_validate(&request, blob.door_cert_fingerprint())
                    .map_err(JoinDoorClientError::InvalidAcceptance),
                JoinAdmissionAccepted::Peer { .. } => Err(JoinDoorClientError::WrongAcceptanceKind),
            },
            JoinAdmissionReply::Refused { refusal } => {
                Err(JoinDoorClientError::Refused { refusal })
            }
        }
    }

    pub async fn admit_peer(
        &self,
        blob: &JoinBlob,
        request: PeerJoinRequest,
    ) -> Result<ValidatedPeerJoinAccepted, JoinDoorClientError> {
        let reply = self
            .request_admission_pinned(
                blob.endpoints(),
                blob.door_cert_fingerprint().decoded_bytes(),
                &JoinAdmissionRequest {
                    token: blob.token_proof(),
                    member: JoinMemberRequest::Peer {
                        request: request.clone(),
                    },
                },
            )
            .await?;
        match reply {
            JoinAdmissionReply::Accepted { admission } => match *admission {
                JoinAdmissionAccepted::Peer { accepted } => accepted
                    .try_validate(&request)
                    .map_err(JoinDoorClientError::InvalidAcceptance),
                JoinAdmissionAccepted::Machine { .. } => {
                    Err(JoinDoorClientError::WrongAcceptanceKind)
                }
            },
            JoinAdmissionReply::Refused { refusal } => {
                Err(JoinDoorClientError::Refused { refusal })
            }
        }
    }

    async fn request_admission_pinned<RequestBody>(
        &self,
        endpoints: &[SocketAddr],
        fingerprint: [u8; 32],
        request: &RequestBody,
    ) -> Result<JoinAdmissionReply, JoinDoorClientError>
    where
        RequestBody: Serialize + ?Sized,
    {
        self.request_pinned_with(endpoints, fingerprint, request, |reply| {
            matches!(
                reply,
                JoinAdmissionReply::Refused {
                    refusal: JoinDoorRefusal::TokenNotFound { .. }
                }
            )
        })
        .await
    }

    #[cfg(test)]
    async fn request_pinned<RequestBody, ResponseBody>(
        &self,
        endpoints: &[SocketAddr],
        fingerprint: [u8; 32],
        request: &RequestBody,
    ) -> Result<ResponseBody, JoinDoorClientError>
    where
        RequestBody: Serialize + ?Sized,
        ResponseBody: DeserializeOwned,
    {
        self.request_pinned_with(endpoints, fingerprint, request, |_| false)
            .await
    }

    async fn request_pinned_with<RequestBody, ResponseBody>(
        &self,
        endpoints: &[SocketAddr],
        fingerprint: [u8; 32],
        request: &RequestBody,
        should_defer_reply: impl Fn(&ResponseBody) -> bool,
    ) -> Result<ResponseBody, JoinDoorClientError>
    where
        RequestBody: Serialize + ?Sized,
        ResponseBody: DeserializeOwned,
    {
        if endpoints.is_empty() {
            return Err(JoinDoorClientError::NoAdvertisedEndpoints);
        }
        tokio::time::timeout(
            self.overall_timeout,
            self.try_endpoints(endpoints, fingerprint, request, &should_defer_reply),
        )
        .await
        .map_err(|_| JoinDoorClientError::OverallTimedOut)?
    }

    async fn try_endpoints<RequestBody, ResponseBody>(
        &self,
        endpoints: &[SocketAddr],
        fingerprint: [u8; 32],
        request: &RequestBody,
        should_defer_reply: &impl Fn(&ResponseBody) -> bool,
    ) -> Result<ResponseBody, JoinDoorClientError>
    where
        RequestBody: Serialize + ?Sized,
        ResponseBody: DeserializeOwned,
    {
        let mut attempts = Vec::with_capacity(endpoints.len());
        let mut deferred_reply = None;
        for endpoint in endpoints {
            let result = tokio::time::timeout(
                self.endpoint_timeout,
                self.try_endpoint(*endpoint, fingerprint, request),
            )
            .await;
            match result {
                Ok(Ok(reply)) if should_defer_reply(&reply) => deferred_reply = Some(reply),
                Ok(Ok(reply)) => return Ok(reply),
                Ok(Err(failure)) => attempts.push(JoinDoorAttempt {
                    endpoint: *endpoint,
                    failure,
                }),
                Err(_) => attempts.push(JoinDoorAttempt {
                    endpoint: *endpoint,
                    failure: JoinDoorAttemptFailure::TimedOut,
                }),
            }
        }
        if let Some(reply) = deferred_reply {
            return Ok(reply);
        }
        Err(JoinDoorClientError::AllEndpointsFailed { attempts })
    }

    async fn try_endpoint<RequestBody, ResponseBody>(
        &self,
        endpoint: SocketAddr,
        fingerprint: [u8; 32],
        request: &RequestBody,
    ) -> Result<ResponseBody, JoinDoorAttemptFailure>
    where
        RequestBody: Serialize + ?Sized,
        ResponseBody: DeserializeOwned,
    {
        let tcp = TcpStream::connect(endpoint)
            .await
            .map_err(|error| JoinDoorAttemptFailure::Connect(error.to_string()))?;
        let mismatch = Arc::new(AtomicBool::new(false));
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|error| JoinDoorAttemptFailure::Tls(error.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(DoorFingerprintVerifier::new(
            fingerprint,
            Arc::clone(&mismatch),
        )))
        .with_no_client_auth();
        let server_name = ServerName::IpAddress(endpoint.ip().into());
        let tls = match TlsConnector::from(Arc::new(config))
            .connect(server_name, tcp)
            .await
        {
            Ok(tls) => tls,
            Err(_) if mismatch.load(Ordering::Relaxed) => {
                return Err(JoinDoorAttemptFailure::FingerprintMismatch);
            }
            Err(error) => return Err(JoinDoorAttemptFailure::Tls(error.to_string())),
        };
        MeshApiClient::new(self.endpoint_timeout, self.max_response_bytes)
            .request_json(tls, Method::POST, JOIN_ROUTE, Some(request))
            .await
            .map_err(JoinDoorAttemptFailure::Http)
    }
}

impl Default for JoinDoorClient {
    fn default() -> Self {
        Self::new(
            DEFAULT_JOIN_ENDPOINT_TIMEOUT,
            DEFAULT_JOIN_OVERALL_TIMEOUT,
            MAX_MESH_API_RESPONSE_BYTES,
        )
    }
}

impl MachineJoinDoor for JoinDoorClient {
    fn redeem_machine(
        &mut self,
        blob: &JoinBlob,
        request: &MachineJoinRequest,
    ) -> impl std::future::Future<Output = Result<MachineJoinAccepted, FailureMessage>> + Send {
        let client = *self;
        let blob = blob.clone();
        let request = request.clone();
        async move {
            client
                .admit_machine(&blob, request)
                .await
                .map(ValidatedMachineJoinAccepted::into_accepted)
                .map_err(|error| {
                    FailureMessage::try_new(error.to_string())
                        .expect("join-door client errors always have non-empty messages")
                })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JoinDoorClientError {
    #[error("join blob contains no advertised door endpoints")]
    NoAdvertisedEndpoints,
    #[error("join door attempts exceeded the overall bounded deadline")]
    OverallTimedOut,
    #[error("every advertised join door failed: {attempts:?}")]
    AllEndpointsFailed { attempts: Vec<JoinDoorAttempt> },
    #[error("join door refused admission: {refusal}")]
    Refused { refusal: JoinDoorRefusal },
    #[error("join door accepted a different member kind than requested")]
    WrongAcceptanceKind,
    #[error("accepted machine join response was invalid: {0}")]
    InvalidAcceptance(JoinAcceptanceValidationError),
}

#[derive(Debug)]
pub struct JoinDoorAttempt {
    pub endpoint: SocketAddr,
    pub failure: JoinDoorAttemptFailure,
}

#[derive(Debug, thiserror::Error)]
pub enum JoinDoorAttemptFailure {
    #[error("connection failed: {0}")]
    Connect(String),
    #[error("TLS handshake failed: {0}")]
    Tls(String),
    #[error("door certificate fingerprint did not match the join blob")]
    FingerprintMismatch,
    #[error("HTTP exchange failed: {0}")]
    Http(#[source] MeshApiClientError),
    #[error("endpoint attempt exceeded its bounded deadline")]
    TimedOut,
}

struct DoorFingerprintVerifier {
    expected: [u8; 32],
    mismatch: Arc<AtomicBool>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl DoorFingerprintVerifier {
    fn new(expected: [u8; 32], mismatch: Arc<AtomicBool>) -> Self {
        Self {
            expected,
            mismatch,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }
}

impl fmt::Debug for DoorFingerprintVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DoorFingerprintVerifier")
            .finish_non_exhaustive()
    }
}

impl ServerCertVerifier for DoorFingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        if actual == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            self.mismatch.store(true, Ordering::Relaxed);
            Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::ids::{MachineRowId, PeerId, TokenId};
    use ployz_core::join::{
        JoinAdmissionRequest, JoinDoorCertFingerprint, JoinDoorRefusal, JoinStorageChoice,
        JoinStorageFacts, JoinTokenSecret, PeerJoinRequest,
    };
    use ployz_core::machine::MachineName;
    use ployz_core::network::WireGuardPublicKey;
    use rcgen::generate_simple_self_signed;
    use rustls::ServerConfig;
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use serde::{Deserialize, Serialize};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    use super::*;

    #[derive(Debug, Serialize)]
    struct SecretRequest<'a> {
        token: &'a str,
        kind: &'static str,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TypedReply {
        accepted_as: String,
    }

    struct DoorFixture {
        listener: TcpListener,
        acceptor: TlsAcceptor,
        fingerprint: [u8; 32],
    }

    async fn door_fixture() -> DoorFixture {
        door_fixture_at(0).await
    }

    async fn door_fixture_at(port: u16) -> DoorFixture {
        door_fixture_on(std::net::Ipv4Addr::LOCALHOST, port).await
    }

    async fn door_fixture_on(ip: std::net::Ipv4Addr, port: u16) -> DoorFixture {
        let generated = generate_simple_self_signed(["door.ployz.internal".to_owned()])
            .expect("fixture certificate");
        let fingerprint: [u8; 32] = Sha256::digest(generated.cert.der()).into();
        let private_key = PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der());
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("safe TLS protocol versions")
                .with_no_client_auth()
                .with_single_cert(vec![generated.cert.der().clone()], private_key.into())
                .expect("TLS server config");
        DoorFixture {
            listener: TcpListener::bind((ip, port)).await.expect("bind door"),
            acceptor: TlsAcceptor::from(Arc::new(config)),
            fingerprint,
        }
    }

    fn machine_request() -> MachineJoinRequest {
        MachineJoinRequest {
            machine_id: MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("machine id"),
            name: MachineName::try_new("edge-a").expect("machine name"),
            public_key: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("public key"),
            endpoint: None,
            storage_choice: JoinStorageChoice::Automatic,
            storage_facts: JoinStorageFacts {
                imported_zfs_pool: false,
                total_memory_bytes: 1024,
            },
        }
    }

    async fn serve_join_reply(
        listener: TcpListener,
        acceptor: TlsAcceptor,
        reply: JoinAdmissionReply,
    ) {
        let (tcp, _) = listener.accept().await.expect("accept TCP");
        let mut tls = acceptor.accept(tcp).await.expect("accept TLS");
        let _request = read_http_request(&mut tls).await;
        let body = serde_json::to_vec(&reply).expect("reply JSON");
        tls.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .expect("write reply head");
        tls.write_all(&body).await.expect("write reply body");
    }

    #[tokio::test]
    async fn lagged_token_not_found_door_does_not_mask_a_later_door() {
        let first = door_fixture_on(
            std::net::Ipv4Addr::new(127, 0, 0, 10),
            ployz_core::join::JOIN_DOOR_PORT,
        )
        .await;
        let second_listener = TcpListener::bind((
            std::net::Ipv4Addr::new(127, 0, 0, 11),
            ployz_core::join::JOIN_DOOR_PORT,
        ))
        .await
        .expect("bind second door");
        let endpoints = vec![
            first.listener.local_addr().expect("first address"),
            second_listener.local_addr().expect("second address"),
        ];
        let fingerprint = JoinDoorCertFingerprint::try_new(
            first
                .fingerprint
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        )
        .expect("door fingerprint");
        let token_id = TokenId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("token id");
        let blob = JoinBlob::try_new(
            token_id.clone(),
            JoinTokenSecret::try_from_bytes([0x5a; 32]),
            fingerprint,
            endpoints,
        )
        .expect("join blob");
        let second_acceptor = first.acceptor.clone();
        let first_task = tokio::spawn(serve_join_reply(
            first.listener,
            first.acceptor,
            JoinAdmissionReply::Refused {
                refusal: JoinDoorRefusal::TokenNotFound { token_id },
            },
        ));
        let second_task = tokio::spawn(serve_join_reply(
            second_listener,
            second_acceptor,
            JoinAdmissionReply::Refused {
                refusal: JoinDoorRefusal::NameConflict {
                    name: "edge-a".to_owned(),
                },
            },
        ));

        let error = JoinDoorClient::default()
            .admit_machine(&blob, machine_request())
            .await
            .expect_err("second door refusal");
        assert!(matches!(
            error,
            JoinDoorClientError::Refused {
                refusal: JoinDoorRefusal::NameConflict { .. }
            }
        ));
        first_task.await.expect("first door task");
        second_task.await.expect("second door task");
    }

    #[tokio::test]
    async fn public_machine_and_peer_methods_preserve_their_typed_request_paths() {
        let door = door_fixture_at(ployz_core::join::JOIN_DOOR_PORT).await;
        let endpoint = door.listener.local_addr().expect("door address");
        let fingerprint = JoinDoorCertFingerprint::try_new(
            door.fingerprint
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        )
        .expect("door fingerprint");
        let blob = JoinBlob::try_new(
            TokenId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("token id"),
            JoinTokenSecret::try_from_bytes([0x5a; 32]),
            fingerprint,
            vec![endpoint],
        )
        .expect("join blob");
        let acceptor = door.acceptor;
        let listener = door.listener;
        let server = tokio::spawn(async move {
            for expected_kind in ["machine", "peer"] {
                let (tcp, _) = listener.accept().await.expect("accept TCP");
                let mut tls = acceptor.accept(tcp).await.expect("accept TLS");
                let request = read_http_request(&mut tls).await;
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .expect("HTTP body");
                let request: JoinAdmissionRequest =
                    serde_json::from_str(body).expect("typed admission request");
                match (expected_kind, request.member) {
                    ("machine", JoinMemberRequest::Machine { .. })
                    | ("peer", JoinMemberRequest::Peer { .. }) => {}
                    (_, actual) => panic!("unexpected admission path: {actual:?}"),
                }
                let reply = match expected_kind {
                    "machine" => JoinAdmissionReply::Refused {
                        refusal: JoinDoorRefusal::NameConflict {
                            name: "edge-a".to_owned(),
                        },
                    },
                    "peer" => JoinAdmissionReply::Refused {
                        refusal: JoinDoorRefusal::IdentityConflict,
                    },
                    _ => unreachable!("test has exactly two request kinds"),
                };
                let body = serde_json::to_vec(&reply).expect("reply JSON");
                tls.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write reply head");
                tls.write_all(&body).await.expect("write reply body");
            }
        });

        let client = JoinDoorClient::default();
        let machine_error = client
            .admit_machine(
                &blob,
                MachineJoinRequest {
                    machine_id: MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW")
                        .expect("machine id"),
                    name: MachineName::try_new("edge-a").expect("machine name"),
                    public_key: WireGuardPublicKey::try_new(
                        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                    )
                    .expect("public key"),
                    endpoint: None,
                    storage_choice: JoinStorageChoice::Automatic,
                    storage_facts: JoinStorageFacts {
                        imported_zfs_pool: false,
                        total_memory_bytes: 1024,
                    },
                },
            )
            .await
            .expect_err("machine refusal");
        assert!(matches!(
            machine_error,
            JoinDoorClientError::Refused {
                refusal: JoinDoorRefusal::NameConflict { .. }
            }
        ));

        let peer_error = client
            .admit_peer(
                &blob,
                PeerJoinRequest {
                    peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("peer id"),
                    name: "operator-laptop".to_owned(),
                    public_key: WireGuardPublicKey::try_new(
                        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                    )
                    .expect("public key"),
                    endpoint: None,
                },
            )
            .await
            .expect_err("peer refusal");
        assert!(matches!(
            peer_error,
            JoinDoorClientError::Refused {
                refusal: JoinDoorRefusal::IdentityConflict
            }
        ));
        server.await.expect("door task");
    }

    #[tokio::test]
    async fn pin_mismatch_prevents_the_token_bearing_http_request() {
        let door = door_fixture().await;
        let endpoint = door.listener.local_addr().expect("door address");
        let server = tokio::spawn(async move {
            let (tcp, _) = door.listener.accept().await.expect("accept TCP");
            assert!(door.acceptor.accept(tcp).await.is_err());
        });
        let client = JoinDoorClient::new(
            Duration::from_secs(1),
            Duration::from_secs(2),
            MAX_MESH_API_RESPONSE_BYTES,
        );
        let error = client
            .request_pinned::<_, TypedReply>(
                &[endpoint],
                [0_u8; 32],
                &SecretRequest {
                    token: "pzjoin_must-not-cross-the-wire",
                    kind: "machine",
                },
            )
            .await
            .expect_err("wrong fingerprint");
        assert!(matches!(
            error,
            JoinDoorClientError::AllEndpointsFailed { attempts }
                if matches!(attempts.as_slice(), [JoinDoorAttempt {
                    failure: JoinDoorAttemptFailure::FingerprintMismatch,
                    ..
                }])
        ));
        server.await.expect("door task");
    }

    #[tokio::test]
    async fn tries_advertised_endpoints_in_order_and_decodes_a_typed_reply() {
        let door = door_fixture().await;
        let good_endpoint = door.listener.local_addr().expect("door address");
        let unavailable = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve unavailable endpoint")
            .local_addr()
            .expect("unavailable address");
        let fingerprint = door.fingerprint;
        let server = tokio::spawn(async move {
            let (tcp, _) = door.listener.accept().await.expect("accept TCP");
            let mut tls = door.acceptor.accept(tcp).await.expect("accept TLS");
            let request = read_http_request(&mut tls).await;
            assert!(request.starts_with("POST /join HTTP/1.1\r\n"));
            assert!(request.ends_with("\r\n\r\n{\"token\":\"pzjoin_example\",\"kind\":\"peer\"}"));
            let body = br#"{"accepted_as":"peer"}"#;
            tls.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .expect("write head");
            tls.write_all(body).await.expect("write body");
        });
        let client = JoinDoorClient::new(
            Duration::from_secs(1),
            Duration::from_secs(3),
            MAX_MESH_API_RESPONSE_BYTES,
        );
        let reply = client
            .request_pinned::<_, TypedReply>(
                &[unavailable, good_endpoint],
                fingerprint,
                &SecretRequest {
                    token: "pzjoin_example",
                    kind: "peer",
                },
            )
            .await
            .expect("fallback door answers");
        assert_eq!(
            reply,
            TypedReply {
                accepted_as: "peer".to_owned()
            }
        );
        server.await.expect("door task");
    }

    #[tokio::test]
    async fn overall_deadline_bounds_stalled_endpoints() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind stalled endpoint");
        let endpoint = listener.local_addr().expect("stalled address");
        let server = tokio::spawn(async move {
            let (_tcp, _) = listener.accept().await.expect("accept TCP");
            std::future::pending::<()>().await;
        });
        let client = JoinDoorClient::new(
            Duration::from_secs(1),
            Duration::from_millis(30),
            MAX_MESH_API_RESPONSE_BYTES,
        );
        let error = client
            .request_pinned::<_, TypedReply>(
                &[endpoint],
                [0_u8; 32],
                &SecretRequest {
                    token: "pzjoin_bounded",
                    kind: "machine",
                },
            )
            .await
            .expect_err("overall bound");
        assert!(matches!(error, JoinDoorClientError::OverallTimedOut));
        server.abort();
    }

    async fn read_http_request<Stream>(stream: &mut Stream) -> String
    where
        Stream: tokio::io::AsyncRead + Unpin,
    {
        let mut request = Vec::new();
        let header_end = loop {
            let mut byte = [0];
            stream.read_exact(&mut byte).await.expect("read request");
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break request.len();
            }
        };
        let headers = String::from_utf8(
            request
                .get(..header_end)
                .expect("header boundary comes from the request length")
                .to_vec(),
        )
        .expect("headers");
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .expect("content length");
        request.resize(header_end + content_length, 0);
        let body = request
            .get_mut(header_end..)
            .expect("resized request contains the declared body range");
        stream.read_exact(body).await.expect("read body");
        String::from_utf8(request).expect("request text")
    }
}
