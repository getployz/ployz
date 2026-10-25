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
        pinned: Default::default(),
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

/// Answers Inspect as Participating, and ListMachines with the restarting
/// daemon's Unavailable refusal for the first `refusals` calls.
async fn restarting_cluster_store(
    machine: Machine,
    refusals: usize,
) -> (
    Client,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let lists = Arc::new(AtomicUsize::new(0));
    let calls = lists.clone();
    let rpc = move |request: Request<OpaquePayload>| {
        let calls = calls.clone();
        let machine = machine.clone();
        async move {
            #[expect(
                clippy::wildcard_enum_match_arm,
                reason = "this fixture accepts readiness RPCs and connection confirmation"
            )]
            let response = match request.into_inner().decode_request().unwrap().body {
                RpcRequestBody::DescribeContract(_) => {
                    RpcResponse::from(ployz_core::ContractDescription {
                        machine_id: machine.id,
                        protocol_major: ployz_core::PROTOCOL_MAJOR,
                        daemon_version: "fixture".into(),
                        capabilities: Default::default(),
                    })
                }
                RpcRequestBody::Inspect(_) => RpcResponse::from(ployz_core::MachineDetails {
                    id: machine.id,
                    phase: LocalMachinePhase::Participating,
                    machine: Some(machine.clone()),
                    public_key: machine.public_key,
                    advertised_endpoints: Vec::new(),
                    store_version: Default::default(),
                    rtts: Vec::new(),
                    management_clients: Vec::new(),
                    telemetry: None,
                    storage: None,
                    upgrade: None,
                    token: None,
                    sealing: None,
                }),
                RpcRequestBody::ListMachines(_) => {
                    if calls.fetch_add(1, Ordering::SeqCst) < refusals {
                        RpcResponse::from(RpcError {
                            code: RpcErrorCode::Unavailable,
                            message: "Cluster store is not available".into(),
                            details: serde_json::Value::Null,
                            cause: Vec::new(),
                        })
                    } else {
                        RpcResponse::from(ployz_core::MachineList {
                            enrollment: None,
                            machines: vec![ployz_core::MachineObservation::new(
                                machine,
                                ployz_core::MembershipObservation::Up,
                            )],
                        })
                    }
                }
                request => panic!("unexpected readiness request: {request:?}"),
            };
            Ok(Response::new(response.encode().unwrap()))
        }
    };
    let (client, server) = crate::connect::test_support::rpc_client(rpc).await;
    (client, lists, server)
}

#[tokio::test(start_paused = true)]
async fn readiness_waits_out_a_cluster_store_that_is_still_opening() {
    let (mut client, lists, server) = restarting_cluster_store(founder(), 3).await;
    crate::setup_retry::run(
        &mut client,
        "Waiting for the participating phase",
        ployz_core::MACHINE_START_WAIT,
        crate::connect::ConnectError::is_setup_retryable,
        async |client| super::check_listed(client).await,
    )
    .await
    .unwrap();
    assert_eq!(lists.load(Ordering::SeqCst), 4);
    server.abort();
}

/// The daemon restarts after it enrolls, so the readiness wait after a healthy join must
/// not read as a failure or send the user to their firewall (#3371).
#[test]
fn readiness_after_a_join_waits_for_the_restart_without_blaming_the_network() {
    // libtest captures eprintln!, so observe the notice from a child run.
    const CHILD: &str = "PLOYZ_JOIN_READINESS_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ployzd.sock");
        let machine = founder();
        let lists = Arc::new(AtomicUsize::new(0));
        let daemon = crate::connect::test_support::unix_daemon(&path, move |body| {
            let (machine, lists) = (machine.clone(), lists.clone());
            async move {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "this fixture answers only readiness RPCs"
                )]
                Ok(match body {
                    RpcRequestBody::Inspect(_) => RpcResponse::from(ployz_core::MachineDetails {
                        id: machine.id,
                        phase: LocalMachinePhase::Participating,
                        machine: Some(machine.clone()),
                        public_key: machine.public_key,
                        advertised_endpoints: Vec::new(),
                        store_version: Default::default(),
                        rtts: Vec::new(),
                        management_clients: Vec::new(),
                        telemetry: None,
                        storage: None,
                        upgrade: None,
                        token: None,
                        sealing: None,
                    }),
                    // The first answer comes from the daemon still opening its store.
                    RpcRequestBody::ListMachines(_)
                        if lists.fetch_add(1, Ordering::SeqCst) == 0 =>
                    {
                        RpcResponse::from(RpcError {
                            code: RpcErrorCode::Unavailable,
                            message: "Cluster store is not available".into(),
                            details: serde_json::Value::Null,
                            cause: Vec::new(),
                        })
                    }
                    RpcRequestBody::ListMachines(_) => RpcResponse::from(ployz_core::MachineList {
                        enrollment: None,
                        machines: vec![ployz_core::MachineObservation::new(
                            machine,
                            ployz_core::MembershipObservation::Up,
                        )],
                    }),
                    request => panic!("unexpected readiness request: {request:?}"),
                })
            }
        });
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "server", "ls"])
            .unwrap();
        let connection = crate::context::Connection::unix(&path).unwrap();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(super::wait_direct_participating(
                &matches,
                &connection,
                "not ready",
            ))
            .unwrap();
        daemon.shutdown_background();
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "handlers::server::helpers::setup_tests::readiness_after_a_join_waits_for_the_restart_without_blaming_the_network",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("Waiting for the Server's daemon to start…"),
        "{stderr}"
    );
    assert!(!stderr.contains("firewall"), "{stderr}");
    assert!(!stderr.contains("couldn't reach"), "{stderr}");
}
