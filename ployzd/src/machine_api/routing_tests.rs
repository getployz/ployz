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
        .uri("/test.Echo/Call")
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
        .uri("/test.Echo/Call")
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
        .uri("/test.Echo/Call")
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
        id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{subnet}.0/24").parse().unwrap(),
        public_key: WireGuardPublicKey([subnet; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
    }
}
