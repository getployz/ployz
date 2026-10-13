//! Real setup RPC deadlines and Enroll resends under virtual time.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::connect::Client;
use ployz_core::{
    EnrollAs, EnrollRequest, Enrolled, InspectRequest, Left, LeftPhase, LocalMachinePhase, Machine,
    MachineId, MachineName, MachineNow, OpaquePayload, RpcError, RpcErrorCode, RpcRequestBody,
    RpcResponse, WireGuardPublicKey, op,
};
use tokio::time::Instant;
use tonic::{Request, Response, Status};

fn founder() -> Machine {
    Machine {
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
        build_concurrency: None,
    }
}

/// Every Enroll before `ready` hangs; later ones get `reply`.
async fn enrolling_machine(
    reply: Enrolled,
) -> (
    Client,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let enrolls = Arc::new(AtomicUsize::new(0));
    let calls = enrolls.clone();
    let ready = Instant::now() + Duration::from_secs(70);
    let rpc = move |request: Request<OpaquePayload>| {
        let calls = calls.clone();
        let reply = reply.clone();
        async move {
            #[expect(
                clippy::wildcard_enum_match_arm,
                reason = "this fixture accepts setup RPCs and connection confirmation"
            )]
            match request.into_inner().decode_request().unwrap().body {
                RpcRequestBody::DescribeContract(_) => Ok(Response::new(
                    RpcResponse::from(ployz_core::ContractDescription {
                        machine_id: MachineId::random(),
                        protocol_major: ployz_core::PROTOCOL_MAJOR,
                        daemon_version: "fixture".into(),
                        capabilities: Default::default(),
                    })
                    .encode()
                    .unwrap(),
                )),
                RpcRequestBody::Enroll(_) => {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if Instant::now() < ready {
                        std::future::pending::<Result<Response<OpaquePayload>, Status>>().await
                    } else {
                        Ok(Response::new(RpcResponse::from(reply).encode().unwrap()))
                    }
                }
                RpcRequestBody::Inspect(_) => {
                    std::future::pending::<Result<Response<OpaquePayload>, Status>>().await
                }
                request => panic!("unexpected setup request: {request:?}"),
            }
        }
    };
    let (client, server) = crate::connect::test_support::rpc_client(rpc).await;
    (client, enrolls, server)
}

fn founder_request(machine: &Machine) -> EnrollRequest {
    EnrollRequest {
        role: EnrollAs::Founder {
            initial_policy: Default::default(),
            name: machine.name.clone(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            public_ip: None,
            advertised_endpoints: Vec::new(),
        },
        wireguard_mtu: None,
    }
}

#[tokio::test(start_paused = true)]
async fn enroll_resends_past_the_read_budget_until_the_machine_replies() {
    let machine = founder();
    let (mut client, enrolls, server) = enrolling_machine(Enrolled::Enrolled {
        machine: machine.clone(),
    })
    .await;
    let started = Instant::now();
    let enrolled = super::enroll(&mut client, founder_request(&machine))
        .await
        .unwrap();
    assert_eq!(enrolled, machine);
    assert!(Instant::now() - started >= Duration::from_secs(70));
    assert!(enrolls.load(Ordering::SeqCst) > 1, "a lost reply is resent");
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_refused_enroll_is_reported_not_resent() {
    let machine = founder();
    let refused = Enrolled::Refused {
        now: MachineNow {
            id: machine.id,
            phase: LocalMachinePhase::Participating,
            machine: Some(machine.clone()),
        },
        error: RpcError {
            code: RpcErrorCode::Conflict,
            message: "holds a different enrollment".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        },
    };
    let (mut client, enrolls, server) = enrolling_machine(refused).await;
    tokio::time::advance(Duration::from_secs(71)).await;
    let error = super::enroll(&mut client, founder_request(&machine))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("holds a different enrollment"));
    assert_eq!(enrolls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn held_setup_rpcs_keep_the_mutation_and_read_deadlines() {
    let machine = founder();
    let (mut client, enrolls, server) = enrolling_machine(Enrolled::Enrolled {
        machine: machine.clone(),
    })
    .await;
    let started = Instant::now();
    let error = client
        .call_unretried::<op::Enroll>(
            founder_request(&machine),
            None,
            crate::cluster::LIVE_MUTATION_REPLY_TIMEOUT,
        )
        .await
        .unwrap_err();
    assert_eq!(Instant::now() - started, Duration::from_secs(60));
    assert!(crate::ui::chain_text(&error).contains("Machine setup mutation reply timed out"));
    assert_eq!(enrolls.load(Ordering::SeqCst), 1);
    let started = Instant::now();
    let error = client
        .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
        .await
        .unwrap_err();
    assert_eq!(Instant::now() - started, Duration::from_secs(60));
    assert!(crate::ui::chain_text(&error).contains("Machine setup read timed out"));
    server.abort();
}

#[tokio::test]
async fn leave_lost_to_a_daemon_restart_is_resent() {
    use crate::connect::test_support::{unix_client, unix_daemon};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ployz.sock");
    let (leave, restarting) = tokio::sync::oneshot::channel();
    let leave = Arc::new(std::sync::Mutex::new(Some(leave)));
    let old = unix_daemon(&path, move |body| {
        let leave = leave.lock().unwrap().take();
        async move {
            assert!(matches!(body, RpcRequestBody::Leave(_)), "{body:?}");
            leave.unwrap().send(()).unwrap();
            std::future::pending().await
        }
    });
    let mut client = unix_client(&path).await;
    let resent = Arc::new(AtomicUsize::new(0));
    let restart = {
        let resent = Arc::clone(&resent);
        async move {
            restarting.await.unwrap();
            // The daemon exits with the Leave reply in flight.
            old.shutdown_background();
            std::fs::remove_file(&path).unwrap();
            unix_daemon(&path, move |body| {
                let resent = Arc::clone(&resent);
                async move {
                    assert!(matches!(body, RpcRequestBody::Leave(_)), "{body:?}");
                    resent.fetch_add(1, Ordering::SeqCst);
                    Ok(RpcResponse::from(Left::Left {
                        phase: LeftPhase::Uninitialized,
                        reset_warning: None,
                    }))
                }
            })
        }
    };
    let (result, new) = tokio::join!(super::leave(&mut client), restart);
    new.shutdown_background();
    result.unwrap();
    assert_eq!(resent.load(Ordering::SeqCst), 1);
}
