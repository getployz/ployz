use std::collections::BTreeMap;

use ployz_core::{
    AdvertisedEndpoint, ContainerRuntimeObservation, DockerVolume, DockerVolumeId,
    DockerVolumeName, DockerVolumeStorageObservation, HealthObservation, Machine, MachinePath,
    MembershipObservation, Namespace, ServiceId, ServiceName, WireGuardPublicKey,
};
use serde_json::{Value, json};

use super::*;
use crate::connect::stop_rpc_timeout;

#[test]
fn unspecified_stop_timeout_has_no_rpc_deadline() {
    assert_eq!(stop_rpc_timeout(None, 1), None);
    assert_eq!(
        stop_rpc_timeout(Some(5), 1),
        Some(TARGET_RPC_TIMEOUT + Duration::from_secs(5))
    );
    // Stops on one Machine run one at a time: the last waits behind the others.
    assert_eq!(
        stop_rpc_timeout(Some(10), 2),
        Some(TARGET_RPC_TIMEOUT + Duration::from_secs(20))
    );
}

#[test]
fn an_unanswered_call_names_the_machine_in_its_deepest_line() {
    let machine = machine_id('a');
    let lost = ployz_core::rpc::target_unanswered_status(&std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "deadline has elapsed",
    ));

    let error = answered_by(&machine, ConnectError::from(lost));

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        format!("Machine {machine} did not answer: deadline has elapsed")
    );
    assert!(error.cause.is_empty(), "{:?}", error.cause);
    assert!(error.target_unanswered());
    let row = ployz_store::Failure::of(
        &crate::deploy::ExecutionError::Machine {
            action: ployz_core::MachineAction::CreateContainer,
            error,
        },
        &[],
    );
    assert_eq!(
        row.cause.last(),
        Some(&format!(
            "Machine {machine} did not answer: deadline has elapsed"
        ))
    );
}

#[test]
fn a_machine_answer_keeps_its_own_words() {
    let answered = unavailable("Docker Volume was created but could not be verified");

    let error = answered_by(&machine_id('a'), ConnectError::Remote(answered.clone()));

    assert_eq!(error, answered);
}

#[test]
fn a_call_whose_connection_dropped_may_still_have_done_the_work() {
    let dropped = answered_by(
        &machine_id('a'),
        ConnectError::from(tonic::Status::from_error(Box::new(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "connection lost",
        )))),
    );
    assert!(dropped.reply_lost(), "{dropped:?}");
    assert_eq!(
        dropped.details.get("machine_id"),
        Some(&json!(machine_id('a').to_string()))
    );

    let sent = answered_by(
        &machine_id('a'),
        ConnectError::from(tonic::Status::unavailable("Cluster is not available")),
    );
    assert!(!sent.reply_lost(), "the daemon answered: {sent:?}");
}

#[test]
fn a_machine_that_sends_unavailable_itself_is_not_called_silent() {
    let error = answered_by(
        &machine_id('a'),
        ConnectError::from(tonic::Status::unavailable("Cluster is not available")),
    );

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "Cluster is not available");
    assert!(!error.target_unanswered());
}

#[test]
fn a_lost_entry_does_not_blame_the_target() {
    let error = answered_by(
        &machine_id('a'),
        ConnectError::from(tonic::Status::deadline_exceeded(
            "entry Machine reconnect timed out",
        )),
    );

    assert_eq!(error.message, "entry Machine reconnect timed out");
}

