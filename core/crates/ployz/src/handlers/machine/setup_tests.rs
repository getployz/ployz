//! Real setup RPC deadlines and initialization recovery under virtual time.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::connect::Client;
use ployz_core::{
    InitializeRequest, InspectRequest, LocalMachinePhase, Machine, MachineDetails, MachineId,
    MachineName, OpaquePayload, RpcRequestBody, RpcResponse, WireGuardPublicKey, op,
};
use tokio::time::Instant;
use tonic::{Request, Response, Status};

async fn starting_machine() -> (
    Client,
    Machine,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let machine = Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::random(),
        name: MachineName::parse("founder").unwrap(),
        subnet: "10.210.0.0/24".parse().unwrap(),
        public_key: WireGuardPublicKey([2; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
    };
    let initialized = Arc::new(AtomicUsize::new(0));
    let calls = initialized.clone();
    let observed = machine.clone();
    let ready = Instant::now() + Duration::from_secs(70);
    let rpc = move |request: Request<OpaquePayload>| {
        let calls = calls.clone();
        let machine = observed.clone();
        async move {
            #[expect(
                clippy::wildcard_enum_match_arm,
                reason = "this fixture accepts setup RPCs and connection confirmation"
            )]
            match request.into_inner().decode_request().unwrap().body {
                RpcRequestBody::DescribeContract(_) => Ok(Response::new(
                    RpcResponse::from(ployz_core::ContractDescription {
                        machine_id: machine.id,
                        protocol_major: ployz_core::PROTOCOL_MAJOR,
                        daemon_version: "fixture".into(),
                        capabilities: Default::default(),
                    })
                    .encode()
                    .unwrap(),
                )),
                RpcRequestBody::Initialize(_) | RpcRequestBody::Join(_) => {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<Result<Response<OpaquePayload>, Status>>().await
                }
                RpcRequestBody::Inspect(_) => {
                    let started = calls.load(Ordering::SeqCst) > 0;
                    if started {
                        tokio::time::sleep_until(ready).await;
                    }
                    let details = MachineDetails {
                        id: machine.id,
                        phase: if started {
                            LocalMachinePhase::Participating
                        } else {
                            LocalMachinePhase::Uninitialized
                        },
                        public_key: machine.public_key,
                        advertised_endpoints: Vec::new(),
                        machine: started.then_some(machine),
                        store_version: Default::default(),
                        rtts: Vec::new(),
                        cloud_paired: false,
                        telemetry: None,
                        storage: None,
                    };
                    Ok(Response::new(RpcResponse::from(details).encode().unwrap()))
                }
                request => panic!("unexpected setup request: {request:?}"),
            }
        }
    };
    let (client, server) = crate::connect::test_support::rpc_client(rpc).await;
    (client, machine, initialized, server)
}

fn initialize_request(machine: &Machine) -> InitializeRequest {
    InitializeRequest {
        initial_policy: Default::default(),
        name: machine.name.clone(),
        cluster_network: "10.210.0.0/16".parse().unwrap(),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        wireguard_mtu: None,
        cloud_pairing: None,
    }
}

#[tokio::test(start_paused = true)]
async fn initialize_recovers_beyond_the_read_budget_without_replaying_the_mutation() {
    let started = Instant::now();
    let (mut client, machine, calls, server) = starting_machine().await;
    let result = super::initialize(&mut client, initialize_request(&machine))
        .await
        .unwrap();
    assert_eq!(result.machine, machine);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(Instant::now() - started, Duration::from_secs(70));
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn held_setup_rpcs_keep_the_mutation_and_read_deadlines() {
    let (mut client, machine, calls, server) = starting_machine().await;
    let started = Instant::now();
    let error = client
        .call_unretried::<op::Initialize>(initialize_request(&machine), None)
        .await
        .unwrap_err();
    assert_eq!(Instant::now() - started, Duration::from_secs(5));
    assert!(
        error
            .to_string()
            .contains("Machine setup mutation reply timed out")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let started = Instant::now();
    let error = client
        .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
        .await
        .unwrap_err();
    assert_eq!(Instant::now() - started, Duration::from_secs(60));
    assert!(error.to_string().contains("Machine setup read timed out"));
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn lost_creation_replies_refuse_an_observed_policy_mismatch() {
    for joining in [false, true] {
        let (mut client, machine, calls, server) = starting_machine().await;
        let result = if joining {
            let mut assigned = machine.clone();
            assigned.accepts_services = false;
            super::join(
                &mut client,
                ployz_core::JoinRequest {
                    registration: ployz_core::Registered {
                        assigned_machine: assigned,
                        visible_peers: Vec::new(),
                        target_versions: Default::default(),
                    },
                    wireguard_mtu: None,
                    cloud_pairing: None,
                },
            )
            .await
        } else {
            let mut request = initialize_request(&machine);
            request.initial_policy.accepts_services = false;
            super::initialize(&mut client, request).await.map(drop)
        };
        assert!(
            result.is_err(),
            "matching identity cannot prove a different initial policy completed"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("initial policy differs")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
