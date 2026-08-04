use super::roster::validate_listener_principal;
use super::server::{
    ApiServerServeError, HttpBody, await_lens_state, await_server_stop,
    fallback_terminal_sse_event, initial_watch_event, lens_snapshot_response, parse_get_route,
    refusal_response, source_from_peer, sse_data, sse_event, sse_keepalive, sse_watch_body,
    version_response,
};
use super::{ApiListenerValidationError, ApiRoleConfig, ApiRoleConfigError};
use crate::corrosion::{BearerToken, CorrosionClientBounds, CorrosionClientConfig};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::header::{ALLOW, CONTENT_TYPE, RETRY_AFTER};
use hyper::{Method, Request, StatusCode};
use ployz_core::corrosion::{
    AcceptedRosterPrincipal, MachineTransport, PeerTransport, Principal, resolve_source_principal,
};
use ployz_core::ids::{ClusterId, MachineRowId, PeerId};
use ployz_core::{
    API_MAJOR, ApiFeature, ApiRefusal, ApiVersion, CorrosionRetryAfterSeconds, KnownApiFeature,
    LensCollection, LensSnapshot, LensWatchEvent, V2Route,
};
use std::future::Future;
use std::net::SocketAddr;
use std::task::Poll;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

fn corrosion_config() -> CorrosionClientConfig {
    CorrosionClientConfig::new(
        "127.0.0.1:8787"
            .parse()
            .expect("valid loopback Corrosion address"),
        BearerToken::new("corrosion-test-token").expect("valid bearer token"),
        CorrosionClientBounds {
            connect_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            stream_idle_timeout: Duration::from_secs(1),
            max_ndjson_frame_bytes: 1024,
            max_error_body_bytes: 1024,
        },
    )
    .expect("valid Corrosion configuration")
}

fn cluster_id() -> ClusterId {
    ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid cluster id")
}

fn machine_id() -> MachineRowId {
    MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("valid machine id")
}

fn other_machine_id() -> MachineRowId {
    MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("valid machine id")
}

fn peer_id() -> PeerId {
    PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("valid peer id")
}

fn services_snapshot() -> LensSnapshot {
    LensSnapshot::Services { rows: Vec::new() }
}

fn corrosion_unavailable() -> ApiRefusal {
    ApiRefusal::CorrosionUnavailable {
        retry_after_seconds: CorrosionRetryAfterSeconds::DEFAULT,
    }
}

#[tokio::test]
async fn server_stop_maps_an_exhausted_lens_to_a_typed_serve_error() {
    let (sender, mut failures) = mpsc::unbounded_channel();
    sender
        .send(LensCollection::Services)
        .expect("server still owns the lifecycle receiver");

    assert_eq!(
        await_server_stop(std::future::pending(), &mut failures).await,
        Err(ApiServerServeError::LensRecoveryExhausted {
            collection: LensCollection::Services,
        })
    );
}

#[tokio::test]
async fn controlled_shutdown_wins_over_a_ready_lens_lifecycle_failure() {
    let (shutdown_sender, shutdown) = oneshot::channel();
    let (sender, mut failures) = mpsc::unbounded_channel();
    sender
        .send(LensCollection::Services)
        .expect("server still owns the lifecycle receiver");
    shutdown_sender
        .send(())
        .expect("server still waits for controlled shutdown");

    assert_eq!(
        await_server_stop(
            async move {
                let _ = shutdown.await;
            },
            &mut failures,
        )
        .await,
        Ok(())
    );
}

#[tokio::test]
async fn a_closed_lifecycle_channel_waits_for_controlled_shutdown() {
    let (sender, mut failures) = mpsc::unbounded_channel();
    drop(sender);
    let (shutdown_sender, shutdown) = oneshot::channel();
    let stop = await_server_stop(
        async move {
            let _ = shutdown.await;
        },
        &mut failures,
    );
    tokio::pin!(stop);

    let first_poll =
        std::future::poll_fn(|context| Poll::Ready(Future::poll(stop.as_mut(), context))).await;
    assert!(first_poll.is_pending());

    shutdown_sender
        .send(())
        .expect("server still waits for controlled shutdown");
    assert_eq!(stop.await, Ok(()));
}