#[test]
fn deploy_snapshot_keeps_successful_observations_and_query_gaps() {
    let machines = vec![machine('a'), machine('b')];
    let container = observation('1', 'a');
    let mut volume = docker_volume('a', "data");
    volume.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/data").unwrap(),
        bound_bytes: std::num::NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 42,
        role: None,
        slot_role: None,
    };
    let containers = PartialResult {
        successes: vec![MachineSuccess {
            machine_id: machine_id('a'),
            value: vec![container.clone()],
        }],
        failures: vec![MachineFailure {
            machine_id: machine_id('b'),
            error: unavailable("container listing failed"),
        }],
        omissions: vec![machine_id('c')],
    };
    let volumes = PartialResult {
        successes: vec![MachineSuccess {
            machine_id: machine_id('a'),
            value: VolumeInventory {
                volumes: vec![volume.clone()],
                failures: vec![ployz_core::VolumeObservationFailure {
                    id: ployz_core::DockerVolumeId {
                        machine_id: machine_id('a'),
                        name: DockerVolumeName::parse("unavailable").unwrap(),
                    },
                    error: unavailable("volume detail failed"),
                }],
            },
        }],
        failures: vec![MachineFailure {
            machine_id: machine_id('b'),
            error: unavailable("volume listing failed"),
        }],
        omissions: Vec::new(),
    };
    let expected_container_failures = containers.failures.clone();
    let expected_container_omissions = containers.omissions.clone();
    let snapshot =
        snapshot_from_partial(machines.clone(), containers, volumes, BTreeMap::new()).unwrap();

    assert_eq!(snapshot.machines, machines);
    assert_eq!(snapshot.containers, [container]);
    assert_eq!(snapshot.volume_snapshot.observations(), [volume]);
    assert!(
        snapshot
            .volume_snapshot
            .named_failures()
            .iter()
            .any(|failure| {
                let message = failure.to_string();
                message.contains(&machine_id('a').to_string())
                    && message.contains("Docker Volume unavailable")
            })
    );
    assert_eq!(snapshot.container_failures, expected_container_failures);
    assert_eq!(snapshot.container_omissions, expected_container_omissions);
    assert_eq!(
        snapshot
            .volume_snapshot
            .machine_gap(machine_id('b'))
            .as_deref(),
        Some("Docker Volume inventory failed: volume listing failed")
    );
    assert!(!snapshot.is_observer_complete());
}

#[tokio::test(start_paused = true)]
async fn deploy_snapshot_waits_out_a_silent_machine_once_not_per_phase() {
    use ployz_core::{
        ContainerList, InspectTelemetry, LocalMachinePhase, MachineDetails, RpcRequestBody,
        RpcResponse, TelemetryObservation,
    };

    let live = machine('a');
    let silent = machine('b');
    let answering = live.machine.clone();
    let (client, server) =
        crate::connect::test_support::rpc_client(move |rpc: tonic::Request<OpaquePayload>| {
            let answering = answering.clone();
            async move {
                let target = rpc.metadata().get(ployz_core::ONE_TARGET_HEADER).cloned();
                let body = rpc.into_inner().decode_request().unwrap().body;
                if target.is_some_and(|target| target != answering.id.as_str()) {
                    std::future::pending::<()>().await;
                }
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "fixture answers only the Deploy Snapshot reads"
                )]
                let response = match body {
                    RpcRequestBody::DescribeContract(_) => {
                        RpcResponse::from(ployz_core::ContractDescription {
                            machine_id: answering.id,
                            protocol_major: ployz_core::PROTOCOL_MAJOR,
                            daemon_version: "fixture".into(),
                            capabilities: Default::default(),
                        })
                    }
                    RpcRequestBody::ListContainers(_) => RpcResponse::from(ContainerList {
                        containers: Vec::new(),
                    }),
                    RpcRequestBody::ListVolumes(_) => RpcResponse::from(VolumeInventory {
                        volumes: Vec::new(),
                        failures: Vec::new(),
                    }),
                    RpcRequestBody::Inspect(inspect) => {
                        assert_eq!(inspect.telemetry, InspectTelemetry::BridgeCapacity);
                        RpcResponse::from(MachineDetails {
                            id: answering.id,
                            phase: LocalMachinePhase::Participating,
                            public_key: answering.public_key,
                            advertised_endpoints: Vec::new(),
                            machine: Some(answering),
                            store_version: Default::default(),
                            rtts: Vec::new(),
                            management_clients: Vec::new(),
                            telemetry: Some(TelemetryObservation::BridgeCapacity {
                                bridge: BridgeEndpointCapacity::new(10, 0),
                            }),
                            storage: None,
                            upgrade: None,
                            token: None,
                            sealing: None,
                        })
                    }
                    other => panic!("unexpected Deploy Snapshot RPC: {other:?}"),
                };
                Ok(tonic::Response::new(response.encode().unwrap()))
            }
        })
        .await;

    let started = tokio::time::Instant::now();
    let snapshot = client
        .deploy_snapshot(vec![live.clone(), silent.clone()])
        .await
        .unwrap();
    let elapsed = started.elapsed();
    server.abort();

    assert!(
        elapsed >= TARGET_RPC_TIMEOUT && elapsed < 2 * TARGET_RPC_TIMEOUT,
        "one silent Machine costs one read deadline, not one per phase: {elapsed:?}"
    );
    assert_eq!(
        snapshot
            .container_failures
            .iter()
            .map(|failure| failure.machine_id)
            .collect::<Vec<_>>(),
        vec![silent.machine.id]
    );
    assert_eq!(
        snapshot.capacity.unwrap().into_keys().collect::<Vec<_>>(),
        vec![live.machine.id]
    );
}

