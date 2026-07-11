#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::cert::{
    LeaseBearerToken, LeaseExpiresAt, LeaseIssuedAt, ManagedCertBundle, ManagedLeaseAcquireRequest,
    ManagedLeaseAcquired, ManagedLeaseName, ManagedLeaseRecord, ManagedLeaseRenewed,
};
use rcgen::CertifiedKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

pub const STUB_LEASE_TTL_SECONDS: u64 = 90 * 24 * 60 * 60;

pub trait Clock {
    fn now_seconds(&self) -> Result<u64, ClockError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_seconds(&self) -> Result<u64, ClockError> {
        Ok(SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ClockError::BeforeUnixEpoch)?
            .as_secs())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClockError {
    #[error("system clock is before unix epoch")]
    BeforeUnixEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseWorkerRequest {
    Acquire(ManagedLeaseAcquireRequest),
    Renew {
        lease: ManagedLeaseName,
        token: LeaseBearerToken,
        request: ManagedLeaseAcquireRequest,
    },
    DownloadBundle {
        lease: ManagedLeaseName,
        token: LeaseBearerToken,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseWorkerResponse {
    LeaseAcquired(ManagedLeaseAcquired),
    LeaseRenewed(ManagedLeaseRenewed),
    BundlePending,
    Bundle(ManagedCertBundle),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeaseWorkerError {
    #[error("managed lease not found")]
    LeaseNotFound,
    #[error("lease bearer token does not match")]
    InvalidBearerToken,
    #[error("managed certificate issue failed: {message}")]
    CertIssue { message: String },
    #[error("{0}")]
    Lease(#[from] ployz_core::cert::ManagedLeaseError),
    #[error("{0}")]
    LeaseTimestamp(#[from] ployz_core::cert::LeaseTimestampError),
    #[error("{0}")]
    Clock(#[from] ClockError),
}

pub struct StubLeaseWorker<C = SystemClock> {
    records: BTreeMap<ManagedLeaseName, ManagedLeaseRecord>,
    bundles: BTreeMap<ManagedLeaseName, ManagedCertBundle>,
    pending_bundles: BTreeSet<ManagedLeaseName>,
    renewal_requests: BTreeMap<ManagedLeaseName, ManagedLeaseAcquireRequest>,
    clock: C,
}

impl StubLeaseWorker<SystemClock> {
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(SystemClock)
    }
}

impl<C: Clock> StubLeaseWorker<C> {
    #[must_use]
    pub fn with_clock(clock: C) -> Self {
        Self {
            records: BTreeMap::new(),
            bundles: BTreeMap::new(),
            pending_bundles: BTreeSet::new(),
            renewal_requests: BTreeMap::new(),
            clock,
        }
    }

    pub fn handle(
        &mut self,
        request: LeaseWorkerRequest,
    ) -> Result<LeaseWorkerResponse, LeaseWorkerError> {
        match request {
            LeaseWorkerRequest::Acquire(request) => self.acquire(request),
            LeaseWorkerRequest::Renew {
                lease,
                token,
                request,
            } => self.renew(lease, &token, request),
            LeaseWorkerRequest::DownloadBundle { lease, token } => {
                self.download_bundle(&lease, &token)
            }
        }
    }

    fn acquire(
        &mut self,
        request: ManagedLeaseAcquireRequest,
    ) -> Result<LeaseWorkerResponse, LeaseWorkerError> {
        let ManagedLeaseAcquireRequest { ipv4: _, ipv6: _ } = request;
        let lease = self.next_lease_name()?;
        let record = self.issue_record(lease.clone(), self.next_token()?)?;
        let bundle = self.issue_bundle(&record)?;
        self.records.insert(lease.clone(), record.clone());
        self.bundles.insert(lease.clone(), bundle);
        self.pending_bundles.insert(lease);

        Ok(LeaseWorkerResponse::LeaseAcquired(ManagedLeaseAcquired {
            lease: record,
            bundle: None,
        }))
    }

    fn renew(
        &mut self,
        lease: ManagedLeaseName,
        token: &LeaseBearerToken,
        request: ManagedLeaseAcquireRequest,
    ) -> Result<LeaseWorkerResponse, LeaseWorkerError> {
        let current = self.record(&lease)?.clone();
        verify_token(&current, token)?;
        self.renewal_requests.insert(lease.clone(), request);
        let renewed = self.issue_record(lease.clone(), current.token.clone())?;
        let bundle = self.issue_bundle(&renewed)?;
        let response_bundle = if self.pending_bundles.contains(&lease) {
            None
        } else {
            Some(bundle.clone())
        };
        self.records.insert(lease.clone(), renewed.clone());
        self.bundles.insert(lease, bundle);

        Ok(LeaseWorkerResponse::LeaseRenewed(ManagedLeaseRenewed {
            lease: renewed,
            bundle: response_bundle,
        }))
    }

    #[must_use]
    pub fn renewal_request(&self, lease: &ManagedLeaseName) -> Option<&ManagedLeaseAcquireRequest> {
        self.renewal_requests.get(lease)
    }

    fn download_bundle(
        &mut self,
        lease: &ManagedLeaseName,
        token: &LeaseBearerToken,
    ) -> Result<LeaseWorkerResponse, LeaseWorkerError> {
        verify_token(self.record(lease)?, token)?;
        if self.pending_bundles.remove(lease) {
            return Ok(LeaseWorkerResponse::BundlePending);
        }

        Ok(LeaseWorkerResponse::Bundle(self.bundle(lease)?.clone()))
    }

    fn issue_record(
        &self,
        lease: ManagedLeaseName,
        token: LeaseBearerToken,
    ) -> Result<ManagedLeaseRecord, LeaseWorkerError> {
        let issued_at = self.now()?;
        ManagedLeaseRecord::try_new(
            lease,
            token,
            LeaseIssuedAt::try_new(issued_at)?,
            LeaseExpiresAt::try_new(issued_at + STUB_LEASE_TTL_SECONDS)?,
        )
        .map_err(Into::into)
    }

    fn issue_bundle(
        &mut self,
        record: &ManagedLeaseRecord,
    ) -> Result<ManagedCertBundle, LeaseWorkerError> {
        let [wildcard, apex] = record.name.wildcard_and_apex();
        let CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed([wildcard.clone(), apex.clone()]).map_err(
                |error| LeaseWorkerError::CertIssue {
                    message: error.to_string(),
                },
            )?;

        Ok(ManagedCertBundle::try_new(
            record.name.clone(),
            [wildcard, apex],
            cert.pem(),
            signing_key.serialize_pem(),
            record.issued_at,
            record.expires_at,
        )?)
    }

    fn record(&self, lease: &ManagedLeaseName) -> Result<&ManagedLeaseRecord, LeaseWorkerError> {
        self.records
            .get(lease)
            .ok_or(LeaseWorkerError::LeaseNotFound)
    }

    fn bundle(&self, lease: &ManagedLeaseName) -> Result<&ManagedCertBundle, LeaseWorkerError> {
        self.bundles
            .get(lease)
            .ok_or(LeaseWorkerError::LeaseNotFound)
    }

    fn next_lease_name(&self) -> Result<ManagedLeaseName, LeaseWorkerError> {
        Ok(ManagedLeaseName::try_new(format!(
            "l-{}",
            nuid::next().to_ascii_lowercase()
        ))?)
    }

    fn next_token(&self) -> Result<LeaseBearerToken, LeaseWorkerError> {
        Ok(LeaseBearerToken::try_new(format!("t-{}", nuid::next()))?)
    }

    fn now(&self) -> Result<u64, LeaseWorkerError> {
        Ok(self.clock.now_seconds()?)
    }
}

impl Default for StubLeaseWorker {
    fn default() -> Self {
        Self::new()
    }
}

fn verify_token(
    record: &ManagedLeaseRecord,
    token: &LeaseBearerToken,
) -> Result<(), LeaseWorkerError> {
    if tokens_match(record.token.as_str(), token.as_str()) {
        return Ok(());
    }

    Err(LeaseWorkerError::InvalidBearerToken)
}

fn tokens_match(expected: &str, actual: &str) -> bool {
    if expected.len() != actual.len() {
        return false;
    }

    expected
        .bytes()
        .zip(actual.bytes())
        .fold(0_u8, |diff, (left, right)| diff | (left ^ right))
        == 0
}

pub async fn serve<C>(
    listener: TcpListener,
    worker: Arc<Mutex<StubLeaseWorker<C>>>,
) -> Result<(), LeaseWorkerHttpError>
where
    C: Clock + Send + 'static,
{
    loop {
        let (stream, _) = listener.accept().await.map_err(LeaseWorkerHttpError::Io)?;
        let worker = Arc::clone(&worker);
        tokio::spawn(async move {
            let _ = serve_connection(stream, worker).await;
        });
    }
}

async fn serve_connection<C>(
    mut stream: TcpStream,
    worker: Arc<Mutex<StubLeaseWorker<C>>>,
) -> Result<(), LeaseWorkerHttpError>
where
    C: Clock,
{
    let request = read_request(&mut stream).await?;
    let response = match request_to_worker(&request) {
        Ok(worker_request) => {
            let mut worker = worker.lock().await;
            response_from_worker(worker.handle(worker_request))
        }
        Err(error) => json_response(400, &error.to_string()),
    };
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(LeaseWorkerHttpError::Io)
}

fn request_to_worker(request: &HttpRequest) -> Result<LeaseWorkerRequest, LeaseWorkerHttpError> {
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/v1/leases") => Ok(LeaseWorkerRequest::Acquire(serde_json::from_slice(
            &request.body,
        )?)),
        ("POST", path) if path.starts_with("/v1/leases/") && path.ends_with("/renew") => {
            let lease = path
                .trim_start_matches("/v1/leases/")
                .trim_end_matches("/renew")
                .trim_end_matches('/');
            let token = bearer_token(request)?;
            let renewal_request = serde_json::from_slice(&request.body)?;
            Ok(LeaseWorkerRequest::Renew {
                lease: ManagedLeaseName::try_new(lease)?,
                token,
                request: renewal_request,
            })
        }
        ("GET", path) if path.starts_with("/v1/leases/") && path.ends_with("/cert-bundle") => {
            let lease = path
                .trim_start_matches("/v1/leases/")
                .trim_end_matches("/cert-bundle")
                .trim_end_matches('/');
            Ok(LeaseWorkerRequest::DownloadBundle {
                lease: ManagedLeaseName::try_new(lease)?,
                token: bearer_token(request)?,
            })
        }
        _ => Err(LeaseWorkerHttpError::NotFound),
    }
}

fn response_from_worker(result: Result<LeaseWorkerResponse, LeaseWorkerError>) -> String {
    match result {
        Ok(LeaseWorkerResponse::LeaseAcquired(response)) => json_response(201, &response),
        Ok(LeaseWorkerResponse::LeaseRenewed(response)) => json_response(200, &response),
        Ok(LeaseWorkerResponse::BundlePending) => {
            json_response(202, &BundlePendingResponse::Pending)
        }
        Ok(LeaseWorkerResponse::Bundle(response)) => json_response(200, &response),
        Err(LeaseWorkerError::InvalidBearerToken) => {
            json_response(401, "lease bearer token does not match")
        }
        Err(LeaseWorkerError::LeaseNotFound) => json_response(404, "managed lease not found"),
        Err(error) => json_response(400, &error.to_string()),
    }
}

#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum BundlePendingResponse {
    Pending,
}

fn bearer_token(request: &HttpRequest) -> Result<LeaseBearerToken, LeaseWorkerHttpError> {
    let Some(header) = request.header("authorization") else {
        return Err(LeaseWorkerHttpError::MissingBearerToken);
    };
    let Some(token) = header.strip_prefix("Bearer ") else {
        return Err(LeaseWorkerHttpError::MissingBearerToken);
    };

    Ok(LeaseBearerToken::try_new(token)?)
}

fn json_response(status: u16, value: &(impl serde::Serialize + ?Sized)) -> String {
    let body =
        serde_json::to_string(value).unwrap_or_else(|_| "\"serialization failed\"".to_owned());
    let reason = match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header_name, _)| header_name == name)
            .map(|(_, value)| value.as_str())
    }
}

async fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, LeaseWorkerHttpError> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    let header_end = loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(LeaseWorkerHttpError::Io)?;
        if read == 0 {
            return Err(LeaseWorkerHttpError::IncompleteRequest);
        }
        let Some(read_bytes) = chunk.get(..read) else {
            return Err(LeaseWorkerHttpError::IncompleteRequest);
        };
        bytes.extend_from_slice(read_bytes);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };

    let Some(head_bytes) = bytes.get(..header_end) else {
        return Err(LeaseWorkerHttpError::IncompleteRequest);
    };
    let head = String::from_utf8(head_bytes.to_vec()).map_err(LeaseWorkerHttpError::Utf8)?;
    let mut lines = head.split("\r\n");
    let Some(request_line) = lines.next() else {
        return Err(LeaseWorkerHttpError::IncompleteRequest);
    };
    let mut request_parts = request_line.split_whitespace();
    let Some(method) = request_parts.next() else {
        return Err(LeaseWorkerHttpError::IncompleteRequest);
    };
    let Some(path) = request_parts.next() else {
        return Err(LeaseWorkerHttpError::IncompleteRequest);
    };
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<Vec<_>>();
    let content_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(LeaseWorkerHttpError::Io)?;
        if read == 0 {
            return Err(LeaseWorkerHttpError::IncompleteRequest);
        }
        let Some(read_bytes) = chunk.get(..read) else {
            return Err(LeaseWorkerHttpError::IncompleteRequest);
        };
        bytes.extend_from_slice(read_bytes);
    }
    let Some(body) = bytes.get(header_end..header_end + content_length) else {
        return Err(LeaseWorkerHttpError::IncompleteRequest);
    };

    Ok(HttpRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        body: body.to_vec(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseWorkerHttpError {
    #[error("invalid listen address: {0}")]
    ListenAddr(#[from] std::net::AddrParseError),
    #[error("{0}")]
    Io(std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Lease(#[from] ployz_core::cert::ManagedLeaseError),
    #[error("bearer token is required")]
    MissingBearerToken,
    #[error("route not found")]
    NotFound,
    #[error("incomplete HTTP request")]
    IncompleteRequest,
    #[error("HTTP request is not valid UTF-8: {0}")]
    Utf8(std::string::FromUtf8Error),
}
