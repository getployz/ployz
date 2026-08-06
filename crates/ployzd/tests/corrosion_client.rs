use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{StreamExt, stream};
use http_body_util::{BodyExt, StreamBody, combinators::UnsyncBoxBody};
use hyper::body::{Frame, Incoming};
use hyper::header::AUTHORIZATION;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use ployz_core::corrosion::{
    ChangeId, CorrosionHealthResponse, SqliteParameter, SqliteValue, Statement,
};
use ployzd::corrosion::{
    BearerToken, CorrosionClient, CorrosionClientBounds, CorrosionClientConfig,
    CorrosionClientError, CorrosionHealthReadError, QueryStreamEvent, SubscriptionStreamEvent,
};
use tokio::net::TcpListener;

const QUERY_ID: &str = "ba247cbc-2a7f-486b-873c-8a9620e72182";

#[tokio::test]
async fn health_uses_the_authenticated_v1_endpoint_and_decodes_success() {
    let server = FakeServer::start(vec![ResponseSpec::json(
        StatusCode::OK,
        r#"{"response":{"gaps":0,"members":3,"p99_lag":0.125,"queue_size":0}}"#,
    )])
    .await;
    let client = client(server.addr(), Duration::from_secs(1), 1024);

    assert_eq!(
        client.health().await,
        Ok(CorrosionHealthResponse::Response {
            gaps: 0,
            members: 3,
            p99_lag: 0.125,
            queue_size: 0,
        })
    );
    let requests = server.requests();
    let [request] = requests.as_slice() else {
        panic!("expected one request, got {}", requests.len());
    };
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path_and_query, "/v1/health");
    assert_eq!(
        request.headers.get(AUTHORIZATION).expect("authorization"),
        "Bearer test-token"
    );
}

#[tokio::test]
async fn health_preserves_the_exact_cold_start_503_as_a_reply() {
    let server = FakeServer::start(vec![ResponseSpec::json(
        StatusCode::SERVICE_UNAVAILABLE,
        r#"{"error":"no p99 lag information available"}"#,
    )])
    .await;
    let client = client(server.addr(), Duration::from_secs(1), 1024);

    assert_eq!(
        client.health().await,
        Ok(CorrosionHealthResponse::Error(
            "no p99 lag information available".to_owned()
        ))
    );
}

#[tokio::test]
async fn health_normalizes_noncanonical_responses_without_exposing_the_body() {
    let server = FakeServer::start(vec![ResponseSpec::json(
        StatusCode::SERVICE_UNAVAILABLE,
        r#"{"error":"database unavailable: bearer test-token"}"#,
    )])
    .await;
    let client = client(server.addr(), Duration::from_secs(1), 1024);

    let error = client
        .health()
        .await
        .expect_err("noncanonical health response is invalid");

    assert_eq!(error, CorrosionHealthReadError::InvalidResponse);
    assert!(!format!("{error:?}").contains("test-token"));
    assert!(!error.to_string().contains("test-token"));
}

#[tokio::test]
async fn execute_sends_auth_path_and_statement_body() {
    let server = FakeServer::start(vec![ResponseSpec::json(
        StatusCode::OK,
        r#"{"results":[{"rows_affected":1,"time":0.01}],"time":0.02}"#,
    )])
    .await;
    let client = client(server.addr(), Duration::from_secs(1), 1024);

    let response = client
        .execute(&[Statement::with_params(
            "INSERT INTO machines (id, document) VALUES (?, ?)",
            vec![
                SqliteParameter::Text("machine-id".to_owned()),
                SqliteParameter::Text("document".to_owned()),
            ],
        )])
        .await
        .expect("transaction succeeds");

    assert_eq!(response.results.len(), 1);
    let requests = server.requests();
    let [request] = requests.as_slice() else {
        panic!("expected one request, got {}", requests.len());
    };
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path_and_query, "/v1/transactions");
    assert_eq!(
        request.headers.get(AUTHORIZATION).expect("authorization"),
        "Bearer test-token"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).expect("request body is JSON"),
        serde_json::json!([[
            "INSERT INTO machines (id, document) VALUES (?, ?)",
            ["machine-id", "document"]
        ]])
    );
}

