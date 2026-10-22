use crate::machine_api::{
    MachineProxy, ProxyRoute, RoutingRequest, TargetResolutionError, resolve_route,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use ployz_core::{
    FanoutOutcome, FanoutResponse, FanoutSelector, Machine, MachineId, MachineName, MachineTarget,
    WireGuardPublicKey, encode_grpc_frame, grpc_frames,
};
use tokio::{net::TcpListener, sync::oneshot};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{body::Body, codegen::http, service::Routes, transport::Server};

use super::echo_service::EchoService;

#[test]
fn routing_resolves_visible_targets_without_repairing_ambiguity() {
    let local = machine('1', "local", 1);
    let first = machine('2', "duplicate", 2);
    let second = machine('3', "duplicate", 3);
    let collision = machine('4', first.id.as_str(), 4);
    let visible = vec![
        local.clone(),
        first.clone(),
        second.clone(),
        collision.clone(),
    ];

    assert_eq!(
        resolve_route(RoutingRequest::Local, &visible).unwrap(),
        ProxyRoute::Local
    );
    let resolved = resolve_route(
        RoutingRequest::Many(vec![
            fanout("local"),
            fanout(first.id.as_str()),
            fanout("local"),
        ]),
        &visible,
    )
    .unwrap();
    assert_eq!(resolved, ProxyRoute::Many(vec![local, first.clone()]));

    assert_eq!(
        resolve_route(RoutingRequest::Many(vec![FanoutSelector::All]), &visible).unwrap(),
        ProxyRoute::Many(visible.clone())
    );
    assert_eq!(
        resolve_route(
            RoutingRequest::Many(vec![FanoutSelector::All, fanout("missing")]),
            &visible,
        ),
        Err(TargetResolutionError::NotFound(vec![target("missing")]))
    );

    assert_eq!(
        resolve_route(RoutingRequest::One(target("duplicate")), &visible),
        Err(TargetResolutionError::Ambiguous {
            selector: target("duplicate"),
            matches: vec![first.id, second.id],
        })
    );

    let ProxyRoute::One(shared_namespace) =
        resolve_route(RoutingRequest::One(target(first.id.as_str())), &visible).unwrap()
    else {
        panic!("expected the exact Machine ID")
    };
    assert_eq!(*shared_namespace, first);

    assert_eq!(
        resolve_route(RoutingRequest::One(target("missing")), &visible),
        Err(TargetResolutionError::NotFound(vec![target("missing")]))
    );
    assert_eq!(
        resolve_route(RoutingRequest::One(target("all")), &visible),
        Err(TargetResolutionError::NotFound(vec![target("all")]))
    );
    assert!(MachineTarget::parse("*").is_err());
}

#[test]
fn an_unknown_server_is_not_found_only_when_members_are_visible() {
    let visible = [machine('a', "alpha", 1)];
    let missing = TargetResolutionError::NotFound(vec![target("nope")]);
    let status = super::route_status(&missing, &visible);
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert_eq!(status.message(), r#"No Server named "nope""#);
    assert_eq!(
        ployz_core::rpc::status_details(status.details()),
        Some(serde_json::json!({ "inspect": ["ployz server ls"] }))
    );
    assert_eq!(
        super::route_status(&missing, &[]).code(),
        tonic::Code::InvalidArgument
    );
    let ambiguous = TargetResolutionError::Ambiguous {
        selector: target("alpha"),
        matches: vec![visible[0].id],
    };
    assert_eq!(
        super::route_status(&ambiguous, &visible).code(),
        tonic::Code::InvalidArgument
    );
}

#[tokio::test]
async fn one_to_one_forwards_unknown_bytes_unchanged() {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let remote_port = listener.local_addr().unwrap().port();
    let (shutdown, shutdown_rx) = oneshot::channel();
    let remote_machine = machine('2', "remote", 2);
    let target_proxy = MachineProxy::new(
        Routes::new(EchoService::default()),
        remote_machine.id,
        remote_port,
        None,
    );
    let server = tokio::spawn(Server::builder().serve_with_incoming_shutdown(
        target_proxy,
        TcpListenerStream::new(listener),
        async { _ = shutdown_rx.await },
    ));

    let local = machine('1', "local", 1);
    let remote = remote_machine;
    let proxy = MachineProxy::new(
        Routes::new(EchoService::default()),
        local.id,
        remote_port,
        None,
    );
    proxy.remote_backends.lock().unwrap().insert(
        remote.management_address(),
        tonic::transport::Endpoint::from_shared(format!("http://[::1]:{remote_port}"))
            .unwrap()
            .connect_lazy(),
    );
    let opaque = br#"{\"future_field\":{\"raw\":[0,255]}}"#;
    let framed = ployz_core::encode_grpc_frame(opaque);
    let request = http::Request::builder()
        .uri("/ployz.rpc.v1.MachineRpc/Inspect")
        .header("content-type", "application/grpc")
        .header("machine", remote.name.as_str())
        .body(Body::new(Full::new(Bytes::from(framed.clone()))))
        .unwrap();

    let response = proxy.dispatch_with_snapshot(request, &[remote]).await;
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        framed
    );

    let _ = shutdown.send(());
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn forwarding_keeps_the_cloud_routed_and_operator_marks() {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let remote_port = listener.local_addr().unwrap().port();
    let (shutdown, shutdown_rx) = oneshot::channel();
    let remote = machine('2', "remote", 2);
    let target_proxy = MachineProxy::new(
        Routes::new(EchoService::default()),
        remote.id,
        remote_port,
        None,
    );
    let server = tokio::spawn(Server::builder().serve_with_incoming_shutdown(
        target_proxy,
        TcpListenerStream::new(listener),
        async { _ = shutdown_rx.await },
    ));
    let local = machine('1', "local", 1);
    let proxy = MachineProxy::new(
        Routes::new(EchoService::default()),
        local.id,
        remote_port,
        None,
    );
    proxy.remote_backends.lock().unwrap().insert(
        remote.management_address(),
        tonic::transport::Endpoint::from_shared(format!("http://[::1]:{remote_port}"))
            .unwrap()
            .connect_lazy(),
    );
    let request = http::Request::builder()
        .uri("/ployz.rpc.v1.MachineRpc/RequestMachineUpgrade")
        .header("content-type", "application/grpc")
        .header("machine", remote.name.as_str())
        .header(ployz_core::CLOUD_ROUTED_HEADER, "1")
        .header(ployz_core::OPERATOR_SCOPE_HEADER, "1")
        .body(Body::new(Full::new(Bytes::from(
            ployz_core::encode_grpc_frame(b"{}"),
        ))))
        .unwrap();

    let response = proxy.dispatch_with_snapshot(request, &[remote]).await;

    assert_eq!(
        response.headers().get(ployz_core::CLOUD_ROUTED_HEADER),
        Some(&http::HeaderValue::from_static("1")),
        "a Cloud request lost its mark on the way to the target Machine"
    );
    assert_eq!(
        response.headers().get(ployz_core::OPERATOR_SCOPE_HEADER),
        Some(&http::HeaderValue::from_static("1")),
        "an Operator request lost its mark on the way to the target Machine"
    );
    let _ = shutdown.send(());
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_dial_the_target_refuses_is_marked_as_never_reaching_it() {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let closed_port = listener.local_addr().unwrap().port();
    drop(listener);

    let status = routed_failure(closed_port, None).await;

    assert!(
        status
            .metadata()
            .contains_key(ployz_core::rpc::TARGET_UNANSWERED_KEY),
        "an older client must still read a failed dial as the router's own failure: {status:?}"
    );
    assert!(
        status
            .metadata()
            .contains_key(ployz_core::rpc::TARGET_UNREACHED_KEY),
        "a refused dial never sent the call: {status:?}"
    );
}

#[tokio::test]
async fn a_target_that_drops_the_call_after_taking_it_may_have_reached_it() {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let target = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_until_request_headers(&mut stream).await;
    });

    let status = routed_failure(port, None).await;
    target.await.unwrap();

    assert!(
        status
            .metadata()
            .contains_key(ployz_core::rpc::TARGET_UNANSWERED_KEY),
        "{status:?}"
    );
    assert!(
        !status
            .metadata()
            .contains_key(ployz_core::rpc::TARGET_UNREACHED_KEY),
        "the target held the call before it dropped: {status:?}"
    );
}

#[tokio::test]
async fn a_target_that_outlives_the_call_deadline_may_have_reached_it() {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (release, released) = oneshot::channel::<()>();
    let target = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_until_request_headers(&mut stream).await;
        let _ = released.await;
    });

    let status = routed_failure(port, Some("100m")).await;
    let _ = release.send(());
    target.await.unwrap();

    assert!(
        status
            .metadata()
            .contains_key(ployz_core::rpc::TARGET_UNANSWERED_KEY),
        "{status:?}"
    );
    assert!(
        !status
            .metadata()
            .contains_key(ployz_core::rpc::TARGET_UNREACHED_KEY),
        "the target held the call when its deadline passed: {status:?}"
    );
}

