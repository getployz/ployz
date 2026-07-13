use ployz_nats::service_runtime::{
    EndpointExecutionPolicy, NATS_SERVICE_ERROR_CODE_HEADER, NATS_SERVICE_ERROR_HEADER,
    NatsJsonServiceRequestError, NatsServiceError, NatsServiceErrorCode,
    NatsServiceErrorHeaderDecodeError, NatsServiceResponse, decode_nats_service_error,
    request_json, start_nats_service,
};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use serde::{Deserialize, Serialize};
use std::future::pending;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

/// A secured server with the production service split: the Controller hosts
/// API endpoints and the Operator requests them.
struct TestNats {
    _server: ployz_test_support::nats::TestNats,
    service_client: async_nats::Client,
    request_client: async_nats::Client,
}

async fn test_nats() -> TestNats {
    let server = ployz_test_support::nats::TestNats::start().await;
    let service_client = server.controller.clone();
    let request_client = server.user.clone();

    TestNats {
        _server: server,
        service_client,
        request_client,
    }
}

async fn wait_for_requester_interest(client: &async_nats::Client) {
    client
        .flush()
        .await
        .expect("request client observes service interest");
    tokio::task::yield_now().await;
}

#[tokio::test]
async fn service_runtime_responds_to_bound_endpoint() {
    let nats = test_nats().await;
    let spec = test_service_spec("plz.v1.rpc.operator.query.test.echo");
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |request| async move {
            NatsServiceResponse::ok(request.payload)
        })
        .await
        .expect("endpoint binds");
    wait_for_requester_interest(&nats.request_client).await;

    let response = nats
        .request_client
        .request("plz.v1.rpc.operator.query.test.echo", "hello".into())
        .await
        .expect("service responds");

    assert_eq!(response.payload.as_ref(), b"hello");
}

#[tokio::test]
async fn request_json_round_trips_typed_payloads() {
    let nats = test_nats().await;
    let spec = test_service_spec("plz.v1.rpc.operator.query.test.json");
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |request| async move {
            NatsServiceResponse::ok(request.payload)
        })
        .await
        .expect("endpoint binds");
    wait_for_requester_interest(&nats.request_client).await;

    let response: TestJsonPayload = request_json(
        &nats.request_client,
        "plz.v1.rpc.operator.query.test.json".to_owned(),
        &TestJsonPayload {
            value: "hello".to_owned(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("json request responds");

    assert_eq!(
        response,
        TestJsonPayload {
            value: "hello".to_owned(),
        }
    );
}

#[tokio::test]
async fn request_json_returns_service_error_headers() {
    let nats = test_nats().await;
    let spec = test_service_spec("plz.v1.rpc.operator.query.test.json_fail");
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            NatsServiceResponse::transport_error(NatsServiceError::conflict("already exists"))
        })
        .await
        .expect("endpoint binds");
    wait_for_requester_interest(&nats.request_client).await;

    let error = request_json::<_, TestJsonPayload>(
        &nats.request_client,
        "plz.v1.rpc.operator.query.test.json_fail".to_owned(),
        &TestJsonPayload {
            value: "hello".to_owned(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect_err("service error headers fail request");

    assert_eq!(
        error,
        NatsJsonServiceRequestError::Service {
            failure: NatsServiceError::conflict("already exists"),
        }
    );
}

#[tokio::test]
async fn service_runtime_returns_service_error_headers() {
    let nats = test_nats().await;
    let spec = test_service_spec("plz.v1.rpc.operator.query.test.fail");
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            NatsServiceResponse::transport_error(NatsServiceError::conflict("already exists"))
        })
        .await
        .expect("endpoint binds");
    wait_for_requester_interest(&nats.request_client).await;

    let response = nats
        .request_client
        .request("plz.v1.rpc.operator.query.test.fail", Vec::new().into())
        .await
        .expect("service responds");
    let headers = response.headers.expect("error response carries headers");

    assert_eq!(
        headers
            .get("Nats-Service-Error")
            .map(|value| value.as_str()),
        Some("already exists")
    );
    assert_eq!(
        headers
            .get("Nats-Service-Error-Code")
            .map(|value| value.as_str()),
        Some("409")
    );
    assert_eq!(NatsServiceErrorCode::Conflict.http_status_code(), 409);
}

#[tokio::test]
async fn service_runtime_counts_domain_error_payloads_without_service_error_headers() {
    let nats = test_nats().await;
    let spec = test_service_spec("plz.v1.rpc.operator.query.test.domain_error");
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            NatsServiceResponse::domain_error(br#"{"status":"domain_error"}"#.to_vec())
        })
        .await
        .expect("endpoint binds");
    wait_for_requester_interest(&nats.request_client).await;

    let response = nats
        .request_client
        .request(
            "plz.v1.rpc.operator.query.test.domain_error",
            Vec::new().into(),
        )
        .await
        .expect("service responds");

    assert!(response.headers.is_none());
    assert_eq!(response.payload.as_ref(), br#"{"status":"domain_error"}"#);
    assert_eq!(runtime.health().domain_failures, 1);
}

#[tokio::test]
async fn service_runtime_times_out_slow_handler_and_records_health() {
    let nats = test_nats().await;
    let spec = test_service_spec("plz.v1.rpc.operator.query.test.timeout");
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");
    let Some(max_concurrent_requests) = NonZeroUsize::new(1) else {
        unreachable!("test concurrency is non-zero");
    };

    runtime
        .bind_endpoint_with_policy(
            endpoint,
            EndpointExecutionPolicy::new(max_concurrent_requests, Duration::from_millis(10)),
            |_request| async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                NatsServiceResponse::ok(Vec::new())
            },
        )
        .await
        .expect("endpoint binds");
    wait_for_requester_interest(&nats.request_client).await;

    let response = nats
        .request_client
        .request("plz.v1.rpc.operator.query.test.timeout", Vec::new().into())
        .await
        .expect("service responds");
    let headers = response.headers.expect("timeout response carries headers");

    assert_eq!(
        headers
            .get("Nats-Service-Error-Code")
            .map(|value| value.as_str()),
        Some("504")
    );
    assert_eq!(runtime.health().request_timeouts, 1);
}