#[tokio::test]
async fn execute_reports_the_failing_statement_without_exposing_auth() {
    let server = FakeServer::start(vec![ResponseSpec::json(
        StatusCode::OK,
        r#"{"results":[{"rows_affected":1,"time":0.01},{"error":"constraint failed"}],"time":0.02}"#,
    )])
    .await;
    let client = client(server.addr(), Duration::from_secs(1), 1024);

    let error = client
        .execute(&[
            Statement::simple("INSERT one"),
            Statement::simple("INSERT two"),
        ])
        .await
        .expect_err("per-statement error fails the transaction");

    assert!(matches!(
        error,
        CorrosionClientError::StatementFailed { index: 1, ref detail }
            if detail == "constraint failed"
    ));
    assert!(!format!("{error:?}").contains("test-token"));
    assert!(!format!("{client:?}").contains("test-token"));
}

#[tokio::test]
async fn query_decodes_frames_split_at_arbitrary_byte_boundaries() {
    let server = FakeServer::start(vec![ResponseSpec::stream(
        StatusCode::OK,
        vec![
            Chunk::now(b"{\"col"),
            Chunk::now(b"umns\":[\"id\"]}\n{\"row\":[7,[\"machine"),
            Chunk::now(b"-id\"]]}\n{\"eoq\":{\"time\":0.1}}\n"),
        ],
    )])
    .await;
    let client = client(server.addr(), Duration::from_secs(1), 1024);

    let mut query = client
        .query(&Statement::simple("SELECT id FROM machines"))
        .await
        .expect("query opens");

    assert!(matches!(
        query.next().await.expect("columns frame"),
        Some(QueryStreamEvent::Columns(columns)) if columns == ["id"]
    ));
    assert!(matches!(
        query.next().await.expect("row frame"),
        Some(QueryStreamEvent::Row(row_id, values))
            if row_id.get() == 7 && values == [SqliteValue::Text("machine-id".to_owned())]
    ));
    assert!(matches!(
        query.next().await.expect("eoq frame"),
        Some(QueryStreamEvent::EndOfQuery(_))
    ));
    assert!(
        query
            .next()
            .await
            .expect("query completes after eoq")
            .is_none()
    );
}

#[tokio::test]
async fn query_rejects_oversized_truncated_and_fatal_frames() {
    let cases = [
        (
            ResponseSpec::text(StatusCode::OK, "{\"columns\":[\"0123456789\"]}\n"),
            ErrorKind::Oversized,
            20,
        ),
        (
            ResponseSpec::text(StatusCode::OK, "{\"columns\":[\"id\"]}"),
            ErrorKind::Truncated,
            64,
        ),
        (
            ResponseSpec::text(StatusCode::OK, "{\"error\":\"query failed\"}\n"),
            ErrorKind::Fatal,
            64,
        ),
    ];

    for (response, expected, frame_limit) in cases {
        let server = FakeServer::start(vec![response]).await;
        let client = client(server.addr(), Duration::from_secs(1), frame_limit);
        let mut query = client
            .query(&Statement::simple("SELECT id FROM machines"))
            .await
            .expect("query opens");
        let error = query.next().await.expect_err("invalid stream must fail");
        assert!(expected.matches(&error), "unexpected error: {error:?}");
    }
}

#[tokio::test]
async fn query_read_is_idle_timeout_bounded() {
    let server = FakeServer::start(vec![ResponseSpec::stream(
        StatusCode::OK,
        vec![Chunk::after(
            Duration::from_millis(200),
            b"{\"eoq\":{\"time\":0.1}}\n",
        )],
    )])
    .await;
    let client = client(server.addr(), Duration::from_millis(20), 1024);
    let mut query = client
        .query(&Statement::simple("SELECT id FROM machines"))
        .await
        .expect("query opens after response headers");

    let error = query.next().await.expect_err("idle stream times out");
    assert!(matches!(
        error,
        CorrosionClientError::StreamIdleTimeout { .. }
    ));
}

