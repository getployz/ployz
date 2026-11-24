//! Real RPC adapter contracts for Global catch-up.

use super::*;
use ployz_core::{IngressDoors, RequestedServiceSpec, RpcResponse};

/// What a joiner asked for while a founder runs the Ingress Proxy's Caddy and holder.
struct BesideDoors {
    result: Result<(), CatchUpError>,
    doors: Vec<SetIngressDoorsRequest>,
    /// The Service of each EnsureContainer, by the spec's name.
    ensured: Vec<String>,
}

/// Catch up a joiner holding the ingress role when `role`, beside a founder running
/// both door Containers, against a daemon that sets the doors as asked.
async fn catch_up_beside_doors(role: bool) -> BesideDoors {
    use ployz_core::{
        ContainerList, DoorChange, DoorOutcome, IngressPorts, LocalMachinePhase, MachineDetails,
        MachineList, MachineObservation, MembershipObservation, OpaquePayload, RpcRequestBody,
        caddy_service_spec, holder_service_spec,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response};

    let mut joiner = machine('1', "joiner");
    joiner.accepts_ingress = role;
    let founder = machine('f', "founder");
    let door = |identity: QualifiedService, spec: RequestedServiceSpec, id, hex| {
        let spec = spec
            .to_resolved(service_id(id), ResolvedUpdateConfig::default())
            .unwrap();
        grouped(identity, spec, running_on(&founder, hex))
            .containers
            .remove(0)
            .into_observation()
    };
    let peers = vec![
        door(
            QualifiedService::system_ingress(),
            caddy_service_spec("caddy:test"),
            'a',
            'a',
        ),
        door(
            QualifiedService::system_ingress_holder(),
            holder_service_spec("pause:test", IngressPorts::Public),
            'b',
            'b',
        ),
    ];
    let doors = Arc::new(Mutex::new(Vec::new()));
    let ensured = Arc::new(Mutex::new(Vec::new()));
    let (asked, created, target, peer_machine) = (
        doors.clone(),
        ensured.clone(),
        joiner.clone(),
        founder.clone(),
    );
    let (mut client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let (target, founder, peers) = (target.clone(), peer_machine.clone(), peers.clone());
            let (asked, created) = (asked.clone(), created.clone());
            let selected = rpc
                .metadata()
                .get(ployz_core::ONE_TARGET_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            async move {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "fixture rejects unrelated operations"
                )]
                let response = match rpc.into_inner().decode_request().unwrap().body {
                    RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                        enrollment: None,
                        machines: vec![
                            MachineObservation::new(target.clone(), MembershipObservation::Up),
                            MachineObservation::new(founder.clone(), MembershipObservation::Up),
                        ],
                    }),
                    RpcRequestBody::ListContainers(_) => RpcResponse::from(ContainerList {
                        containers: if selected.as_deref() == Some(founder.id.as_str()) {
                            peers
                        } else {
                            Vec::new()
                        },
                    }),
                    RpcRequestBody::Inspect(_) => RpcResponse::from(MachineDetails {
                        devices: Vec::new(),
                        removed_keys: Vec::new(),
                        removed_grants: Vec::new(),
                        id: target.id,
                        public_key: target.public_key,
                        machine: Some(target.clone()),
                        phase: LocalMachinePhase::Participating,
                        advertised_endpoints: Vec::new(),
                        store_version: Default::default(),
                        rtts: Vec::new(),
                        management_clients: Vec::new(),
                        passkeys: Vec::new(),
                        storage: None,
                        upgrade: None,
                        telemetry: None,
                        token: None,
                        sealing: None,
                        founding: Default::default(),
                        grants: Vec::new(),
                    }),
                    RpcRequestBody::SetIngressDoors(request) => {
                        asked.lock().unwrap().push(request);
                        let created = |hex| DoorOutcome {
                            now: running_on(&target, hex),
                            change: DoorChange::Created,
                        };
                        RpcResponse::from(IngressDoorsSet {
                            holder: Some(created('c')),
                            proxy: Some(created('d')),
                            ..IngressDoorsSet::default()
                        })
                    }
                    RpcRequestBody::EnsureContainer(ensure) => {
                        created.lock().unwrap().push(format!("{:?}", ensure.want));
                        panic!("no door Container is ensured: {:?}", ensure.want)
                    }
                    other => panic!("unexpected RPC: {other:?}"),
                };
                Ok(Response::new(response.encode().unwrap()))
            }
        })
        .await;
    let result = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await;
    server.abort();
    let doors = doors.lock().unwrap().clone();
    let ensured = ensured.lock().unwrap().clone();
    BesideDoors {
        result,
        doors,
        ensured,
    }
}