async fn next_sse_frame(body: &mut HttpBody) -> Bytes {
    body.frame()
        .await
        .expect("the watch body has another frame")
        .expect("the watch body is infallible")
        .into_data()
        .expect("the watch body emits data frames")
}

#[test]
fn production_configuration_rejects_ipv4_wildcard_listener() {
    let listen_addr: SocketAddr = "0.0.0.0:9876".parse().expect("valid socket address");

    let error = ApiRoleConfig::new(
        corrosion_config(),
        cluster_id(),
        machine_id(),
        listen_addr,
        "build-123".to_owned(),
    )
    .expect_err("wildcard listener is forbidden");

    assert!(matches!(
        error,
        ApiRoleConfigError::WildcardListenAddress { listen_addr: actual }
            if actual == listen_addr
    ));
}

#[test]
fn production_configuration_rejects_ipv6_wildcard_listener() {
    let listen_addr: SocketAddr = "[::]:9876".parse().expect("valid socket address");

    let error = ApiRoleConfig::new(
        corrosion_config(),
        cluster_id(),
        machine_id(),
        listen_addr,
        "build-123".to_owned(),
    )
    .expect_err("wildcard listener is forbidden");

    assert!(matches!(
        error,
        ApiRoleConfigError::WildcardListenAddress { listen_addr: actual }
            if actual == listen_addr
    ));
}

#[test]
fn production_configuration_rejects_an_unassigned_listener_port() {
    let listen_addr: SocketAddr = "[fd00::10]:0".parse().expect("valid socket address");

    let error = ApiRoleConfig::new(
        corrosion_config(),
        cluster_id(),
        machine_id(),
        listen_addr,
        "build-123".to_owned(),
    )
    .expect_err("an API service needs a concrete port");

    assert!(matches!(error, ApiRoleConfigError::ZeroListenPort));
}

#[test]
fn production_configuration_preserves_a_concrete_listener() {
    let listen_addr: SocketAddr = "[fd00::10]:9876".parse().expect("valid socket address");

    let config = ApiRoleConfig::new(
        corrosion_config(),
        cluster_id(),
        machine_id(),
        listen_addr,
        "build-123".to_owned(),
    )
    .expect("concrete listener is valid before roster validation");

    assert_eq!(config.listen_addr(), listen_addr);
    assert_eq!(config.build(), "build-123");
}

#[test]
fn sse_payloads_use_one_complete_data_frame() {
    assert_eq!(
        sse_data(br#"{"kind":"snapshot"}"#).as_ref(),
        b"data: {\"kind\":\"snapshot\"}\n\n"
    );
}

#[test]
fn sse_keepalive_is_a_comment() {
    assert_eq!(sse_keepalive().as_ref(), b": keepalive\n\n");
}

#[test]
fn only_get_accepts_exact_v2_routes() {
    assert_eq!(
        parse_get_route(&Method::GET, "/version"),
        Ok(V2Route::Version)
    );
    assert_eq!(
        parse_get_route(&Method::GET, "/lenses/services"),
        Ok(V2Route::Lens(LensCollection::Services))
    );
    assert_eq!(
        parse_get_route(&Method::GET, "/lenses/services/watch"),
        Ok(V2Route::LensWatch(LensCollection::Services))
    );
    assert_eq!(
        parse_get_route(&Method::POST, "/version"),
        Err(ApiRefusal::UnsupportedMethod {
            method: "POST".to_owned(),
        })
    );
    assert_eq!(
        parse_get_route(&Method::GET, "/lenses/services/extra"),
        Err(ApiRefusal::UnsupportedRoute)
    );
}

#[tokio::test]
async fn version_is_success_json_with_the_core_capability_catalog() {
    let response = version_response("build-123");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&"application/json".parse().expect("valid header value"))
    );
    let body = response
        .into_body()
        .collect()
        .await
        .expect("infallible body")
        .to_bytes();
    let version: ApiVersion = serde_json::from_slice(&body).expect("version JSON");
    assert_eq!(version.major, API_MAJOR);
    assert_eq!(version.build, "build-123");
    assert_eq!(
        version.features,
        vec![ApiFeature::Known(KnownApiFeature::Lenses)]
    );
}

