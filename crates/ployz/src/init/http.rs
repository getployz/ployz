//! Bounded bootstrap-credentialed access to machine one's local API.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::init::orchestration::{FoundingControlPlane, FoundingReadiness};
use futures_util::StreamExt as _;
use ployz_core::founding::ValidatedFoundingRequest;
use ployz_core::founding::{FoundingRefusal, FoundingRequest, FoundingResult};
use ployz_core::ids::MachineRowId;
use ployz_core::operation::FailureMessage;
use ployz_core::{
    API_MAJOR, ApiFeature, ApiRefusal, ApiVersion, FOUNDING_ROUTE, KnownApiFeature, LensCollection,
    LensSnapshot, VERSION_ROUTE, lens_route,
};
use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const READINESS_TIMEOUT: Duration = Duration::from_secs(45);
const RETRY_INTERVAL: Duration = Duration::from_millis(250);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct BootstrapSecret(String);

impl BootstrapSecret {
    pub fn new(value: impl Into<String>) -> Result<Self, HttpFoundingError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 4_096
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(HttpFoundingError::InvalidBootstrapSecret);
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BootstrapSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BootstrapSecret([REDACTED])")
    }
}

#[derive(Clone)]
pub struct HttpFoundingControlPlane {
    client: Client,
    base_url: String,
    bootstrap_secret: BootstrapSecret,
}

impl fmt::Debug for HttpFoundingControlPlane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpFoundingControlPlane")
            .field("base_url", &self.base_url)
            .field("bootstrap_secret", &self.bootstrap_secret)
            .finish_non_exhaustive()
    }
}