#[test]
fn targeted_volume_inventory_rejects_unsafe_routing_evidence() {
    let target = machine_id('a');
    let failure = |machine, name: &str| ployz_core::VolumeObservationFailure {
        id: DockerVolumeId {
            machine_id: machine_id(machine),
            name: DockerVolumeName::parse(name).unwrap(),
        },
        error: unavailable("inspect failed"),
    };

    for inventory in [
        VolumeInventory {
            volumes: vec![docker_volume('b', "wrong-machine")],
            failures: Vec::new(),
        },
        VolumeInventory {
            volumes: Vec::new(),
            failures: vec![failure('b', "wrong-machine")],
        },
        VolumeInventory {
            volumes: vec![
                docker_volume('a', "duplicate"),
                docker_volume('a', "duplicate"),
            ],
            failures: Vec::new(),
        },
        VolumeInventory {
            volumes: vec![docker_volume('a', "contradiction")],
            failures: vec![failure('a', "contradiction")],
        },
    ] {
        assert_eq!(
            validate_volume_inventory(target, inventory)
                .expect_err("unsafe inventory is rejected before MachineSuccess")
                .code,
            RpcErrorCode::Internal
        );
    }

    assert!(
        validate_volume_inventory(
            target,
            VolumeInventory {
                volumes: vec![docker_volume('a', "observed")],
                failures: vec![failure('a', "unavailable")],
            },
        )
        .is_ok()
    );
}

#[test]
fn volume_snapshot_rejects_duplicate_and_contradictory_evidence() {
    let volume = docker_volume('a', "data");
    let named_failure = || ployz_core::VolumeObservationFailure {
        id: volume.id.clone(),
        error: unavailable("inspect failed"),
    };
    let machine_failure = || MachineFailure {
        machine_id: machine_id('a'),
        error: unavailable("list failed"),
    };

    for result in [
        VolumeSnapshot::try_from_observations(vec![volume.clone(), volume.clone()]),
        VolumeSnapshot::try_from_parts(
            vec![volume.clone()],
            vec![named_failure()],
            Vec::new(),
            Vec::new(),
        ),
        VolumeSnapshot::try_from_parts(
            Vec::new(),
            vec![named_failure(), named_failure()],
            Vec::new(),
            Vec::new(),
        ),
        VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            vec![machine_failure(), machine_failure()],
            Vec::new(),
        ),
        VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            vec![machine_failure()],
            vec![machine_id('a')],
        ),
        VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![machine_id('a'), machine_id('a')],
        ),
        VolumeSnapshot::try_from_parts(
            vec![volume.clone()],
            Vec::new(),
            Vec::new(),
            vec![machine_id('a')],
        ),
    ] {
        assert_eq!(
            result
                .expect_err("invalid Volume Snapshot evidence is rejected")
                .code,
            RpcErrorCode::InvalidArgument
        );
    }

    let partial = PartialResult {
        successes: vec![MachineSuccess {
            machine_id: machine_id('a'),
            value: VolumeInventory {
                volumes: vec![volume],
                failures: Vec::new(),
            },
        }],
        failures: vec![machine_failure()],
        omissions: Vec::new(),
    };
    assert_eq!(
        VolumeSnapshot::from_partial(partial).unwrap_err().code,
        RpcErrorCode::Internal
    );
}