#[tokio::test]
async fn every_refusal_has_one_structured_status_and_json_body() {
    let source = "fd00::21".parse().expect("IP address");
    let cases = [
        (ApiRefusal::UnknownSource { source }, StatusCode::FORBIDDEN),
        (
            ApiRefusal::AmbiguousSource {
                source,
                candidate_count: 2,
            },
            StatusCode::FORBIDDEN,
        ),
        (ApiRefusal::UnsupportedRoute, StatusCode::NOT_FOUND),
        (
            ApiRefusal::UnsupportedMethod {
                method: "POST".to_owned(),
            },
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (ApiRefusal::MissingCluster, StatusCode::SERVICE_UNAVAILABLE),
        (ApiRefusal::InvalidCluster, StatusCode::SERVICE_UNAVAILABLE),
        (corrosion_unavailable(), StatusCode::SERVICE_UNAVAILABLE),
    ];

    for (refusal, expected_status) in cases {
        let response = refusal_response(refusal.clone());
        assert_eq!(response.status(), expected_status);
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&"application/json".parse().expect("valid header value"))
        );
        if matches!(&refusal, ApiRefusal::UnsupportedMethod { .. }) {
            assert_eq!(
                response.headers().get(ALLOW),
                Some(&"GET".parse().expect("header"))
            );
        }
        if matches!(&refusal, ApiRefusal::CorrosionUnavailable { .. }) {
            assert_eq!(
                response.headers().get(RETRY_AFTER),
                Some(&"1".parse().expect("header"))
            );
        }
        let body = response
            .into_body()
            .collect()
            .await
            .expect("infallible body")
            .to_bytes();
        assert_eq!(
            serde_json::from_slice::<ApiRefusal>(&body).expect("refusal JSON"),
            refusal
        );
    }
}

#[tokio::test]
async fn snapshot_response_serializes_a_state_or_returns_its_typed_refusal() {
    let snapshot = services_snapshot();
    let response = lens_snapshot_response(Ok(snapshot.clone()));

    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("infallible body")
        .to_bytes();
    assert_eq!(
        serde_json::from_slice::<LensSnapshot>(&body).expect("lens snapshot JSON"),
        snapshot
    );

    let refusal = corrosion_unavailable();
    let response = lens_snapshot_response(Err(refusal.clone()));
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("infallible body")
        .to_bytes();
    assert_eq!(
        serde_json::from_slice::<ApiRefusal>(&body).expect("typed refusal JSON"),
        refusal
    );
}

#[tokio::test]
async fn each_watch_connection_wraps_its_current_state_as_a_snapshot() {
    let first_snapshot = services_snapshot();
    let second_snapshot = LensSnapshot::Operations { rows: Vec::new() };
    let (sender, receiver) = watch::channel(Some(Ok(first_snapshot.clone())));
    let mut first_connection = receiver.clone();

    let first = initial_watch_event(
        await_lens_state(&mut first_connection)
            .await
            .expect("the populated state cell is available"),
    );
    assert_eq!(first, LensWatchEvent::snapshot(first_snapshot));

    sender
        .send(Some(Ok(second_snapshot.clone())))
        .expect("the later connection keeps its receiver");
    let mut later_connection = receiver;
    let later = initial_watch_event(
        await_lens_state(&mut later_connection)
            .await
            .expect("the newer populated state cell is available"),
    );
    assert_eq!(later, LensWatchEvent::snapshot(second_snapshot));
}

