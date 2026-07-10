use futures_util::StreamExt;
use ployz_core::dataplane::{
    DEFAULT_WIREGUARD_LISTEN_PORT, DataplaneMember, DataplanePrepareError, DataplanePrepareRequest,
    DataplaneProviderFailure, EbpfForwardingReady, EbpfForwardingReadyEvidence,
    PloyzNativeMeshComponent, PloyzNativeMeshMachineReady, PloyzNativeMeshReady,
    WireGuardPeerEndpoint, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
};
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
use ployz_core::state::MachineEndpointObservation;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, start_nats_service};
use ployz_test_support::ids::{failure_message, machine_id, operation_id};
use ployzd::operations::deploy::DataplanePreparer;
use ployzd::roles::machine::client::NatsMachineDataplanePreparer;
use ployzd::roles::machine::protocol::{
    MachineDataplanePrepareRpcRequest, MachineDataplanePrepareRpcResponse, MachineFactsGetRpcOk,
    MachineFactsGetRpcResponse, MachinePloyzNativeMeshPrepareDomainError,
    MachinePloyzNativeMeshPrepareRpcOk, MachinePloyzNativeMeshPrepareRpcRequest,
};
use ployzd::service_catalog::machine_role_service;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn nats_machine_preparer_calls_wireguard_ebpf_prepare_service() {
    let nats = test_nats().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let _service =
        start_wireguard_ebpf_prepare_service(nats.machine_a.clone(), &machine_id("machine_a"), {
            let received = Arc::clone(&received);
            move |request| {
                received
                    .lock()
                    .expect("received request lock is not poisoned")
                    .push(request);
            }
        })
        .await;
    let mut preparer = NatsMachineDataplanePreparer::new(nats.client);

    let report = preparer
        .prepare_dataplane(dataplane_request(&["machine_a"]))
        .await
        .expect("dataplane prepare succeeds");
    assert_eq!(report.machines, vec![ready_machine("machine_a")]);

    assert_eq!(
        received
            .lock()
            .expect("received request lock is not poisoned")
            .as_slice(),
        [native_mesh_rpc_request(
            &["machine_a"],
            MachinePloyzNativeMeshPrepareRpcRequest::PrepareDataplane {
                endpoint_routes: endpoint_routes(&["machine_a"]),
                peers: Vec::new(),
            },
        )]
    );
}

#[tokio::test]
async fn nats_machine_preparer_returns_unreachable_overlay_targets() {
    let nats = test_nats().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let _service =
        start_wireguard_ebpf_prepare_service(nats.machine_a, &machine_id("machine_a"), {
            let received = Arc::clone(&received);
            move |request| {
                received
                    .lock()
                    .expect("received request lock is not poisoned")
                    .push(request);
            }
        })
        .await;
    let preparer = NatsMachineDataplanePreparer::new(nats.client);
    let peer = wireguard_public_key("peer-public-key");

    let unreachable = preparer
        .probe_overlay(
            operation_id("op_123"),
            &machine_id("machine_a"),
            vec![machine_id("machine_a")],
            vec![peer.clone()],
        )
        .await
        .expect("overlay probe succeeds");

    assert_eq!(unreachable, vec![peer.clone()]);
    assert_eq!(
        received
            .lock()
            .expect("received request lock is not poisoned")
            .as_slice(),
        [native_mesh_rpc_request(
            &["machine_a"],
            MachinePloyzNativeMeshPrepareRpcRequest::ProbeOverlay {
                public_keys: vec![peer],
            },
        )]
    );
}