#[tokio::test]
async fn subscribe_captures_identity_and_resume_uses_safe_path_and_cursor() {
    let subscription = ResponseSpec::text(
        StatusCode::OK,
        "{\"eoq\":{\"time\":0.1,\"change_id\":0}}\n{\"change\":[\"insert\",7,[\"machine-id\"],1]}\n",
    )
    .header("corro-query-id", QUERY_ID)
    .header("corro-query-hash", "query-hash");
    let resumed = ResponseSpec::text(
        StatusCode::OK,
        "{\"change\":[\"update\",7,[\"machine-id\"],2]}\n",
    )
    .header("corro-query-id", QUERY_ID)
    .header("corro-query-hash", "query-hash");
    let server = FakeServer::start(vec![subscription, resumed]).await;
    let client = client(server.addr(), Duration::from_secs(1), 1024);

    let mut subscription = client
        .subscribe(&Statement::simple("SELECT id FROM machines"))
        .await
        .expect("subscription opens");
    assert_eq!(subscription.identity().id().to_string(), QUERY_ID);
    assert_eq!(subscription.identity().hash(), "query-hash");
    assert!(matches!(
        subscription.next().await.expect("eoq frame"),
        Some(SubscriptionStreamEvent::EndOfQuery(_))
    ));
    assert_eq!(
        subscription.last_contiguous_change_id(),
        Some(ChangeId::new(0))
    );
    assert!(matches!(
        subscription.next().await.expect("change frame"),
        Some(SubscriptionStreamEvent::Change(_, _, _, id)) if id == ChangeId::new(1)
    ));
    assert_eq!(
        subscription.last_contiguous_change_id(),
        Some(ChangeId::new(1))
    );

    let mut resumed = client
        .resume(subscription.identity(), ChangeId::new(1))
        .await
        .expect("subscription resumes");
    assert!(matches!(
        resumed.next().await.expect("resumed change"),
        Some(SubscriptionStreamEvent::Change(_, _, _, id)) if id == ChangeId::new(2)
    ));

    let requests = server.requests();
    let [subscribe_request, resume_request] = requests.as_slice() else {
        panic!("expected two requests, got {}", requests.len());
    };
    assert_eq!(
        subscribe_request.path_and_query,
        "/v1/subscriptions?skip_rows=false"
    );
    assert_eq!(
        resume_request.path_and_query,
        format!("/v1/subscriptions/{QUERY_ID}?skip_rows=true&from=1")
    );
}

#[tokio::test]
async fn subscription_rejects_non_contiguous_change_ids() {
    let server = FakeServer::start(vec![ResponseSpec::text(
        StatusCode::OK,
        "{\"eoq\":{\"time\":0.1,\"change_id\":4}}\n{\"change\":[\"insert\",7,[\"machine-id\"],6]}\n",
    )
    .header("corro-query-id", QUERY_ID)
    .header("corro-query-hash", "query-hash")])
    .await;
    let client = client(server.addr(), Duration::from_secs(1), 1024);
    let mut subscription = client
        .subscribe(&Statement::simple("SELECT id FROM machines"))
        .await
        .expect("subscription opens");
    _ = subscription.next().await.expect("eoq frame");

    let error = subscription
        .next()
        .await
        .expect_err("gap invalidates the stream");
    assert!(matches!(
        error,
        CorrosionClientError::NonContiguousChange { expected, actual }
            if expected == ChangeId::new(5) && actual == ChangeId::new(6)
    ));
    assert_eq!(
        subscription.last_contiguous_change_id(),
        Some(ChangeId::new(4))
    );
}

#[tokio::test]
async fn http_errors_have_capped_diagnostics() {
    let server = FakeServer::start(vec![ResponseSpec::text(
        StatusCode::SERVICE_UNAVAILABLE,
        "0123456789-secret-tail",
    )])
    .await;
    let client = configured_client(server.addr(), Duration::from_secs(1), 1024, 10);

    let error = client
        .execute(&[Statement::simple("INSERT one")])
        .await
        .expect_err("HTTP error fails");
    assert!(matches!(
        error,
        CorrosionClientError::HttpStatus {
            status: 503,
            ref detail,
            truncated: true,
        } if detail == "0123456789"
    ));
    assert!(!format!("{error:?}").contains("secret-tail"));
}

#[tokio::test]
async fn transaction_body_has_one_total_request_deadline() {
    let server = FakeServer::start(vec![ResponseSpec::stream(
        StatusCode::OK,
        vec![
            Chunk::after(Duration::from_millis(15), b"{\"results\":"),
            Chunk::after(Duration::from_millis(15), b"[{\"rows_affected\":1,"),
            Chunk::after(Duration::from_millis(15), b"\"time\":0.1}],\"time\":0.1}"),
        ],
    )])
    .await;
    let client = configured_client_with_request_timeout(
        server.addr(),
        Duration::from_millis(25),
        Duration::from_secs(1),
        1024,
        256,
    );

    let error = client
        .execute(&[Statement::simple("INSERT one")])
        .await
        .expect_err("trickling body exceeds the one request deadline");
    assert!(matches!(
        error,
        CorrosionClientError::RequestTimeout { timeout }
            if timeout == Duration::from_millis(25)
    ));
}

fn client(addr: SocketAddr, idle_timeout: Duration, max_frame_bytes: usize) -> CorrosionClient {
    configured_client(addr, idle_timeout, max_frame_bytes, 256)
}