#[tokio::test]
async fn service_runtime_shutdown_waits_for_in_flight_request() {
    let nats = test_nats().await;
    let spec = test_service_spec("plz.v1.rpc.operator.query.test.shutdown");
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();

    let started_tx = std::sync::Mutex::new(Some(started_tx));
    let release_rx = std::sync::Mutex::new(Some(release_rx));
    runtime
        .bind_endpoint(endpoint, move |_request| {
            let started_tx = started_tx.lock().expect("started signal lock").take();
            let release_rx = release_rx.lock().expect("release signal lock").take();
            async move {
                if let Some(started_tx) = started_tx {
                    let _ = started_tx.send(());
                }
                if let Some(release_rx) = release_rx {
                    let _ = release_rx.await;
                }
                NatsServiceResponse::ok("done")
            }
        })
        .await
        .expect("endpoint binds");
    wait_for_requester_interest(&nats.request_client).await;

    let request = tokio::spawn({
        let client = nats.request_client.clone();
        async move {
            client
                .request("plz.v1.rpc.operator.query.test.shutdown", Vec::new().into())
                .await
                .expect("service responds")
        }
    });
    started_rx.await.expect("handler starts");
    let shutdown = tokio::spawn(async move { runtime.shutdown().await });

    release_tx.send(()).expect("handler release sends");
    let response = request.await.expect("request task joins");
    shutdown
        .await
        .expect("shutdown task joins")
        .expect("runtime shuts down");

    assert_eq!(response.payload.as_ref(), b"done");
}