#[tokio::test]
async fn nats_machine_preparer_bootstraps_public_keys_then_programs_peers() {
    let nats = test_nats().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let mut services = Vec::new();
    for (machine, machine_client) in [
        ("machine_a", nats.machine_a.clone()),
        ("machine_b", nats.machine_b.clone()),
    ] {
        let received = Arc::clone(&received);
        services.push(
            start_wireguard_ebpf_prepare_service(
                machine_client,
                &machine_id(machine),
                move |request| {
                    received
                        .lock()
                        .expect("received request lock is not poisoned")
                        .push(request);
                },
            )
            .await,
        );
    }
    let _machine_a_facts =
        start_machine_facts_service(nats.machine_a.clone(), "machine_a", Some(1)).await;
    let _machine_b_facts =
        start_machine_facts_service(nats.machine_b.clone(), "machine_b", Some(2)).await;
    let mut preparer = NatsMachineDataplanePreparer::new(nats.client);

    let report = preparer
        .prepare_dataplane(dataplane_request(&["machine_a", "machine_b"]))
        .await
        .expect("dataplane prepare succeeds");
    assert_eq!(
        report.machines,
        vec![ready_machine("machine_a"), ready_machine("machine_b")]
    );
    let received = received
        .lock()
        .expect("received request lock is not poisoned")
        .clone();
    let [first_read, second_read, first_prepare, second_prepare] = received.as_slice() else {
        panic!("expected two read requests and two prepare requests");
    };
    assert_eq!(
        native_mesh_request(first_read),
        &MachinePloyzNativeMeshPrepareRpcRequest::ReadPublicKey
    );
    assert_eq!(
        native_mesh_request(second_read),
        &MachinePloyzNativeMeshPrepareRpcRequest::ReadPublicKey
    );
    let MachinePloyzNativeMeshPrepareRpcRequest::PrepareDataplane {
        peers: first_peers, ..
    } = native_mesh_request(first_prepare)
    else {
        panic!("expected first prepare request");
    };
    let MachinePloyzNativeMeshPrepareRpcRequest::PrepareDataplane {
        peers: second_peers,
        ..
    } = native_mesh_request(second_prepare)
    else {
        panic!("expected second prepare request");
    };
    assert_eq!(
        first_peers.as_slice(),
        [
            ployz_core::dataplane::WireGuardPeer::from_endpoint(
                peer_endpoint("machine_a", 1),
                wireguard_public_key("public-machine_a"),
            ),
            ployz_core::dataplane::WireGuardPeer::from_endpoint(
                peer_endpoint("machine_b", 2),
                wireguard_public_key("public-machine_b"),
            ),
        ]
    );
    assert_eq!(second_peers, first_peers);
}

#[tokio::test]
async fn nats_machine_preparer_maps_missing_responder_to_wireguard_unavailable() {
    let nats = test_nats().await;
    let mut preparer = NatsMachineDataplanePreparer::new(nats.client);

    let error = preparer
        .prepare_dataplane(dataplane_request(&["machine_missing"]))
        .await
        .expect_err("missing machine responder fails");

    assert_eq!(
        error,
        DataplanePrepareError::Unavailable {
            machine_id: machine_id("machine_missing"),
            provider: DataplaneProviderFailure::PloyzNativeMesh {
                component: PloyzNativeMeshComponent::WireGuard,
            },
            message: failure_message("machine runtime has no responders"),
        }
    );
}

#[tokio::test]
async fn nats_machine_preparer_requires_public_ip_facts_for_multi_machine_mesh() {
    let nats = test_nats().await;
    let _machine_a_facts =
        start_machine_facts_service(nats.machine_a.clone(), "machine_a", Some(1)).await;
    let mut preparer = NatsMachineDataplanePreparer::new(nats.client);

    let error = preparer
        .prepare_dataplane(dataplane_request(&["machine_a", "machine_b"]))
        .await
        .expect_err("missing public ip fact fails before dataplane prepare RPC");

    assert_eq!(
        error,
        DataplanePrepareError::Unavailable {
            machine_id: machine_id("machine_b"),
            provider: DataplaneProviderFailure::PloyzNativeMesh {
                component: PloyzNativeMeshComponent::WireGuard,
            },
            message: failure_message(
                "machine public ip fact could not be read: machine machine_b facts unavailable: machine runtime has no responders"
            ),
        }
    );
}

#[tokio::test]
async fn nats_machine_preparer_rejects_duplicate_native_mesh_subnets_before_rpc() {
    let nats = test_nats().await;
    let mut preparer = NatsMachineDataplanePreparer::new(nats.client);

    let error = preparer
        .prepare_dataplane(DataplanePrepareRequest {
            operation_id: operation_id("op_123"),
            membership: vec![
                DataplaneMember::default_for_machine(machine_id("machine_1")),
                DataplaneMember::default_for_machine(machine_id("machine_255")),
            ],
        })
        .await
        .expect_err("native mesh endpoint subnets must be unique");

    assert_eq!(
        error,
        DataplanePrepareError::Unavailable {
            machine_id: machine_id("machine_255"),
            provider: DataplaneProviderFailure::PloyzNativeMesh {
                component: PloyzNativeMeshComponent::WireGuard,
            },
            message: failure_message(
                "ployz native mesh endpoint subnet is already assigned: 10.42.1.0/24"
            ),
        }
    );
}