#[tokio::test]
async fn a_target_s_own_router_marks_never_reach_the_caller() {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (shutdown, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(Server::builder().serve_with_incoming_shutdown(
        MarkingService,
        TcpListenerStream::new(listener),
        async { _ = shutdown_rx.await },
    ));
    let local = machine('1', "local", 1);
    let remote = machine('2', "remote", 2);
    let proxy = MachineProxy::new(Routes::new(EchoService::default()), local.id, port, None);
    proxy.remote_backends.lock().unwrap().insert(
        remote.management_address(),
        tonic::transport::Endpoint::from_shared(format!("http://[::1]:{port}"))
            .unwrap()
            .connect_lazy(),
    );
    let request = http::Request::builder()
        .uri("/ployz.rpc.v1.MachineRpc/Inspect")
        .header("content-type", "application/grpc")
        .header("machine", remote.name.as_str())
        .body(Body::empty())
        .unwrap();

    let response = proxy.dispatch_with_snapshot(request, &[remote]).await;
    let (parts, body) = response.into_parts();
    let trailers = body.collect().await.unwrap().trailers().cloned().unwrap();

    for (place, headers) in [("headers", &parts.headers), ("trailers", &trailers)] {
        for mark in [
            ployz_core::rpc::TARGET_UNANSWERED_KEY,
            ployz_core::rpc::TARGET_UNREACHED_KEY,
        ] {
            assert!(
                !headers.contains_key(mark),
                "the target's {place} passed {mark} off as the entry router's: {headers:?}"
            );
        }
    }
    let _ = shutdown.send(());
    server.await.unwrap().unwrap();
}

/// A target that puts both router marks on its own response headers and trailers.
#[derive(Clone)]
struct MarkingService;

impl tower_service::Service<http::Request<Body>> for MarkingService {
    type Response = http::Response<Body>;
    type Error = std::convert::Infallible;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _request: http::Request<Body>) -> Self::Future {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", http::HeaderValue::from_static("14"));
        mark(&mut trailers);
        let frames = [
            Ok::<_, std::convert::Infallible>(http_body::Frame::data(Bytes::from(
                encode_grpc_frame(b"{}"),
            ))),
            Ok(http_body::Frame::trailers(trailers)),
        ];
        let body = http_body_util::StreamBody::new(tokio_stream::iter(frames));
        let mut response = http::Response::new(Body::new(body));
        response.headers_mut().insert(
            "content-type",
            http::HeaderValue::from_static("application/grpc"),
        );
        mark(response.headers_mut());
        std::future::ready(Ok(response))
    }
}