fn machine(hex: char) -> MachineObservation {
    MachineObservation::new(
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: machine_id(hex),
            name: MachineName::parse(format!("machine-{hex}")).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
            build_concurrency: None,
        },
        MembershipObservation::Up,
    )
}

fn machine_id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

fn observation(id: char, machine: char) -> ContainerObservation {
    let service_id = ServiceId::parse(id.to_string().repeat(32)).unwrap();
    let service_name = ServiceName::parse("api").unwrap();
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        rotation: None,
        r#gen: None,
        container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: "api".into(),
        created_at_unix_nanos: 0,
        machine_id: machine_id(machine),
        namespace: Namespace::parse("app").unwrap(),
        kind: ployz_core::ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
        }))
        .unwrap(),
        address: None,
        labels: BTreeMap::new(),
        stopped: false,
        run: None,
    })
    .unwrap()
}

fn docker_volume(machine: char, name: &str) -> DockerVolume {
    DockerVolume {
        id: DockerVolumeId {
            machine_id: machine_id(machine),
            name: DockerVolumeName::parse(name).unwrap(),
        },
        options: BTreeMap::from([("type".into(), "none".into())]),
        labels: BTreeMap::from([("keep".into(), "out".into())]),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    }
}

fn unavailable(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.into(),
        details: Value::Null,
        cause: Vec::new(),
    }
}

#[test]
fn a_reset_does_not_report_the_builder_cache_as_lost() {
    let cache = format!("buildx_buildkit_{}0_state", ployz_build::builder_name());
    let lost = volumes_lost(VolumeInventory {
        volumes: vec![docker_volume('a', &cache), docker_volume('a', "app_data")],
        failures: Vec::new(),
    });
    assert_eq!(
        lost,
        [DataLoss::DockerVolume {
            id: docker_volume('a', "app_data").id
        }]
    );
}

#[test]
fn an_ingress_off_wait_holds_only_once_the_role_is_off() {
    let mut observed = machine('a').machine;
    assert!(!RoleSetting::IngressOff.holds(&observed));
    observed.accepts_ingress = false;
    assert!(RoleSetting::IngressOff.holds(&observed));
    assert!(!RoleSetting::IngressOn.holds(&observed));
}

/// A Machine that drops `ListMachines` and refuses to describe itself, as a bare
/// endpoint now held by another daemon does.
async fn redialed_into_a_refusal(bounded: bool) -> Option<MachineId> {
    let (mut client, server) =
        crate::connect::test_support::rpc_client(|rpc: tonic::Request<OpaquePayload>| async move {
            let body = rpc.into_inner().decode_request().unwrap().body;
            if matches!(body, ployz_core::RpcRequestBody::DescribeContract(_)) {
                Err(tonic::Status::unauthenticated("not authorized"))
            } else {
                Err(tonic::Status::unavailable("restarting"))
            }
        })
        .await;
    client.entry = Some(machine_id('a'));
    let refused = if bounded {
        client
            .try_read::<op::ListMachines>(
                ListMachinesRequest {},
                &MachineTarget::parse("machine-a").unwrap(),
            )
            .await
    } else {
        client
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
    };
    assert!(refused.is_err(), "{refused:?}");
    server.abort();
    client.entry_machine()
}

#[tokio::test(start_paused = true)]
async fn a_redial_the_new_daemon_refuses_forgets_the_machine_it_replaced() {
    assert_eq!(redialed_into_a_refusal(false).await, None);
    assert_eq!(redialed_into_a_refusal(true).await, None);
}

#[tokio::test]
async fn a_redial_whose_entry_fails_its_check_leaves_every_clone_on_the_old_channel() {
    let (mut client, _old) = crate::connect::test_support::rpc_client(|_| async {
        Err(tonic::Status::not_found("answered by the old daemon"))
    })
    .await;
    let (other, _new) = crate::connect::test_support::rpc_client(|_| async {
        Err(tonic::Status::unavailable("not ready"))
    })
    .await;
    client.connector = crate::connect::test_support::connector(other.channel());
    let clone = client.clone();

    assert!(client.redial_shared().await.is_err());

    let answer = clone.entry_id().await.unwrap_err();
    assert!(format!("{answer:?}").contains("old daemon"), "{answer:?}");
}