#[tokio::test]
async fn nats_machine_preparer_rejects_dataplane_readiness_during_public_key_phase() {
    let nats = test_nats().await;
    let _machine_a = start_wireguard_ebpf_prepare_raw_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        {
            move |_request| {
                NatsServiceResponse::ok(encode_wireguard_ebpf_response(
                    MachineDataplanePrepareRpcResponse::Ok(
                        MachinePloyzNativeMeshPrepareRpcOk::Ready {
                            readiness: ready_machine("machine_a"),
                        },
                    ),
                ))
            }
        },
    )
    .await;
    let _machine_b =
        start_wireguard_ebpf_prepare_service(nats.machine_b.clone(), &machine_id("machine_b"), {
            move |_request| {}
        })
        .await;
    let _machine_a_facts =
        start_machine_facts_service(nats.machine_a.clone(), "machine_a", Some(1)).await;
    let _machine_b_facts =
        start_machine_facts_service(nats.machine_b.clone(), "machine_b", Some(2)).await;
    let mut preparer = NatsMachineDataplanePreparer::new(nats.client);

    let error = preparer
        .prepare_dataplane(dataplane_request(&["machine_a", "machine_b"]))
        .await
        .expect_err("readiness during public key phase is invalid");

    assert_eq!(
        error,
        DataplanePrepareError::InvalidReport {
            message: failure_message(
                "machine machine_a returned dataplane readiness for public key request"
            ),
        }
    );
}

#[tokio::test]
async fn nats_machine_preparer_rejects_public_key_during_prepare_phase() {
    let nats = test_nats().await;
    let _service = start_wireguard_ebpf_prepare_raw_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        {
            move |_request| {
                NatsServiceResponse::ok(encode_wireguard_ebpf_response(
                    MachineDataplanePrepareRpcResponse::Ok(
                        MachinePloyzNativeMeshPrepareRpcOk::PublicKey {
                            machine_id: machine_id("machine_a"),
                            public_key: wireguard_public_key("public-machine_a"),
                        },
                    ),
                ))
            }
        },
    )
    .await;
    let mut preparer = NatsMachineDataplanePreparer::new(nats.client);

    let error = preparer
        .prepare_dataplane(dataplane_request(&["machine_a"]))
        .await
        .expect_err("public key during prepare phase is invalid");

    assert_eq!(
        error,
        DataplanePrepareError::InvalidReport {
            message: failure_message(
                "machine machine_a returned public key for dataplane prepare request"
            ),
        }
    );
}

#[tokio::test]
async fn nats_machine_preparer_rejects_native_mesh_response_for_wrong_machine() {
    let nats = test_nats().await;
    let _service = start_wireguard_ebpf_prepare_raw_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        {
            move |_request| {
                NatsServiceResponse::ok(encode_wireguard_ebpf_response(
                    MachineDataplanePrepareRpcResponse::Ok(
                        MachinePloyzNativeMeshPrepareRpcOk::Ready {
                            readiness: ready_machine("machine_b"),
                        },
                    ),
                ))
            }
        },
    )
    .await;
    let mut preparer = NatsMachineDataplanePreparer::new(nats.client);

    let error = preparer
        .prepare_dataplane(dataplane_request(&["machine_a"]))
        .await
        .expect_err("wrong machine readiness is unavailable");

    assert_eq!(
        error,
        DataplanePrepareError::Unavailable {
            machine_id: machine_id("machine_a"),
            provider: DataplaneProviderFailure::PloyzNativeMesh {
                component: PloyzNativeMeshComponent::WireGuard,
            },
            message: failure_message("machine runtime replied for a different machine: machine_b"),
        }
    );
}

#[tokio::test]
async fn nats_machine_preparer_preserves_native_mesh_invalid_report_domain_error() {
    let nats = test_nats().await;
    let _service = start_wireguard_ebpf_prepare_raw_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        {
            move |_request| {
                NatsServiceResponse::domain_error(encode_wireguard_ebpf_response(
                    MachineDataplanePrepareRpcResponse::DomainError {
                        machine_id: machine_id("machine_a"),
                        error: MachinePloyzNativeMeshPrepareDomainError::InvalidReport {
                            message: failure_message("host report was malformed"),
                        },
                    },
                ))
            }
        },
    )
    .await;
    let mut preparer = NatsMachineDataplanePreparer::new(nats.client);

    let error = preparer
        .prepare_dataplane(dataplane_request(&["machine_a"]))
        .await
        .expect_err("invalid report domain error is preserved");

    assert_eq!(
        error,
        DataplanePrepareError::InvalidReport {
            message: failure_message("host report was malformed"),
        }
    );
}