#[tokio::test]
async fn later_watch_states_are_state_events_and_refusals_are_terminal_events() {
    let first_snapshot = services_snapshot();
    let second_snapshot = LensSnapshot::Operations { rows: Vec::new() };
    let (sender, mut updates) = watch::channel(Some(Ok(first_snapshot)));
    let initial = initial_watch_event(
        await_lens_state(&mut updates)
            .await
            .expect("the initial state is available"),
    );
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let mut body = sse_watch_body(updates, initial.clone(), shutdown);
    assert_eq!(
        next_sse_frame(&mut body).await,
        sse_event(&initial).expect("initial snapshot encodes")
    );

    sender
        .send(Some(Ok(second_snapshot.clone())))
        .expect("the receiver remains subscribed");
    assert_eq!(
        next_sse_frame(&mut body).await,
        sse_event(&LensWatchEvent::state(second_snapshot)).expect("later state encodes")
    );

    let refusal = corrosion_unavailable();
    sender
        .send(Some(Err(refusal.clone())))
        .expect("the receiver remains subscribed");
    assert_eq!(
        next_sse_frame(&mut body).await,
        sse_event(&LensWatchEvent::terminal(refusal)).expect("terminal refusal encodes")
    );
    assert!(body.frame().await.is_none());
}

#[tokio::test]
async fn a_ready_terminal_lens_update_precedes_a_ready_api_shutdown() {
    let snapshot = services_snapshot();
    let (state_sender, mut updates) = watch::channel(Some(Ok(snapshot)));
    let initial = initial_watch_event(
        await_lens_state(&mut updates)
            .await
            .expect("the populated state cell is available"),
    );
    let (shutdown_sender, shutdown) = watch::channel(false);
    let mut body = sse_watch_body(updates, initial.clone(), shutdown);
    assert_eq!(
        next_sse_frame(&mut body).await,
        sse_event(&initial).expect("initial snapshot encodes")
    );

    let refusal = corrosion_unavailable();
    state_sender
        .send(Some(Err(refusal.clone())))
        .expect("the stream remains subscribed to lens updates");
    shutdown_sender
        .send(true)
        .expect("the stream remains subscribed to API shutdown");

    assert_eq!(
        next_sse_frame(&mut body).await,
        sse_event(&LensWatchEvent::terminal(refusal)).expect("terminal refusal encodes")
    );
    assert!(body.frame().await.is_none());
}

#[tokio::test]
async fn an_initial_lens_refusal_is_a_terminal_watch_envelope() {
    let refusal = corrosion_unavailable();
    let (_sender, mut updates) = watch::channel(Some(Err::<LensSnapshot, _>(refusal.clone())));

    let event = initial_watch_event(
        await_lens_state(&mut updates)
            .await
            .expect("the populated terminal state is available"),
    );
    assert_eq!(event, LensWatchEvent::terminal(refusal));
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let mut body = sse_watch_body(updates, event.clone(), shutdown);
    assert_eq!(
        next_sse_frame(&mut body).await,
        sse_event(&event).expect("terminal event encodes")
    );
    assert!(body.frame().await.is_none());
}

#[test]
fn sse_snapshot_state_and_terminal_use_the_core_envelope_and_event_names() {
    let snapshot = services_snapshot();
    let events = [
        LensWatchEvent::snapshot(snapshot.clone()),
        LensWatchEvent::state(snapshot),
        LensWatchEvent::terminal(corrosion_unavailable()),
    ];
    let names = ["snapshot", "state", "terminal"];

    for (event, name) in events.iter().zip(names) {
        let frame = sse_event(event).expect("watch event encodes");
        assert!(frame.starts_with(format!("event: {name}\ndata: ").as_bytes()));
        assert!(frame.ends_with(b"\n\n"));
        let json = frame
            .as_ref()
            .strip_prefix(format!("event: {name}\ndata: ").as_bytes())
            .expect("SSE frame includes its event prefix")
            .strip_suffix(b"\n\n")
            .expect("SSE frame has one complete data terminator");
        assert_eq!(
            serde_json::from_slice::<LensWatchEvent>(json).expect("Core watch envelope JSON"),
            event.clone()
        );
    }
}