fn configured_client(
    addr: SocketAddr,
    idle_timeout: Duration,
    max_frame_bytes: usize,
    max_error_body_bytes: usize,
) -> CorrosionClient {
    configured_client_with_request_timeout(
        addr,
        Duration::from_secs(1),
        idle_timeout,
        max_frame_bytes,
        max_error_body_bytes,
    )
}

fn configured_client_with_request_timeout(
    addr: SocketAddr,
    request_timeout: Duration,
    idle_timeout: Duration,
    max_frame_bytes: usize,
    max_error_body_bytes: usize,
) -> CorrosionClient {
    let token = BearerToken::new("test-token").expect("valid bearer token");
    let config = CorrosionClientConfig::new(
        addr,
        token,
        CorrosionClientBounds {
            connect_timeout: Duration::from_secs(1),
            request_timeout,
            stream_idle_timeout: idle_timeout,
            max_ndjson_frame_bytes: max_frame_bytes,
            max_error_body_bytes,
        },
    )
    .expect("valid client config");
    CorrosionClient::new(config).expect("client builds")
}

enum ErrorKind {
    Oversized,
    Truncated,
    Fatal,
}

impl ErrorKind {
    fn matches(&self, error: &CorrosionClientError) -> bool {
        match self {
            Self::Oversized => matches!(error, CorrosionClientError::FrameTooLarge { .. }),
            Self::Truncated => matches!(error, CorrosionClientError::TruncatedFrame),
            Self::Fatal => matches!(error, CorrosionClientError::FatalStream { detail }
                if detail == "query failed"),
        }
    }
}

#[derive(Clone, Debug)]
struct RecordedRequest {
    method: Method,
    path_and_query: String,
    headers: HeaderMap,
    body: Bytes,
}

#[derive(Clone)]
struct Chunk {
    delay: Duration,
    bytes: Bytes,
}

impl Chunk {
    fn now(bytes: &'static [u8]) -> Self {
        Self::after(Duration::ZERO, bytes)
    }

    fn after(delay: Duration, bytes: &'static [u8]) -> Self {
        Self {
            delay,
            bytes: Bytes::from_static(bytes),
        }
    }
}

#[derive(Clone)]
struct ResponseSpec {
    status: StatusCode,
    headers: Vec<(&'static str, &'static str)>,
    chunks: Vec<Chunk>,
}

impl ResponseSpec {
    fn json(status: StatusCode, body: &'static str) -> Self {
        Self::text(status, body).header("content-type", "application/json")
    }

    fn text(status: StatusCode, body: &'static str) -> Self {
        Self::stream(status, vec![Chunk::now(body.as_bytes())])
    }

    fn stream(status: StatusCode, chunks: Vec<Chunk>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            chunks,
        }
    }

    fn header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }

    fn into_response(self) -> Response<UnsyncBoxBody<Bytes, Infallible>> {
        let chunks = stream::iter(self.chunks).then(|chunk| async move {
            tokio::time::sleep(chunk.delay).await;
            Ok::<_, Infallible>(Frame::data(chunk.bytes))
        });
        let mut response = Response::new(StreamBody::new(chunks).boxed_unsync());
        *response.status_mut() = self.status;
        for (name, value) in self.headers {
            response.headers_mut().insert(
                hyper::header::HeaderName::from_static(name),
                hyper::header::HeaderValue::from_static(value),
            );
        }
        response
    }
}

struct FakeServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeServer {
    async fn start(responses: Vec<ResponseSpec>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake server binds");
        let addr = listener.local_addr().expect("fake server address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shared_requests = Arc::clone(&requests);
        let responses = Arc::new(Mutex::new(responses.into_iter()));

        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let requests = Arc::clone(&shared_requests);
                let responses = Arc::clone(&responses);
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let requests = Arc::clone(&requests);
                        let responses = Arc::clone(&responses);
                        async move {
                            let (parts, body) = request.into_parts();
                            let body = body.collect().await.expect("request body reads").to_bytes();
                            requests
                                .lock()
                                .expect("requests lock")
                                .push(RecordedRequest {
                                    method: parts.method,
                                    path_and_query: parts
                                        .uri
                                        .path_and_query()
                                        .expect("request path")
                                        .as_str()
                                        .to_owned(),
                                    headers: parts.headers,
                                    body,
                                });
                            let response = responses
                                .lock()
                                .expect("responses lock")
                                .next()
                                .expect("one fake response per request");
                            Ok::<_, Infallible>(response.into_response())
                        }
                    });
                    _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });

        Self {
            addr,
            requests,
            task,
        }
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