async fn start_wireguard_ebpf_prepare_service(
    client: async_nats::Client,
    machine_id: &MachineId,
    handler: impl Fn(MachineDataplanePrepareRpcRequest) + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let machine_id = machine_id.clone();
    start_wireguard_ebpf_prepare_raw_service(client, machine_id.clone(), move |request| {
        let response = decode_wireguard_ebpf_request(request).map(|request| {
            let provider_request = native_mesh_request(&request).clone();
            handler(request);
            wireguard_ebpf_prepare_ok_response(machine_id.clone(), provider_request)
        });
        match response {
            Ok(response) => response,
            Err(message) => NatsServiceResponse::transport_error(
                ployz_nats::service_runtime::NatsServiceError::bad_request(message),
            ),
        }
    })
    .await
}

async fn start_wireguard_ebpf_prepare_raw_service(
    client: async_nats::Client,
    machine_id: MachineId,
    handler: impl Fn(NatsServiceRequest) -> NatsServiceResponse + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let spec = machine_role_service(&machine_id);
    let endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject
                == machine_service(&machine_id, MachineServiceEndpoint::DataplanePrepare)
        })
        .expect("dataplane.prepare endpoint exists")
        .clone();
    let mut service = start_nats_service(client.clone(), &spec)
        .await
        .expect("start machine service");
    service
        .bind_endpoint(&endpoint, move |request| {
            let response = handler(request);
            async move { response }
        })
        .await
        .expect("bind dataplane.prepare endpoint");
    client.flush().await.expect("flush service subscription");
    service
}

fn decode_wireguard_ebpf_request(
    request: NatsServiceRequest,
) -> Result<MachineDataplanePrepareRpcRequest, String> {
    serde_json::from_slice(&request.payload).map_err(|error| error.to_string())
}

fn encode_wireguard_ebpf_response(response: MachineDataplanePrepareRpcResponse) -> Vec<u8> {
    serde_json::to_vec(&response).expect("machine wireguard ebpf response encodes")
}

fn wireguard_ebpf_prepare_ok_response(
    machine_id: MachineId,
    request: MachinePloyzNativeMeshPrepareRpcRequest,
) -> NatsServiceResponse {
    match request {
        MachinePloyzNativeMeshPrepareRpcRequest::ReadPublicKey => NatsServiceResponse::ok(
            encode_wireguard_ebpf_response(MachineDataplanePrepareRpcResponse::Ok(
                MachinePloyzNativeMeshPrepareRpcOk::PublicKey {
                    public_key: wireguard_public_key(format!("public-{}", machine_id.as_str())),
                    machine_id,
                },
            )),
        ),
        MachinePloyzNativeMeshPrepareRpcRequest::PrepareWireGuard { .. } => {
            NatsServiceResponse::ok(encode_wireguard_ebpf_response(
                MachineDataplanePrepareRpcResponse::Ok(
                    MachinePloyzNativeMeshPrepareRpcOk::WireGuardReady {
                        machine_id: machine_id.clone(),
                        readiness: ready_machine_for_id(machine_id).ready.wireguard,
                    },
                ),
            ))
        }
        MachinePloyzNativeMeshPrepareRpcRequest::PrepareDataplane { .. } => {
            NatsServiceResponse::ok(encode_wireguard_ebpf_response(
                MachineDataplanePrepareRpcResponse::Ok(MachinePloyzNativeMeshPrepareRpcOk::Ready {
                    readiness: ready_machine_for_id(machine_id),
                }),
            ))
        }
        MachinePloyzNativeMeshPrepareRpcRequest::ProbeOverlay { public_keys } => {
            NatsServiceResponse::ok(encode_wireguard_ebpf_response(
                MachineDataplanePrepareRpcResponse::Ok(
                    MachinePloyzNativeMeshPrepareRpcOk::OverlayProbe {
                        machine_id,
                        unreachable: public_keys,
                    },
                ),
            ))
        }
    }
}

fn native_mesh_rpc_request(
    machines: &[&str],
    request: MachinePloyzNativeMeshPrepareRpcRequest,
) -> MachineDataplanePrepareRpcRequest {
    MachineDataplanePrepareRpcRequest::ployz_native_mesh(
        operation_id("op_123"),
        machines.iter().map(|machine| machine_id(machine)).collect(),
        request,
    )
}

