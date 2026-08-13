mod echo_service;
mod test_dir;

use std::{
    collections::BTreeSet,
    net::{Ipv4Addr, TcpListener},
    time::Duration,
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use ployz_core::{
    FanoutOutcome, FanoutResponse, Machine, MachineId, MachineName, MachineSubnet,
    ManagementAddress, WireGuardPublicKey, encode_grpc_frame, grpc_frames,
};
use ployzd::{
    corrosion::{CorrosionConfig, ReplicatedStore},
    proxy::MachineProxy,
};
use tokio::{net::TcpListener as TokioTcpListener, sync::oneshot, task::JoinHandle};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{
    body::Body,
    codegen::{Service, http},
    service::Routes,
    transport::{Error as TransportError, Server},
};

use echo_service::EchoService;
use test_dir::TestDir;

#[tokio::test]
#[ignore = "requires Docker and the pinned Corrosion image"]
async fn replicated_entry_routes_and_returns_partial_fanout() {
    let root = TestDir::new("ployzd-proxy");
    let entry_gossip = unused_address();
    let mut entry_corrosion = CorrosionConfig::new(
        root.0.join("entry/data"),
        root.0.join("entry/run"),
        unused_address(),
        entry_gossip,
        format!("ployz-proxy-entry-{}", MachineId::random()),
    )
    .start()
    .await
    .unwrap();
    let mut target_corrosion = CorrosionConfig::new(
        root.0.join("target/data"),
        root.0.join("target/run"),
        unused_address(),
        unused_address(),
        format!("ployz-proxy-target-{}", MachineId::random()),
    )
    .with_bootstrap([entry_gossip])
    .start()
    .await
    .unwrap();
    let entry_store = entry_corrosion.store().clone();
    let target_store = target_corrosion.store().clone();

    let local = machine('1', "entry", "::1");
    let remote = machine('2', "remote", "::ffff:127.0.0.2");
    let duplicate_a = machine('3', "duplicate", "::ffff:127.0.0.3");
    let duplicate_b = machine('4', "duplicate", "::ffff:127.0.0.4");
    let id_collision = machine('5', remote.id.as_str(), "::ffff:127.0.0.5");
    entry_store.publish_local_machine(&local).await.unwrap();
    for machine in [&remote, &duplicate_a, &duplicate_b, &id_collision] {
        target_store.publish_local_machine(machine).await.unwrap();
    }
    wait_for_machines(
        &entry_store,
        [&local, &remote, &duplicate_a, &duplicate_b, &id_collision],
    )
    .await;

    let listener = TokioTcpListener::bind((Ipv4Addr::new(127, 0, 0, 2), 0))
        .await
        .unwrap();
    let remote_port = listener.local_addr().unwrap().port();
    let (remote_shutdown, remote_server) = start_target(listener, &remote, remote_port);
    let mut other_targets = Vec::new();
    for (address, machine) in [
        (Ipv4Addr::new(127, 0, 0, 3), &duplicate_a),
        (Ipv4Addr::new(127, 0, 0, 4), &duplicate_b),
        (Ipv4Addr::new(127, 0, 0, 5), &id_collision),
    ] {
        let listener = TokioTcpListener::bind((address, remote_port))
            .await
            .unwrap();
        other_targets.push(start_target(listener, machine, remote_port));
    }
    let mut entry = MachineProxy::new(
        Routes::new(EchoService::default()),
        local.id.clone(),
        remote_port,
        Some(entry_store),
    );

    let opaque = encode_grpc_frame(br#"{"future_field":{"untouched":true}}"#);
    let direct = request("machine", remote.name.as_str(), opaque.clone());
    let response = Service::<http::Request<Body>>::call(&mut entry, direct)
        .await
        .unwrap();
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        opaque
    );

    for ambiguous in ["duplicate", remote.id.as_str()] {
        let response = Service::<http::Request<Body>>::call(
            &mut entry,
            request("machine", ambiguous, opaque.clone()),
        )
        .await
        .unwrap();
        let responder = response
            .headers()
            .get("test-machine-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            opaque
        );
        let allowed = if ambiguous == "duplicate" {
            [&duplicate_a.id, &duplicate_b.id]
        } else {
            [&remote.id, &id_collision.id]
        };
        assert!(allowed.iter().any(|id| id.as_str() == responder));
    }

    let mut fanout = request("machines", local.name.as_str(), opaque.clone());
    fanout
        .headers_mut()
        .append("machines", remote.name.as_str().parse().unwrap());
    let response = Service::<http::Request<Body>>::call(&mut entry, fanout)
        .await
        .unwrap();
    let outcomes = decode_outcomes(response).await;
    assert_eq!(outcomes.len(), 2);
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome.machine_id().unwrap())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([local.id.clone(), remote.id.clone()])
    );
    assert!(outcomes.iter().all(|outcome| {
        matches!(
            &outcome.outcome,
            Some(FanoutOutcome::FramedPayload(payload)) if payload == &opaque
        )
    }));

    let _ = remote_shutdown.send(());
    remote_server.await.unwrap().unwrap();

    let mut fanout = request("machines", local.name.as_str(), opaque.clone());
    fanout
        .headers_mut()
        .append("machines", remote.name.as_str().parse().unwrap());
    let outcomes = decode_outcomes(
        Service::<http::Request<Body>>::call(&mut entry, fanout)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().any(|outcome| {
        outcome.machine_id().unwrap() == local.id
            && matches!(outcome.outcome, Some(FanoutOutcome::FramedPayload(_)))
    }));
    assert!(outcomes.iter().any(|outcome| {
        outcome.machine_id().unwrap() == remote.id
            && matches!(outcome.outcome, Some(FanoutOutcome::Failure(_)))
    }));

    for (shutdown, server) in other_targets {
        let _ = shutdown.send(());
        server.await.unwrap().unwrap();
    }
    target_corrosion.cleanup().await.unwrap();
    entry_corrosion.cleanup().await.unwrap();
}