#[tokio::test]
async fn a_redial_that_reaches_another_machine_is_refused_and_keeps_the_old_channel() {
    let (mut client, _old) = crate::connect::test_support::rpc_client(|_| async {
        Err(tonic::Status::not_found("answered by the old daemon"))
    })
    .await;
    let (other, _new) = crate::connect::test_support::rpc_client(|_| async {
        let described = ployz_core::RpcResponse::from(ployz_core::ContractDescription {
            machine_id: machine_id('b'),
            protocol_major: ployz_core::PROTOCOL_MAJOR,
            daemon_version: "test".into(),
            capabilities: Default::default(),
        });
        Ok(tonic::Response::new(described.encode().unwrap()))
    })
    .await;
    client.entry = Some(machine_id('a'));
    client.confirmed = Some(machine_id('a'));
    client.connector = crate::connect::test_support::connector(other.channel());

    // A client whose last redial failed has forgotten its entry, and still refuses.
    let mut forgot = client.clone();
    forgot.entry = None;
    for redialing in [client.clone(), forgot] {
        let refused = redialing.redial_shared().await;
        assert!(
            matches!(refused, Err(ConnectError::IdentityMismatch { .. })),
            "{refused:?}"
        );
    }

    let answer = client.clone().entry_id().await.unwrap_err();
    assert!(format!("{answer:?}").contains("old daemon"), "{answer:?}");
}

#[tokio::test]
async fn a_shared_redial_whose_daemon_will_not_say_which_machine_it_is_keeps_the_old_channel() {
    let (mut client, _old) = crate::connect::test_support::rpc_client(|_| async {
        Err(tonic::Status::not_found("answered by the old daemon"))
    })
    .await;
    let (other, _new) = crate::connect::test_support::rpc_client(|_| async {
        let refused = ployz_core::RpcResponse::from(unavailable("not this Machine's caller"));
        Ok(tonic::Response::new(refused.encode().unwrap()))
    })
    .await;
    client.entry = Some(machine_id('a'));
    client.confirmed = Some(machine_id('a'));
    client.connector = crate::connect::test_support::connector(other.channel());

    let refused = client.redial_shared().await;
    assert!(
        matches!(refused, Err(ConnectError::Remote(_))),
        "{refused:?}"
    );

    // A retry's redial forgets its entry before it dials, and still refuses.
    let refused = client.clone().redial().await;
    assert!(
        matches!(refused, Err(ConnectError::Remote(_))),
        "{refused:?}"
    );

    let answer = client.clone().entry_id().await.unwrap_err();
    assert!(format!("{answer:?}").contains("old daemon"), "{answer:?}");
}

#[tokio::test]
async fn a_retry_redial_that_reaches_another_machine_moves_only_that_client() {
    let (mut client, _old) = crate::connect::test_support::rpc_client(|_| async {
        Err(tonic::Status::not_found("answered by the old daemon"))
    })
    .await;
    let (other, _new) = crate::connect::test_support::rpc_client(|_| async {
        let described = ployz_core::RpcResponse::from(ployz_core::ContractDescription {
            machine_id: machine_id('b'),
            protocol_major: ployz_core::PROTOCOL_MAJOR,
            daemon_version: "test".into(),
            capabilities: Default::default(),
        });
        Ok(tonic::Response::new(described.encode().unwrap()))
    })
    .await;
    client.entry = Some(machine_id('a'));
    client.confirmed = Some(machine_id('a'));
    client.connector = crate::connect::test_support::connector(other.channel());

    // As a Server reset under `server add` does, the endpoint now answers as another
    // Machine; the retry follows it. A clone whose last redial failed has forgotten
    // its entry, and still moves only itself.
    let mut forgot = client.clone();
    forgot.entry = None;
    for mut retrying in [client.clone(), forgot] {
        retrying.redial().await.unwrap();
        assert_eq!(retrying.entry_machine(), Some(machine_id('b')));
        assert_eq!(retrying.entry_id().await.unwrap(), machine_id('b'));
    }

    let answer = client.clone().entry_id().await.unwrap_err();
    assert!(format!("{answer:?}").contains("old daemon"), "{answer:?}");
}