fn native_mesh_request(
    request: &MachineDataplanePrepareRpcRequest,
) -> &MachinePloyzNativeMeshPrepareRpcRequest {
    &request.request
}

fn ready_machine(machine_id: &str) -> PloyzNativeMeshMachineReady {
    ready_machine_for_id(self::machine_id(machine_id))
}

fn ready_machine_for_id(machine_id: MachineId) -> PloyzNativeMeshMachineReady {
    let public_key = wireguard_public_key(format!("public-{}", machine_id.as_str()));
    PloyzNativeMeshMachineReady {
        machine_id,
        ready: PloyzNativeMeshReady {
            wireguard: WireGuardReady {
                public_key,
                evidence: vec![WireGuardReadyEvidence::Command {
                    program: "wg".to_owned(),
                    args: vec!["--version".to_owned()],
                }],
            },
            ebpf_forwarding: EbpfForwardingReady {
                evidence: vec![EbpfForwardingReadyEvidence::PloyzTcBytecode {
                    path: "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".to_owned(),
                    symbols: vec!["ployz_egress".to_owned(), "ployz_ingress".to_owned()],
                }],
            },
        },
    }
}

fn dataplane_request(machines: &[&str]) -> DataplanePrepareRequest {
    DataplanePrepareRequest::for_machines(
        operation_id("op_123"),
        machines.iter().map(|machine| machine_id(machine)).collect(),
    )
}

fn peer_endpoint(machine: &str, last_octet: u8) -> WireGuardPeerEndpoint {
    WireGuardPeerEndpoint::new(
        machine_id(machine),
        SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, last_octet)),
            DEFAULT_WIREGUARD_LISTEN_PORT,
        ),
    )
}

fn wireguard_public_key(value: impl Into<String>) -> WireGuardPublicKey {
    WireGuardPublicKey::try_new(value).expect("valid wireguard public key")
}

fn endpoint_routes(machines: &[&str]) -> Vec<ployz_core::dataplane::WireGuardEbpfEndpointRoute> {
    machines
        .iter()
        .map(|machine| {
            ployz_core::dataplane::WireGuardEbpfEndpointRoute::default_for_machine(&machine_id(
                machine,
            ))
        })
        .collect()
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    client: async_nats::Client,
    machine_a: async_nats::Client,
    machine_b: async_nats::Client,
}

async fn test_nats() -> TestNats {
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        machine_id("machine_a"),
        machine_id("machine_b"),
    ])
    .await;
    let client = nats.controller.clone();
    let machine_a = nats.machine_client(&machine_id("machine_a")).await;
    let machine_b = nats.machine_client(&machine_id("machine_b")).await;

    TestNats {
        _nats: nats,
        client,
        machine_a,
        machine_b,
    }
}

async fn start_machine_facts_service(
    client: async_nats::Client,
    machine_id_value: &str,
    public_ip_octet: Option<u8>,
) -> tokio::task::JoinHandle<()> {
    let machine_id = machine_id(machine_id_value);
    let subject = machine_service(&machine_id, MachineServiceEndpoint::FactsGet);
    let mut subscriber = client
        .subscribe(subject)
        .await
        .expect("subscribe facts service");
    client
        .flush()
        .await
        .expect("flush facts service subscription");
    tokio::spawn(async move {
        while let Some(message) = subscriber.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            let response =
                serde_json::to_vec(&MachineFactsGetRpcResponse::Ok(MachineFactsGetRpcOk {
                    facts: machine_facts(&machine_id, public_ip_octet),
                }))
                .expect("facts response serializes");
            let _ = client.publish(reply, response.into()).await;
        }
    })
}

fn machine_facts(machine_id: &MachineId, public_ip_octet: Option<u8>) -> MachineFactsSnapshot {
    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        MachineContainerObservationSnapshot::try_new(machine_id.clone(), Vec::new())
            .expect("empty container snapshot is valid"),
        public_ip_octet.map(|octet| MachineEndpointObservation {
            machine_id: machine_id.clone(),
            control_endpoints: vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, octet))],
            mesh_endpoints: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(203, 0, 113, octet)),
                DEFAULT_WIREGUARD_LISTEN_PORT,
            )],
        }),
        test_disk_space(),
        1,
    )
    .expect("machine facts are valid")
}

fn test_disk_space() -> ployz_core::machine_runtime::MachineDiskSpace {
    ployz_test_support::fixtures::test_disk_space()
}