// Catches an older-style catch-up placing Caddy on a Server without the ingress role
// because a peer runs one.
#[tokio::test]
async fn a_joiner_without_the_role_gets_no_ingress_proxy() {
    let caught_up = catch_up_beside_doors(false).await;

    caught_up.result.unwrap();
    assert!(caught_up.doors.is_empty(), "{:?}", caught_up.doors);
    assert!(caught_up.ensured.is_empty(), "{:?}", caught_up.ensured);
}

// Catches a joiner with the role asking twice, once per door identity, or deriving its
// holder from a proxy other than the one the Cluster runs.
#[tokio::test]
async fn a_joiner_with_the_role_sets_its_public_door_once() {
    let caught_up = catch_up_beside_doors(true).await;

    caught_up.result.unwrap();
    assert_eq!(
        caught_up.doors,
        [SetIngressDoorsRequest {
            doors: IngressDoors::public(),
            proxy_service: service_id('a'),
            sealed_token: None,
        }]
    );
    assert!(
        caught_up.ensured.is_empty(),
        "an observed holder never becomes an EnsureContainer: {:?}",
        caught_up.ensured
    );
}

/// A Machine being retired that does not answer its listing names itself in the refusal,
/// whichever way the other Machine answers.
#[tokio::test(start_paused = true)]
async fn a_target_that_does_not_answer_its_listing_is_named_in_the_refusal() {
    use crate::deploy::CopyLifecycle;
    use ployz_core::{
        ContainerList, MachineList, MachineObservation, MembershipObservation, OpaquePayload,
        RpcRequestBody,
    };
    use tonic::{Request, Response};

    let target = machine('1', "target");
    let peer = machine('2', "peer");
    let (answering, silent) = (target.clone(), peer.clone());
    let (client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let (target, peer) = (answering.clone(), silent.clone());
            let selected = rpc
                .metadata()
                .get(ployz_core::ONE_TARGET_HEADER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            async move {
                let body = rpc.into_inner().decode_request().unwrap().body;
                let response = if matches!(body, RpcRequestBody::ListMachines(_)) {
                    RpcResponse::from(MachineList {
                        enrollment: None,
                        machines: [peer, target.clone()]
                            .into_iter()
                            .map(|machine| {
                                MachineObservation::new(machine, MembershipObservation::Up)
                            })
                            .collect(),
                    })
                } else {
                    assert!(
                        matches!(body, RpcRequestBody::ListContainers(_)),
                        "unexpected {body:?}"
                    );
                    if selected == target.id.as_str() {
                        std::future::pending::<()>().await;
                    }
                    RpcResponse::from(ContainerList { containers: vec![] })
                };
                Ok(Response::new(response.encode().unwrap()))
            }
        })
        .await;
    let refusal = client
        .target_copies(&target.id, &qualified("app", "api"))
        .await
        .unwrap_err();
    server.abort();
    assert!(refusal.target_unanswered(), "{refusal:?}");
    assert!(
        refusal
            .message
            .starts_with("Machine target did not answer: "),
        "{}",
        refusal.message
    );
    assert_eq!(
        serde_json::to_value(&refusal).unwrap().get("details"),
        Some(&serde_json::json!({"machine_id": target.id.as_str(), "reason": "target_unanswered"}))
    );
}