fn mark(headers: &mut http::HeaderMap) {
    for key in [
        ployz_core::rpc::TARGET_UNANSWERED_KEY,
        ployz_core::rpc::TARGET_UNREACHED_KEY,
    ] {
        headers.insert(key, http::HeaderValue::from_static("1"));
    }
}

/// The status the router answers for one call to a remote Machine listening on `port`.
async fn routed_failure(port: u16, grpc_timeout: Option<&str>) -> tonic::Status {
    let local = machine('1', "local", 1);
    let remote = machine('2', "remote", 2);
    let proxy = MachineProxy::new(Routes::new(EchoService::default()), local.id, port, None);
    proxy.remote_backends.lock().unwrap().insert(
        remote.management_address(),
        tonic::transport::Endpoint::from_shared(format!("http://[::1]:{port}"))
            .unwrap()
            .connect_lazy(),
    );
    let mut request = http::Request::builder()
        .uri("/ployz.rpc.v1.MachineRpc/Inspect")
        .header("content-type", "application/grpc")
        .header("machine", remote.name.as_str());
    if let Some(timeout) = grpc_timeout {
        request = request.header("grpc-timeout", timeout);
    }
    let request = request.body(Body::empty()).unwrap();

    let response = proxy.dispatch_with_snapshot(request, &[remote]).await;
    let status = tonic::Status::from_header_map(response.headers()).unwrap();
    assert_eq!(status.code(), tonic::Code::Unavailable, "{status:?}");
    status
}

/// Reads the HTTP/2 preface and frames until the call's HEADERS frame has arrived.
async fn read_until_request_headers(stream: &mut tokio::net::TcpStream) {
    use tokio::io::AsyncReadExt;

    const PREFACE: usize = 24;
    const HEADERS: u8 = 0x1;
    let mut received = Vec::new();
    let mut frame_start = PREFACE;
    loop {
        while let Some(&[l0, l1, l2, kind, ..]) = received.get(frame_start..frame_start + 9) {
            if kind == HEADERS {
                return;
            }
            frame_start += 9 + (usize::from(l0) << 16 | usize::from(l1) << 8 | usize::from(l2));
        }
        let read = stream.read_buf(&mut received).await.unwrap();
        assert_ne!(read, 0, "the router closed before sending the call");
    }
}

