use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ployz_core::certificateificate::{
    ManagedCertBundle, ManagedLeaseAcquired, ManagedLeaseRenewed,
};
use ployz_test_lease_worker::{Clock, ClockError, StubLeaseWorker, serve};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

#[tokio::test]
async fn http_worker_returns_pending_once_before_bundle_ready() {
    let clock = ManualClock::new(1_700_000_000);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let addr = listener.local_addr().expect("listener addr");
    let worker = Arc::new(Mutex::new(StubLeaseWorker::with_clock(clock.clone())));
    let server = tokio::spawn(serve(listener, worker));

    let acquired_response = request(
        addr,
        "POST",
        "/v1/leases",
        &[],
        r#"{"acquisition_id":"a1","token":"client-token","ipv4":[],"ipv6":[]}"#,
    )
    .await
    .expect("lease acquired");
    assert_eq!(acquired_response.status, 201);
    let acquired_json: serde_json::Value =
        serde_json::from_str(&acquired_response.body).expect("lease acquired response is JSON");
    assert_eq!(acquired_json.get("bundle"), Some(&serde_json::Value::Null));
    assert_eq!(acquired_json.as_object().map(serde_json::Map::len), Some(2));
    let acquired: ManagedLeaseAcquired =
        serde_json::from_value(acquired_json).expect("lease acquired response matches contract");
    assert_eq!(
        acquired.lease.expires_at.unix_seconds() - acquired.lease.issued_at.unix_seconds(),
        7_776_000
    );

    let unauthorized = request(
        addr,
        "GET",
        &format!("/v1/leases/{}/cert-bundle", acquired.lease.name.as_str()),
        &[("Authorization", "Bearer wrong-token".to_owned())],
        "",
    )
    .await
    .expect("unauthorized response");
    assert_eq!(unauthorized.status, 401);

    let pending = request(
        addr,
        "GET",
        &format!("/v1/leases/{}/cert-bundle", acquired.lease.name.as_str()),
        &[(
            "Authorization",
            format!("Bearer {}", acquired.lease.token.as_str()),
        )],
        "",
    )
    .await
    .expect("pending response");
    assert_eq!(pending.status, 202);
    assert_eq!(pending.body, r#"{"status":"pending"}"#);

    let bundle: ManagedCertBundle = request_json(
        addr,
        "GET",
        &format!("/v1/leases/{}/cert-bundle", acquired.lease.name.as_str()),
        &[(
            "Authorization",
            format!("Bearer {}", acquired.lease.token.as_str()),
        )],
        "",
    )
    .await
    .expect("bundle downloaded");
    assert!(
        bundle
            .certificate_chain_pem
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    assert!(
        bundle
            .private_key_pem
            .starts_with("-----BEGIN PRIVATE KEY-----")
    );

    clock.advance(60);
    let renewed: ManagedLeaseRenewed = request_json(
        addr,
        "POST",
        &format!("/v1/leases/{}/renew", acquired.lease.name.as_str()),
        &[(
            "Authorization",
            format!("Bearer {}", acquired.lease.token.as_str()),
        )],
        r#"{"ipv4":["203.0.113.8"],"ipv6":["2001:db8::8"]}"#,
    )
    .await
    .expect("lease renewed");
    let renewed_bundle = renewed.bundle.expect("ready renewal returns bundle");
    assert_ne!(renewed_bundle.digest, bundle.digest);
    assert!(renewed.lease.issued_at > acquired.lease.issued_at);
    assert!(renewed.lease.expires_at > acquired.lease.expires_at);

    server.abort();
}

#[tokio::test]
async fn http_worker_renew_returns_no_bundle_while_issuance_is_pending() {
    let clock = ManualClock::new(1_700_000_000);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let addr = listener.local_addr().expect("listener addr");
    let worker = Arc::new(Mutex::new(StubLeaseWorker::with_clock(clock.clone())));
    let server = tokio::spawn(serve(listener, worker));

    let acquired_response = request(
        addr,
        "POST",
        "/v1/leases",
        &[],
        r#"{"acquisition_id":"a2","token":"client-token","ipv4":[],"ipv6":[]}"#,
    )
    .await
    .expect("lease acquired");
    let acquired: ManagedLeaseAcquired = serde_json::from_str(&acquired_response.body)
        .expect("lease acquired response matches contract");

    clock.advance(60);
    let renewed: ManagedLeaseRenewed = request_json(
        addr,
        "POST",
        &format!("/v1/leases/{}/renew", acquired.lease.name.as_str()),
        &[(
            "Authorization",
            format!("Bearer {}", acquired.lease.token.as_str()),
        )],
        r#"{"ipv4":["203.0.113.8"],"ipv6":[]}"#,
    )
    .await
    .expect("lease renewed");
    assert!(renewed.bundle.is_none());

    server.abort();
}

#[derive(Clone)]
struct ManualClock {
    now: Arc<AtomicU64>,
}

impl ManualClock {
    fn new(now: u64) -> Self {
        Self {
            now: Arc::new(AtomicU64::new(now)),
        }
    }

    fn advance(&self, seconds: u64) {
        self.now.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_seconds(&self) -> Result<u64, ClockError> {
        Ok(self.now.load(Ordering::SeqCst))
    }
}

struct HttpResponse {
    status: u16,
    body: String,
}

async fn request_json<T: serde::de::DeserializeOwned>(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> Result<T, TestHttpError> {
    let response = request(addr, method, path, headers, body).await?;
    if response.status != 200 {
        return Err(TestHttpError::UnexpectedStatus(response.status));
    }

    Ok(serde_json::from_str(&response.body)?)
}

async fn request(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> Result<HttpResponse, TestHttpError> {
    let mut stream = TcpStream::connect(addr).await?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body);

    stream.write_all(request.as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;

    parse_response(&response)
}

fn parse_response(response: &str) -> Result<HttpResponse, TestHttpError> {
    let Some((head, body)) = response.split_once("\r\n\r\n") else {
        return Err(TestHttpError::MalformedResponse);
    };
    let Some(status) = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
    else {
        return Err(TestHttpError::MalformedResponse);
    };

    Ok(HttpResponse {
        status,
        body: body.to_owned(),
    })
}

#[derive(Debug, thiserror::Error)]
enum TestHttpError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("unexpected status {0}")]
    UnexpectedStatus(u16),
    #[error("malformed HTTP response")]
    MalformedResponse,
}