#[tokio::test]
async fn service_runtime_shutdown_completes_immediately_when_nats_disconnects() {
    let nats = test_nats().await;
    let service_client = nats.service_client.clone();
    let spec = test_service_spec("plz.v1.rpc.operator.query.test.disconnected_shutdown");
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            NatsServiceResponse::ok(Vec::new())
        })
        .await
        .expect("endpoint binds");
    drop(nats);
    timeout(Duration::from_secs(2), async {
        while service_client.connection_state() == async_nats::connection::State::Connected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("service client observes NATS disconnect");

    timeout(Duration::from_secs(1), runtime.shutdown())
        .await
        .expect("disconnected shutdown completes well below the grace deadline")
        .expect("runtime shuts down");
}

#[tokio::test]
async fn service_runtime_shutdown_cancels_all_endpoints_after_one_shared_grace_deadline() {
    let nats = test_nats().await;
    let first_subject = "plz.v1.rpc.operator.query.test.slow_shutdown.first";
    let second_subject = "plz.v1.rpc.operator.query.test.slow_shutdown.second";
    let spec = test_service_spec_with_two_endpoints(first_subject, second_subject);
    let [first_endpoint, second_endpoint] = spec.endpoints.as_slice() else {
        unreachable!("test service has two endpoints");
    };
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (cancelled_tx, mut cancelled_rx) = mpsc::unbounded_channel();
    for endpoint in [first_endpoint, second_endpoint] {
        let started_tx = started_tx.clone();
        let cancelled_tx = cancelled_tx.clone();
        runtime
            .bind_endpoint(endpoint, move |_request| {
                let started_tx = started_tx.clone();
                let cancelled_tx = cancelled_tx.clone();
                async move {
                    let _cancelled = SendOnDrop(cancelled_tx);
                    let _ = started_tx.send(());
                    pending::<NatsServiceResponse>().await
                }
            })
            .await
            .expect("endpoint binds");
    }
    wait_for_requester_interest(&nats.request_client).await;

    let first_request = tokio::spawn({
        let client = nats.request_client.clone();
        async move { client.request(first_subject, Vec::new().into()).await }
    });
    let second_request = tokio::spawn({
        let client = nats.request_client.clone();
        async move { client.request(second_subject, Vec::new().into()).await }
    });
    timeout(Duration::from_secs(2), async {
        started_rx.recv().await.expect("first handler starts");
        started_rx.recv().await.expect("second handler starts");
    })
    .await
    .expect("both handlers start");

    let started = Instant::now();
    timeout(Duration::from_secs(3), runtime.shutdown())
        .await
        .expect("all endpoints share one grace deadline")
        .expect("runtime shuts down after cancelling slow handlers");
    let elapsed = started.elapsed();
    cancelled_rx
        .recv()
        .await
        .expect("first handler is cancelled");
    cancelled_rx
        .recv()
        .await
        .expect("second handler is cancelled");
    first_request.abort();
    second_request.abort();

    assert!(
        elapsed >= Duration::from_millis(1_800),
        "shutdown skipped its grace period: {elapsed:?}"
    );
}

#[tokio::test]
async fn service_runtime_shutdown_handles_completed_endpoint_before_stuck_endpoint() {
    let nats = test_nats().await;
    let idle_subject = "plz.v1.rpc.operator.query.test.mixed_shutdown.idle";
    let stuck_subject = "plz.v1.rpc.operator.query.test.mixed_shutdown.stuck";
    let spec = test_service_spec_with_two_endpoints(idle_subject, stuck_subject);
    let [idle_endpoint, stuck_endpoint] = spec.endpoints.as_slice() else {
        unreachable!("test service has two endpoints");
    };
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(idle_endpoint, |_request| async move {
            NatsServiceResponse::ok(Vec::new())
        })
        .await
        .expect("idle endpoint binds");
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (cancelled_tx, mut cancelled_rx) = mpsc::unbounded_channel();
    runtime
        .bind_endpoint(stuck_endpoint, move |_request| {
            let started_tx = started_tx.clone();
            let cancelled_tx = cancelled_tx.clone();
            async move {
                let _cancelled = SendOnDrop(cancelled_tx);
                let _ = started_tx.send(());
                pending::<NatsServiceResponse>().await
            }
        })
        .await
        .expect("stuck endpoint binds");
    wait_for_requester_interest(&nats.request_client).await;

    let request = tokio::spawn({
        let client = nats.request_client.clone();
        async move { client.request(stuck_subject, Vec::new().into()).await }
    });
    timeout(Duration::from_secs(2), started_rx.recv())
        .await
        .expect("stuck handler starts before timeout")
        .expect("stuck handler starts");

    let started = Instant::now();
    timeout(Duration::from_secs(3), runtime.shutdown())
        .await
        .expect("mixed endpoint shutdown uses one grace deadline")
        .expect("mixed endpoint shutdown does not double-poll completed tasks");
    let elapsed = started.elapsed();
    cancelled_rx
        .recv()
        .await
        .expect("stuck handler is cancelled");
    request.abort();

    assert!(
        elapsed >= Duration::from_millis(1_800),
        "shutdown skipped its grace period: {elapsed:?}"
    );
}

struct SendOnDrop(mpsc::UnboundedSender<()>);

impl Drop for SendOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn test_service_spec(subject: &str) -> NatsServiceSpec {
    NatsServiceSpec::new(
        "plz-api.test",
        "plz-api",
        ServiceVersion::new(0, 1, 0),
        "test service",
        ServiceMetadata::empty(),
        vec![NatsServiceEndpointSpec::new(
            "test.endpoint",
            subject,
            EndpointExecution::Query,
        )],
    )
}

fn test_service_spec_with_two_endpoints(
    first_subject: &str,
    second_subject: &str,
) -> NatsServiceSpec {
    NatsServiceSpec::new(
        "plz-api.test",
        "plz-api",
        ServiceVersion::new(0, 1, 0),
        "test service",
        ServiceMetadata::empty(),
        vec![
            NatsServiceEndpointSpec::new(
                "test.endpoint.first",
                first_subject,
                EndpointExecution::Query,
            ),
            NatsServiceEndpointSpec::new(
                "test.endpoint.second",
                second_subject,
                EndpointExecution::Query,
            ),
        ],
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestJsonPayload {
    value: String,
}

#[test]
fn service_error_header_decoder_rejects_partial_or_unknown_headers() {
    let mut headers = async_nats::HeaderMap::new();
    headers.insert(NATS_SERVICE_ERROR_HEADER, "bad request");

    assert_eq!(
        decode_nats_service_error(Some(&headers)),
        Err(NatsServiceErrorHeaderDecodeError::MissingCode)
    );

    headers.insert(NATS_SERVICE_ERROR_CODE_HEADER, "418");
    assert_eq!(
        decode_nats_service_error(Some(&headers)),
        Err(NatsServiceErrorHeaderDecodeError::UnknownCode { code: 418 })
    );

    headers.insert(NATS_SERVICE_ERROR_CODE_HEADER, "400");
    assert_eq!(
        decode_nats_service_error(Some(&headers)),
        Ok(Some(NatsServiceError {
            code: NatsServiceErrorCode::BadRequest,
            message: "bad request".to_owned(),
        }))
    );
}