#[test]
fn fallback_sse_is_a_complete_terminal_envelope() {
    let frame = fallback_terminal_sse_event();
    let json = frame
        .as_ref()
        .strip_prefix(b"event: terminal\ndata: ")
        .expect("fallback SSE frame includes its terminal event prefix")
        .strip_suffix(b"\n\n")
        .expect("fallback SSE frame has one complete data terminator");

    assert_eq!(
        serde_json::from_slice::<LensWatchEvent>(json).expect("fallback terminal envelope JSON"),
        LensWatchEvent::terminal(corrosion_unavailable())
    );
}

#[test]
fn listener_must_resolve_to_the_configured_machine() {
    let listen_addr: SocketAddr = "[fd00::20]:9876".parse().expect("socket address");

    assert!(
        validate_listener_principal(
            &machine_id(),
            listen_addr,
            Principal::Machine {
                machine_id: machine_id(),
            },
        )
        .is_ok()
    );
    assert!(matches!(
        validate_listener_principal(
            &machine_id(),
            listen_addr,
            Principal::Machine {
                machine_id: other_machine_id(),
            },
        ),
        Err(ApiListenerValidationError::OtherMachine { .. })
    ));
    assert!(matches!(
        validate_listener_principal(
            &machine_id(),
            listen_addr,
            Principal::Peer { peer_id: peer_id() },
        ),
        Err(ApiListenerValidationError::Peer { .. })
    ));
}

#[test]
fn accepted_sources_resolve_only_by_the_exact_mesh_identity() {
    let machine = AcceptedRosterPrincipal::machine(
        machine_id(),
        MachineTransport::Tailscale {
            ip: "100.64.0.20".parse().expect("mesh IP"),
            subnet_v4: ployz_core::network::MachineEndpointSubnet::try_new("10.210.20.0/24")
                .expect("container subnet"),
        },
    );
    let peer = AcceptedRosterPrincipal::peer(
        peer_id(),
        PeerTransport::Tailscale {
            ip: "100.64.0.21".parse().expect("mesh IP"),
        },
    );
    let duplicate_peer = AcceptedRosterPrincipal::peer(
        peer_id(),
        PeerTransport::Tailscale {
            ip: "100.64.0.20".parse().expect("mesh IP"),
        },
    );

    assert_eq!(
        resolve_source_principal("100.64.0.20".parse().expect("IP"), [&machine]),
        Ok(Principal::Machine {
            machine_id: machine_id(),
        })
    );
    assert_eq!(
        resolve_source_principal("100.64.0.21".parse().expect("IP"), [&peer]),
        Ok(Principal::Peer { peer_id: peer_id() })
    );
    assert!(matches!(
        resolve_source_principal("10.210.20.9".parse().expect("IP"), [&machine]),
        Err(ployz_core::corrosion::SourcePrincipalResolutionError::UnknownSource { .. })
    ));
    assert!(matches!(
        resolve_source_principal(
            "100.64.0.20".parse().expect("IP"),
            [&machine, &duplicate_peer],
        ),
        Err(
            ployz_core::corrosion::SourcePrincipalResolutionError::AmbiguousSource {
                candidate_count: 2,
                ..
            }
        )
    ));
}

#[test]
fn forwarded_headers_cannot_replace_the_accepted_socket_peer() {
    let peer: SocketAddr = "[fd00::20]:49152".parse().expect("socket address");
    let request = Request::builder()
        .header("x-forwarded-for", "100.64.0.21")
        .header("x-real-ip", "100.64.0.22")
        .body(())
        .expect("HTTP request");

    assert_eq!(source_from_peer(peer, &request), peer.ip());
}