fn start_target(
    listener: TokioTcpListener,
    machine: &Machine,
    remote_port: u16,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), TransportError>>) {
    let target = MachineProxy::new(
        Routes::new(EchoService {
            identity: Some(
                machine
                    .id
                    .as_str()
                    .parse()
                    .expect("Machine IDs are valid headers"),
            ),
        }),
        machine.id.clone(),
        remote_port,
        None,
    );
    let (shutdown, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(Server::builder().serve_with_incoming_shutdown(
        target,
        TcpListenerStream::new(listener),
        async { _ = shutdown_rx.await },
    ));
    (shutdown, server)
}

async fn decode_outcomes(response: http::Response<Body>) -> Vec<FanoutResponse> {
    let merged = response.into_body().collect().await.unwrap().to_bytes();
    grpc_frames(&merged)
        .unwrap()
        .into_iter()
        .map(|frame| FanoutResponse::decode_grpc_frame(frame).unwrap())
        .collect()
}

async fn wait_for_machines<'a>(
    store: &ReplicatedStore,
    expected: impl IntoIterator<Item = &'a Machine>,
) {
    let expected = expected
        .into_iter()
        .map(|machine| machine.id.clone())
        .collect::<Vec<_>>();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let visible = store.machines().await.unwrap().observations;
            if expected
                .iter()
                .all(|id| visible.iter().any(|machine| &machine.id == id))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("both Corrosion peers should replicate every Machine observation");
}

fn request(header: &str, target: &str, body: Vec<u8>) -> http::Request<Body> {
    http::Request::builder()
        .uri("/test.Echo/Call")
        .header("content-type", "application/grpc")
        .header(header, target)
        .body(Body::new(Full::new(Bytes::from(body))))
        .unwrap()
}

fn machine(id: char, name: &str, address: &str) -> Machine {
    Machine {
        id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: MachineSubnet(
            format!("10.210.{}.0/24", id.to_digit(10).unwrap())
                .parse()
                .unwrap(),
        ),
        management_address: ManagementAddress(address.parse().unwrap()),
        public_key: WireGuardPublicKey([id as u8; 32]),
        advertised_endpoints: Vec::new(),
    }
}

fn unused_address() -> std::net::SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