/// Parallel forgets through one entry: the Server's first answer is a retryable failure,
/// and reconnecting to the busy entry outlasts the read, so whether it holds copies stays
/// unknown.
#[tokio::test(start_paused = true)]
async fn a_forget_whose_entry_reconnect_fails_says_to_run_it_again() {
    let (mut client, server) =
        crate::connect::test_support::rpc_client(|rpc: tonic::Request<OpaquePayload>| async move {
            let body = rpc.into_inner().decode_request().unwrap().body;
            if matches!(body, ployz_core::RpcRequestBody::DescribeContract(_)) {
                tokio::time::sleep(std::time::Duration::from_millis(4900)).await;
            }
            Err(tonic::Status::unavailable("restarting"))
        })
        .await;
    client.entry = Some(machine_id('a'));

    let error = client.held_copies(&machine('b')).await.unwrap_err();

    assert_eq!(
        error.message,
        "Couldn't check whether Server machine-b holds Volume copies. No changes made. Run the \
         command again, one Server at a time if you're forgetting several."
    );
    assert_eq!(error.cause, ["entry Machine reconnect timed out"]);
    server.abort();
}

/// A forget racing Cloud's finished removal, #3612: the entry no longer lists the Server
/// by the time its copies are read, so it is gone rather than unchecked.
#[tokio::test]
async fn a_server_the_entry_no_longer_lists_reads_gone() {
    let (client, server) =
        crate::connect::test_support::rpc_client(|rpc: tonic::Request<OpaquePayload>| async move {
            let body = rpc.into_inner().decode_request().unwrap().body;
            assert!(
                matches!(body, ployz_core::RpcRequestBody::InspectStorage(_)),
                "{body:?}"
            );
            let id = machine_id('b').to_string();
            Err(ployz_core::rpc::hinted_status(
                tonic::Code::NotFound,
                &std::io::Error::other(format!("No Server named {id:?}")),
                json!({
                    "inspect": ["ployz server ls"],
                    "reason": ployz_core::rpc::NO_SUCH_SERVER_REASON,
                    "targets": [id],
                }),
            ))
        })
        .await;

    let held = client.held_copies(&machine('b')).await;

    assert!(matches!(held, Ok(HeldCopies::Gone)), "{held:?}");
    server.abort();
}

/// Parallel forgets through one busy entry, #2934: the entry's router says the Server didn't
/// answer, so the entry itself answered, and reconnecting to it would outlast the read.
#[tokio::test(start_paused = true)]
async fn a_server_the_entry_reports_silent_reads_dead_without_reconnecting_to_the_entry() {
    let reconnects = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counted = std::sync::Arc::clone(&reconnects);
    let (mut client, server) =
        crate::connect::test_support::rpc_client(move |rpc: tonic::Request<OpaquePayload>| {
            let counted = std::sync::Arc::clone(&counted);
            async move {
                let body = rpc.into_inner().decode_request().unwrap().body;
                if matches!(body, ployz_core::RpcRequestBody::DescribeContract(_)) {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(4900)).await;
                    return Err(tonic::Status::unavailable("busy"));
                }
                Err(ployz_core::rpc::target_unanswered_status(
                    &std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline has elapsed"),
                ))
            }
        })
        .await;
    client.entry = Some(machine_id('a'));

    let held = client.held_copies(&machine('b')).await;

    assert!(matches!(held, Ok(HeldCopies::Dead)), "{held:?}");
    assert_eq!(reconnects.load(std::sync::atomic::Ordering::SeqCst), 0);
    server.abort();
}

#[test]
fn an_unanswered_listing_keeps_whether_the_router_could_dial_the_target() {
    let timed_out = std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline has elapsed");
    let silent = unanswered_kept(ConnectError::from(
        ployz_core::rpc::target_unanswered_status(&timed_out),
    ));
    assert!(
        silent.target_unanswered() && !silent.undialed(),
        "{silent:?}"
    );
    let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connect refused");
    let undialed = unanswered_kept(ConnectError::from(
        ployz_core::rpc::target_unreached_status(&refused),
    ));
    assert!(
        undialed.target_unanswered() && undialed.undialed(),
        "{undialed:?}"
    );
}