#[tokio::test]
async fn fanout_keeps_successes_and_target_failures_as_valid_frames() {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let remote_port = listener.local_addr().unwrap().port();
    let (shutdown, shutdown_rx) = oneshot::channel();
    let target_machine = machine('2', "reachable", 2);
    let target_proxy = MachineProxy::new(
        Routes::new(EchoService::default()),
        target_machine.id,
        remote_port,
        None,
    );
    let server = tokio::spawn(Server::builder().serve_with_incoming_shutdown(
        target_proxy,
        TcpListenerStream::new(listener),
        async { _ = shutdown_rx.await },
    ));

    let local = machine('1', "local", 1);
    let reachable = target_machine;
    let unreachable = machine('3', "unreachable", 3);
    let proxy = MachineProxy::new(
        Routes::new(EchoService::default()),
        local.id,
        remote_port,
        None,
    );
    proxy.remote_backends.lock().unwrap().insert(
        reachable.management_address(),
        tonic::transport::Endpoint::from_shared(format!("http://[::1]:{remote_port}"))
            .unwrap()
            .connect_lazy(),
    );
    let request_frames = [encode_grpc_frame(b"opaque"), encode_grpc_frame(b"future")];
    let request_body = request_frames.concat();
    let request = http::Request::builder()
        .uri("/ployz.rpc.v1.MachineRpc/Inspect")
        .header("content-type", "application/grpc")
        .header("machines", local.name.as_str())
        .header("machines", reachable.name.as_str())
        .header("machines", unreachable.name.as_str())
        .body(Body::new(Full::new(Bytes::from(request_body))))
        .unwrap();

    let response = proxy
        .dispatch_with_snapshot(
            request,
            &[local.clone(), reachable.clone(), unreachable.clone()],
        )
        .await;
    let merged = response.into_body().collect().await.unwrap().to_bytes();
    let outcomes = grpc_frames(&merged)
        .unwrap()
        .into_iter()
        .map(|frame| FanoutResponse::decode_grpc_frame(frame).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(outcomes.len(), 5);
    let local_payloads = outcomes
        .iter()
        .filter(|outcome| outcome.machine_id().unwrap() == local.id)
        .filter_map(|outcome| match &outcome.outcome {
            Some(FanoutOutcome::FramedPayload(payload)) => Some(payload.clone()),
            Some(FanoutOutcome::Failure(_)) | None => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(local_payloads, request_frames);
    let reachable_payloads = outcomes
        .iter()
        .filter(|outcome| outcome.machine_id().unwrap() == reachable.id)
        .filter_map(|outcome| match &outcome.outcome {
            Some(FanoutOutcome::FramedPayload(payload)) => Some(payload.clone()),
            Some(FanoutOutcome::Failure(_)) | None => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reachable_payloads, request_frames);
    let unreachable_outcome = outcomes
        .iter()
        .find(|outcome| outcome.machine_id().unwrap() == unreachable.id)
        .unwrap();
    assert!(matches!(
        unreachable_outcome.outcome,
        Some(FanoutOutcome::Failure(_))
    ));

    let _ = shutdown.send(());
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn fanout_emits_an_omission_for_a_target_with_no_message() {
    let local = machine('1', "local", 1);
    let proxy = MachineProxy::new(Routes::new(EchoService::default()), local.id, 1, None);
    let request = http::Request::builder()
        .uri("/ployz.rpc.v1.MachineRpc/Inspect")
        .header("content-type", "application/grpc")
        .header("machines", local.name.as_str())
        .body(Body::empty())
        .unwrap();

    let response = proxy
        .dispatch_with_snapshot(request, std::slice::from_ref(&local))
        .await;
    let merged = response.into_body().collect().await.unwrap().to_bytes();
    let frames = grpc_frames(&merged).unwrap();
    assert_eq!(frames.len(), 1);
    let omission = FanoutResponse::decode_grpc_frame(frames.first().unwrap()).unwrap();
    assert_eq!(omission.machine_id().unwrap(), local.id);
    assert_eq!(omission.outcome, None);
}

fn target(value: &str) -> MachineTarget {
    MachineTarget::parse(value).unwrap()
}

fn fanout(value: &str) -> FanoutSelector {
    FanoutSelector::parse(value).unwrap()
}

fn machine(id: char, name: &str, subnet: u8) -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{subnet}.0/24").parse().unwrap(),
        public_key: WireGuardPublicKey([subnet; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        build_concurrency: None,
    }
}

#[tokio::test]
async fn client_streaming_methods_reject_fanout_before_reading_input() {
    let local = machine('1', "local", 1);
    let proxy = MachineProxy::new(Routes::new(EchoService::default()), local.id, 1, None);
    for method in ["Build", "Exec"] {
        let request = http::Request::builder()
            .uri(format!("/ployz.rpc.v1.MachineRpc/{method}"))
            .header("content-type", "application/grpc")
            .header("machines", local.name.as_str())
            .body(Body::empty())
            .unwrap();
        let response = proxy
            .dispatch_with_snapshot(request, std::slice::from_ref(&local))
            .await;
        assert_eq!(response.headers().get("grpc-status").unwrap(), "3");
    }
}