impl HttpFoundingControlPlane {
    pub fn new(
        listen_addr: SocketAddr,
        source_ip: IpAddr,
        bootstrap_secret: BootstrapSecret,
    ) -> Result<Self, HttpFoundingError> {
        if source_ip.is_unspecified() {
            return Err(HttpFoundingError::InvalidSourceIp);
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .local_address(source_ip)
            .build()
            .map_err(|error| HttpFoundingError::Client(error.to_string()))?;
        Ok(Self {
            client,
            base_url: format!("http://{listen_addr}"),
            bootstrap_secret,
        })
    }

    pub async fn submit_founding(
        &self,
        request: &FoundingRequest,
    ) -> Result<FoundingResult, HttpFoundingError> {
        let deadline = tokio::time::Instant::now() + READINESS_TIMEOUT;
        loop {
            match self.submit_founding_once(request).await {
                Ok(result) => return Ok(result),
                Err(HttpFoundingError::Transport(_))
                | Err(HttpFoundingError::ApiRefused(ApiRefusal::CorrosionUnavailable { .. })) => {}
                Err(error) => return Err(error),
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(HttpFoundingError::ReadinessTimedOut);
            }
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    }

    async fn submit_founding_once(
        &self,
        request: &FoundingRequest,
    ) -> Result<FoundingResult, HttpFoundingError> {
        let response = self
            .client
            .post(self.url(FOUNDING_ROUTE))
            .bearer_auth(self.bootstrap_secret.expose())
            .json(request)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        let body = bounded_body(response).await?;
        if status.is_success() {
            return decode(&body, "founding result");
        }
        if let Ok(refusal) = serde_json::from_slice::<FoundingRefusal>(&body) {
            return Err(HttpFoundingError::FoundingRefused(refusal));
        }
        if let Ok(refusal) = serde_json::from_slice::<ApiRefusal>(&body) {
            return Err(HttpFoundingError::ApiRefused(refusal));
        }
        Err(HttpFoundingError::UnexpectedStatus { status })
    }

    /// Waits on the public machines fold after the founding POST has ensured
    /// the Docker endpoint network. This is a convergence barrier, not direct
    /// Docker inspection.
    pub async fn await_endpoint_network_ready(
        &self,
        machine_id: &MachineRowId,
    ) -> Result<(), HttpFoundingError> {
        self.retry_until_ready(|snapshot| match snapshot {
            LensSnapshot::Machines { rows, .. } => rows.iter().any(|row| row.id == *machine_id),
            LensSnapshot::Services { .. }
            | LensSnapshot::Containers { .. }
            | LensSnapshot::MachineStatus { .. }
            | LensSnapshot::Operations { .. } => false,
        })
        .await
    }

    /// Verifies the ordinary, post-bootstrap API surface and founding capability.
    pub async fn verify_bootstrap_readiness(&self) -> Result<(), HttpFoundingError> {
        let _snapshot = self
            .get_json::<LensSnapshot>(&lens_route(LensCollection::Machines))
            .await?;
        self.verify_version().await
    }

    pub async fn verify_ordinary_readiness(&self) -> Result<(), HttpFoundingError> {
        self.verify_version().await?;
        let response = self
            .client
            .post(self.url(FOUNDING_ROUTE))
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        let body = bounded_body(response).await?;
        match serde_json::from_slice::<ApiRefusal>(&body) {
            Ok(ApiRefusal::UnsupportedRoute) if status == StatusCode::NOT_FOUND => Ok(()),
            Ok(refusal) => Err(HttpFoundingError::ApiRefused(refusal)),
            Err(_) => Err(HttpFoundingError::BootstrapRouteStillEnabled { status }),
        }
    }

    async fn verify_version(&self) -> Result<(), HttpFoundingError> {
        let deadline = tokio::time::Instant::now() + READINESS_TIMEOUT;
        loop {
            match self.get_json::<ApiVersion>(VERSION_ROUTE).await {
                Ok(version)
                    if version.major == API_MAJOR
                        && version.features.iter().any(|feature| {
                            matches!(feature, ApiFeature::Known(KnownApiFeature::Founding))
                        }) =>
                {
                    return Ok(());
                }
                Ok(_) | Err(HttpFoundingError::Transport(_)) => {}
                Err(error) => return Err(error),
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(HttpFoundingError::ReadinessTimedOut);
            }
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    }

    async fn retry_until_ready(
        &self,
        predicate: impl Fn(&LensSnapshot) -> bool,
    ) -> Result<(), HttpFoundingError> {
        let deadline = tokio::time::Instant::now() + READINESS_TIMEOUT;
        loop {
            match self
                .get_json::<LensSnapshot>(&lens_route(LensCollection::Machines))
                .await
            {
                Ok(snapshot) if predicate(&snapshot) => return Ok(()),
                Ok(_) | Err(HttpFoundingError::Transport(_)) => {}
                Err(HttpFoundingError::ApiRefused(ApiRefusal::CorrosionUnavailable { .. })) => {}
                Err(error) => return Err(error),
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(HttpFoundingError::ReadinessTimedOut);
            }
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, HttpFoundingError> {
        let response = self
            .client
            .get(self.url(path))
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        let body = bounded_body(response).await?;
        if status.is_success() {
            return decode(&body, "API response");
        }
        if let Ok(refusal) = serde_json::from_slice::<ApiRefusal>(&body) {
            return Err(HttpFoundingError::ApiRefused(refusal));
        }
        Err(HttpFoundingError::UnexpectedStatus { status })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

impl FoundingControlPlane for HttpFoundingControlPlane {
    async fn submit_founding(
        &mut self,
        request: &ValidatedFoundingRequest,
    ) -> Result<(), FailureMessage> {
        HttpFoundingControlPlane::submit_founding(self, request.request())
            .await
            .map(|_| ())
            .map_err(failure_message)
    }

    async fn await_endpoint_network_ready(
        &mut self,
        request: &ValidatedFoundingRequest,
    ) -> Result<(), FailureMessage> {
        HttpFoundingControlPlane::await_endpoint_network_ready(self, &request.request().machine_id)
            .await
            .map_err(failure_message)
    }

    async fn verify_readiness(
        &mut self,
        readiness: FoundingReadiness,
    ) -> Result<(), FailureMessage> {
        match readiness {
            FoundingReadiness::Bootstrap => self.verify_bootstrap_readiness().await,
            FoundingReadiness::Ordinary => self.verify_ordinary_readiness().await,
        }
        .map_err(failure_message)
    }
}

fn failure_message(error: HttpFoundingError) -> FailureMessage {
    FailureMessage::try_new(error.to_string()).expect("HTTP founding failures are nonempty")
}

fn transport_error(error: reqwest::Error) -> HttpFoundingError {
    HttpFoundingError::Transport(error.to_string())
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>, HttpFoundingError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(HttpFoundingError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(transport_error)?;
        let Some(total) = body.len().checked_add(chunk.len()) else {
            return Err(HttpFoundingError::ResponseTooLarge);
        };
        if total > MAX_RESPONSE_BYTES {
            return Err(HttpFoundingError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn decode<T: DeserializeOwned>(body: &[u8], subject: &'static str) -> Result<T, HttpFoundingError> {
    serde_json::from_slice(body).map_err(|error| HttpFoundingError::InvalidResponse {
        subject,
        detail: error.to_string(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum HttpFoundingError {
    #[error("bootstrap secret is invalid")]
    InvalidBootstrapSecret,
    #[error("local API source address must be concrete")]
    InvalidSourceIp,
    #[error("could not build the local API client: {0}")]
    Client(String),
    #[error("local API transport failed: {0}")]
    Transport(String),
    #[error("founding was refused: {0:?}")]
    FoundingRefused(FoundingRefusal),
    #[error("local API refused the request: {0:?}")]
    ApiRefused(ApiRefusal),
    #[error("local API returned unexpected HTTP status {status}")]
    UnexpectedStatus { status: StatusCode },
    #[error("local API response exceeded the bounded response size")]
    ResponseTooLarge,
    #[error("could not decode {subject}: {detail}")]
    InvalidResponse {
        subject: &'static str,
        detail: String,
    },
    #[error("local API readiness did not converge before the deadline")]
    ReadinessTimedOut,
    #[error(
        "bootstrap-only founding route remained enabled after credential removal (HTTP {status})"
    )]
    BootstrapRouteStillEnabled { status: StatusCode },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    use ployz_core::corrosion::{
        AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp,
        MachineDocument, MachineStorageSelection, MachineStorageSelectionReason, MachineTransport,
        MeshProvider, OperationInitiator, OperatorWriteProvenance, StorageMode,
        derive_builtin_wireguard_member,
    };
    use ployz_core::founding::{FoundingDriverEnrollment, FoundingRequest};
    use ployz_core::ids::{ClusterId, MachineRowId};
    use ployz_core::machine::{MachineLifecycle, MachineName};
    use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    fn request() -> FoundingRequest {
        let cluster_id = ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id");
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("machine id");
        let provenance = OperatorWriteProvenance {
            written_by: OperationInitiator::Machine {
                machine_id: machine_id.clone(),
            },
            written_at: CorrosionTimestamp::try_new("2026-08-04T12:00:00Z").expect("timestamp"),
        };
        let pubkey = WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .expect("public key");
        FoundingRequest {
            cluster_id: cluster_id.clone(),
            cluster: ClusterDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: cluster_id.clone(),
                provenance: provenance.clone(),
                name: "ares".to_owned(),
                storage_default: StorageMode::Plain,
                hostname_mode: AutomaticHostnameMode::Disabled,
                prefix: MachineEndpointSupernet::default_v1(),
                provider: MeshProvider::BuiltinWireguard,
                acme_directory_url: "https://acme.example/directory".to_owned(),
                acme_contact: None,
            },
            machine_id,
            machine: MachineDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: cluster_id.clone(),
                provenance,
                name: MachineName::try_new("ares").expect("machine name"),
                lifecycle: MachineLifecycle::Active,
                transport: MachineTransport::Wireguard {
                    addr_v6: derive_builtin_wireguard_member(&cluster_id, &pubkey)
                        .bind_address()
                        .get(),
                    pubkey,
                    endpoint: None,
                    subnet_v4: MachineEndpointSubnet::try_new("10.210.0.0/24").expect("subnet"),
                },
                storage: MachineStorageSelection {
                    mode: StorageMode::Plain,
                    reason: MachineStorageSelectionReason::Default,
                },
            },
            driver: FoundingDriverEnrollment::OnHost,
        }
    }

    async fn mock_server(responses: Vec<(u16, Vec<u8>)>) -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("mock bind");
        let address = listener.local_addr().expect("mock address");
        tokio::spawn(async move {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.expect("mock accept");
                let mut request = vec![0_u8; 64 * 1024];
                let _ = stream.read(&mut request).await.expect("mock read");
                let reason = match status {
                    200 => "OK",
                    404 => "Not Found",
                    503 => "Service Unavailable",
                    _ => "Error",
                };
                let head = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).await.expect("mock head");
                stream.write_all(&body).await.expect("mock body");
            }
        });
        address
    }

    fn client(address: SocketAddr) -> HttpFoundingControlPlane {
        HttpFoundingControlPlane::new(
            address,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            BootstrapSecret::new("bootstrap-secret").expect("secret"),
        )
        .expect("client")
    }

    #[test]
    fn bootstrap_secret_never_appears_in_debug_or_errors() {
        let secret = BootstrapSecret::new("the-secret").expect("secret");
        let client = HttpFoundingControlPlane::new(
            "127.0.0.1:9876".parse().expect("address"),
            "127.0.0.1".parse().expect("IP"),
            secret,
        )
        .expect("client");
        assert!(!format!("{client:?}").contains("the-secret"));
        assert!(
            !HttpFoundingError::InvalidBootstrapSecret
                .to_string()
                .contains("the-secret")
        );
    }

    #[tokio::test]
    async fn founding_retries_only_transient_api_failure() {
        let unavailable = serde_json::to_vec(&ApiRefusal::CorrosionUnavailable {
            retry_after_seconds: ployz_core::CorrosionRetryAfterSeconds::DEFAULT,
        })
        .expect("refusal JSON");
        let success = serde_json::to_vec(&FoundingResult::Found).expect("success JSON");
        let address = mock_server(vec![(503, unavailable), (200, success)]).await;

        assert_eq!(
            client(address)
                .submit_founding(&request())
                .await
                .expect("retry succeeds"),
            FoundingResult::Found
        );
    }

    #[tokio::test]
    async fn typed_state_refusal_is_terminal() {
        let refusal = serde_json::to_vec(&ApiRefusal::InvalidCluster).expect("refusal JSON");
        let address = mock_server(vec![(400, refusal)]).await;

        assert!(matches!(
            client(address).submit_founding(&request()).await,
            Err(HttpFoundingError::ApiRefused(ApiRefusal::InvalidCluster))
        ));
    }

    #[tokio::test]
    async fn public_fold_and_ordinary_route_refusal_prove_readiness() {
        let request = request();
        let lens = LensSnapshot::Machines {
            cluster: Box::new(request.cluster.clone()),
            rows: vec![ployz_core::MachineLensRow {
                id: request.machine_id.clone(),
                document: request.machine.clone(),
            }],
        };
        let version = ApiVersion::new("test", [ApiFeature::Known(KnownApiFeature::Founding)]);
        let unsupported = serde_json::to_vec(&ApiRefusal::UnsupportedRoute).expect("refusal JSON");
        let address = mock_server(vec![
            (200, serde_json::to_vec(&lens).expect("lens JSON")),
            (200, serde_json::to_vec(&version).expect("version JSON")),
            (404, unsupported),
        ])
        .await;
        let control = client(address);

        control
            .await_endpoint_network_ready(&request.machine_id)
            .await
            .expect("fold ready");
        control
            .verify_ordinary_readiness()
            .await
            .expect("ordinary route disabled");
    }
}
