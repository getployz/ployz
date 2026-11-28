use super::*;
use ployz_core::error_chain;

/// What `machine`'s daemon reported, `details`, as an enrollment that asked for `initial_policy`.
fn identified(
    machine: &Machine,
    details: MachineDetails,
    initial_policy: ployz_core::InitialMachinePolicy,
) -> Identified {
    let machine_token = MachineToken {
        id: machine.id,
        public_key: machine.public_key,
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        memory_total_bytes: None,
        disk_total_bytes: None,
        disk_available_bytes: None,
        first_key: None,
    };
    let identity = EnrollIdentity::from_machine_token(
        machine.name.clone(),
        &machine_token,
        ployz_core::StorageChoice::None,
        initial_policy.clone(),
        false,
    );
    Identified {
        details,
        machine_token,
        name: machine.name.clone(),
        initial_policy,
        identity,
    }
}

fn registered(machine: Machine) -> ployz_core::Registered {
    ployz_core::Registered {
        assigned_machine: machine,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
        sealing_key: None,
        devices: Vec::new(),
        grants: Vec::new(),
        restrictions: None,
    }
}

use std::{io, path::PathBuf};

use crate::context::ConnectionSource;

#[test]
fn wait_retries_no_config_and_unreachable_connect_errors() {
    assert!(retry_local_connect(&ConnectError::Context(
        ContextError::NoConfig
    )));
    assert!(retry_local_connect(&ConnectError::Io(io::Error::from(
        io::ErrorKind::ConnectionRefused
    ))));
    assert!(!retry_local_connect(&ConnectError::AllFailed {
        relayed: false,
        selection: ConnectionSource::LocalSocket,
        attempts: 1,
        server: None,
        retry: ployz_core::Retry::Never,
        reached: false,
        last: None,
    }));
    assert!(!retry_local_connect(&ConnectError::Context(
        ContextError::NoCurrentContext(PathBuf::from("config.yaml"))
    )));
}

#[tokio::test]
async fn an_unreachable_destination_reports_its_ssh_failure_not_the_local_config() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    let matches = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "--ployz-config",
            config.to_str().unwrap(),
            "server",
            "add",
            "root@127.0.0.1:1",
            "--no-install",
            "--ssh-timeout",
            "2",
        ])
        .unwrap();
    let installed = std::cell::Cell::new(false);
    let error = connect_machine(&EnrollPlan::from_matches(&matches), &|_| {
        installed.set(true);
        async { Ok(()) }
    })
    .await
    .err()
    .expect("nothing listens on port 1");
    assert!(!installed.get(), "never installs on this host");
    let causes = error.causes().join("\n");
    assert!(
        causes.contains("<USER>@127.0.0.1"),
        "names the SSH destination: {error}: {causes}"
    );
}

#[tokio::test]
async fn resumed_enrollment_founder_waits_for_published_machine() {
    assert_resumed_enrollment_waits_for_published_machine(true).await;
}

#[tokio::test]
async fn resumed_enrollment_joiner_waits_for_published_machine() {
    assert_resumed_enrollment_waits_for_published_machine(false).await;
}

#[tokio::test]
async fn readiness_refuses_another_machine_of_the_selection() {
    use crate::connect::test_support::{unix_client, unix_daemon};
    use ployz_core::{
        MachineList, MachineObservation, MembershipObservation, RpcRequestBody, RpcResponse,
    };

    let enrolled = crate::global_fixtures::machine('a', "web-3");
    let mut fallback = crate::global_fixtures::machine('b', "web-4");
    fallback.runtime.daemon_version = "fixture".into();
    let details = MachineDetails {
        devices: Vec::new(),
        removed_keys: Vec::new(),
        removed_grants: Vec::new(),
        id: fallback.id,
        phase: LocalMachinePhase::Participating,
        public_key: fallback.public_key,
        advertised_endpoints: Vec::new(),
        machine: Some(fallback.clone()),
        store_version: Default::default(),
        rtts: Vec::new(),
        passkeys: Vec::new(),
        management_clients: Vec::new(),
        telemetry: None,
        storage: None,
        upgrade: None,
        token: None,
        sealing: None,
        founding: Default::default(),
        grants: Vec::new(),
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ployz.sock");
    let server = {
        let fallback = fallback.clone();
        unix_daemon(&path, move |body| {
            let details = details.clone();
            let fallback = fallback.clone();
            async move {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "the fallback daemon answers only readiness reads"
                )]
                let response = match body {
                    RpcRequestBody::Inspect(_) => RpcResponse::from(details),
                    RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                        enrollment: None,
                        machines: vec![MachineObservation::new(
                            fallback,
                            MembershipObservation::Up,
                        )],
                    }),
                    other => panic!("unexpected RPC: {other:?}"),
                };
                Ok(response)
            }
        })
    };
    let mut client = unix_client(&path).await;
    let refused = check_enrolled(&mut client, enrolled.id, &mut machine::Joining::default()).await;
    let accepted = check_enrolled(&mut client, fallback.id, &mut machine::Joining::default()).await;
    server.shutdown_background();
    assert!(
        matches!(refused, Err(ConnectError::Attempt(_))),
        "a listed Machine other than the enrolled one is not ready: {refused:?}"
    );
    accepted.unwrap();
}

#[tokio::test]
async fn readiness_waits_through_the_enrollment_connection() {
    use crate::connect::test_support::unix_daemon;
    use ployz_core::{
        MachineList, MachineObservation, MembershipObservation, RpcRequestBody, RpcResponse,
    };

    // Two participating, listed daemons: the selection's first entry is
    // machine-b, while the enrollment reached machine-a.
    let daemon = |path: &std::path::Path, machine: Machine| {
        let details = MachineDetails {
            devices: Vec::new(),
            removed_keys: Vec::new(),
            removed_grants: Vec::new(),
            id: machine.id,
            phase: LocalMachinePhase::Participating,
            public_key: machine.public_key,
            advertised_endpoints: Vec::new(),
            machine: Some(machine.clone()),
            store_version: Default::default(),
            rtts: Vec::new(),
            passkeys: Vec::new(),
            management_clients: Vec::new(),
            telemetry: None,
            storage: None,
            upgrade: None,
            token: None,
            sealing: None,
            founding: Default::default(),
            grants: Vec::new(),
        };
        unix_daemon(path, move |body| {
            let details = details.clone();
            let machine = machine.clone();
            async move {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "the fixture answers only readiness reads"
                )]
                let response = match body {
                    RpcRequestBody::Inspect(_) => RpcResponse::from(details),
                    RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                        enrollment: None,
                        machines: vec![MachineObservation::new(machine, MembershipObservation::Up)],
                    }),
                    other => panic!("unexpected RPC: {other:?}"),
                };
                Ok(response)
            }
        })
    };
    let enrolled = crate::global_fixtures::machine('a', "machine-a");
    let earlier = crate::global_fixtures::machine('b', "machine-b");
    let root = tempfile::tempdir().unwrap();
    let enrolled_path = root.path().join("a.sock");
    let earlier_path = root.path().join("b.sock");
    let enrolled_server = daemon(&enrolled_path, enrolled.clone());
    let earlier_server = daemon(&earlier_path, earlier);
    let selection = format!("unix://{}", earlier_path.display());
    let cli = crate::cli::command()
        .try_get_matches_from(["ployz", "--connect", &selection, "server", "add", "--yes"])
        .unwrap();
    let matches = &EnrollPlan::from_matches(&cli);
    let via = Connection::unix(&enrolled_path).unwrap();
    let ready = wait_phase(
        matches,
        Awaited::Enrolled {
            id: enrolled.id,
            via: &via,
        },
        "the first Server did not become ready",
    )
    .await;
    enrolled_server.shutdown_background();
    earlier_server.shutdown_background();
    assert_eq!(ready.unwrap().connection(), &via);
}

/// A daemon that refused to start never comes back by itself, so the wait
/// names its refusal at once instead of after its full deadline (#3703).
#[tokio::test]
async fn readiness_ends_at_a_startup_refusal() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join(ployz_core::STARTUP_REFUSAL_FILE),
        "refusing to change the retained WireGuard interface\n",
    )
    .unwrap();
    let cli = crate::cli::command()
        .try_get_matches_from(["ployz", "server", "add", "--yes"])
        .unwrap();
    let via = Connection::unix(root.path().join("ployz.sock")).unwrap();
    let refused = tokio::time::timeout(
        Duration::from_secs(10),
        wait_phase(
            &EnrollPlan::from_matches(&cli),
            Awaited::Enrolled {
                id: crate::global_fixtures::machine('a', "machine-a").id,
                via: &via,
            },
            "the joined Server did not become ready",
        ),
    )
    .await
    .expect("a refusal ends the wait")
    .err()
    .expect("a refusing daemon is not ready");
    let text = error_chain::row(&refused);
    assert!(
        text.contains(
            "ployzd refused to start: refusing to change the retained WireGuard interface"
        ),
        "{text}"
    );
}

async fn assert_resumed_enrollment_waits_for_published_machine(founder: bool) {
    use crate::connect::test_support::{unix_client, unix_daemon};
    use ployz_core::{
        MachineList, MachineObservation, MembershipObservation, RpcRequestBody, RpcResponse,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let mut machine = crate::global_fixtures::machine('a', "web-3");
    machine.accepts_ingress = true;
    machine.runtime.daemon_version = "fixture".into();
    let details = MachineDetails {
        devices: Vec::new(),
        removed_keys: Vec::new(),
        removed_grants: Vec::new(),
        id: machine.id,
        phase: LocalMachinePhase::Participating,
        public_key: machine.public_key,
        advertised_endpoints: Vec::new(),
        machine: Some(machine.clone()),
        store_version: Default::default(),
        rtts: Vec::new(),
        passkeys: Vec::new(),
        management_clients: Vec::new(),
        telemetry: None,
        storage: None,
        upgrade: None,
        token: None,
        sealing: None,
        founding: Default::default(),
        grants: Vec::new(),
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ployz.sock");
    let listed = Arc::new(AtomicBool::new(false));
    let listing_observed = Arc::new(AtomicBool::new(false));
    let inspecting = Arc::new(tokio::sync::Notify::new());
    let publish = Arc::new(tokio::sync::Notify::new());
    let following = Arc::new(tokio::sync::Notify::new());
    let server = {
        let machine = machine.clone();
        let details = details.clone();
        let listed = Arc::clone(&listed);
        let listing_observed = Arc::clone(&listing_observed);
        let inspecting = Arc::clone(&inspecting);
        let publish = Arc::clone(&publish);
        let following = Arc::clone(&following);
        unix_daemon(&path, move |body| {
            let machine = machine.clone();
            let details = details.clone();
            let listed = Arc::clone(&listed);
            let listing_observed = Arc::clone(&listing_observed);
            let inspecting = Arc::clone(&inspecting);
            let publish = Arc::clone(&publish);
            let following = Arc::clone(&following);
            async move {
                let response = if matches!(body, RpcRequestBody::Inspect(_)) {
                    if !listed.load(Ordering::SeqCst) {
                        inspecting.notify_one();
                        publish.notified().await;
                    }
                    RpcResponse::from(details)
                } else if matches!(body, RpcRequestBody::ListMachines(_)) {
                    let machines = if listed.load(Ordering::SeqCst) {
                        listing_observed.store(true, Ordering::SeqCst);
                        vec![MachineObservation::new(machine, MembershipObservation::Up)]
                    } else {
                        Vec::new()
                    };
                    RpcResponse::from(MachineList {
                        enrollment: None,
                        machines,
                    })
                } else {
                    following.notify_one();
                    return std::future::pending().await;
                };
                Ok(response)
            }
        })
    };
    let mut client = unix_client(&path).await;
    assert!(client.machines().await.unwrap().is_empty());
    let connection = format!("unix://{}", path.display());
    let root = crate::cli::command()
        .try_get_matches_from(["ployz", "--connect", &connection, "server", "add", "--yes"])
        .unwrap();
    let matches = &EnrollPlan::from_matches(&root);
    let token = CloudEnrollToken::parse("pmet_resumed_enroll").unwrap();
    let pairing = CloudPairing {
        secret: cloud::enroll::PairingCredential::parse("pairing").unwrap(),
    };
    let policy = ployz_core::InitialMachinePolicy::default();
    let mut report = SetupReport::start();
    let install = |_| async { Ok(()) };
    let enrollment = Enrollment {
        plan: matches,
        token: &token,
        cloud_url: "http://unused.invalid",
        install: &install,
        progress: &|_| {},
    };
    let identified = identified(&machine, details, policy);
    let enrolling = async {
        if founder {
            let initialize = Initialize {
                mode: InitializeMode::Resume,
                pairing,
                storage: StorageChoice::None.into(),
                recovery: None,
            };
            enrollment
                .found(
                    client,
                    identified,
                    Default::default(),
                    initialize,
                    &mut report,
                )
                .await
        } else {
            let join = Join {
                pairing,
                storage: StorageChoice::None.into(),
                registration: registered(machine),
            };
            enrollment
                .join(client, identified, join, Default::default(), &mut report)
                .await
        }
    };
    tokio::pin!(enrolling);
    let result = tokio::select! {
        outcome = &mut enrolling => Err(format!("enrollment ended before publication: {outcome:?}")),
        () = inspecting.notified() => Ok(()),
        () = following.notified() => Err("enrollment follow-up ran before this Machine was listed".into()),
    };
    let result = if result.is_ok() {
        listed.store(true, Ordering::SeqCst);
        publish.notify_one();
        tokio::select! {
            outcome = &mut enrolling => Err(format!("enrollment ended before follow-up: {outcome:?}")),
            () = following.notified() => Ok(()),
        }
    } else {
        result
    };
    server.shutdown_background();
    result.unwrap();
    assert!(
        listing_observed.load(Ordering::SeqCst),
        "founder={founder}: enrollment follow-up ran before observing this Machine listed"
    );
}

#[tokio::test]
async fn unacknowledged_enroll_never_reports_committed_joining() {
    use ployz_core::{OpaquePayload, RpcRequestBody, RpcResponse};
    use std::cell::RefCell;
    use tonic::{Request, Response};

    for founder in [false, true] {
        let mut machine = crate::global_fixtures::machine('a', "web-3");
        machine.accepts_ingress = false;
        let details = MachineDetails {
            devices: Vec::new(),
            removed_keys: Vec::new(),
            removed_grants: Vec::new(),
            id: machine.id,
            phase: LocalMachinePhase::Uninitialized,
            public_key: machine.public_key,
            advertised_endpoints: Vec::new(),
            machine: None,
            store_version: Default::default(),
            rtts: Vec::new(),
            passkeys: Vec::new(),
            management_clients: Vec::new(),
            telemetry: None,
            storage: None,
            upgrade: None,
            token: None,
            sealing: None,
            founding: Default::default(),
            grants: Vec::new(),
        };
        let observed = details.clone();
        let reached = std::sync::Arc::new(tokio::sync::Notify::new());
        let received = std::sync::Arc::clone(&reached);
        let (client, server) =
            crate::connect::test_support::rpc_client(move |request: Request<OpaquePayload>| {
                let details = observed.clone();
                let received = std::sync::Arc::clone(&received);
                async move {
                    let request = request.into_inner().decode_request().unwrap().body;
                    if matches!(request, RpcRequestBody::Inspect(_)) {
                        return Ok(Response::new(RpcResponse::from(details).encode().unwrap()));
                    }
                    assert!(
                        matches!(request, RpcRequestBody::Enroll(_)),
                        "unexpected RPC: {request:?}"
                    );
                    received.notify_one();
                    std::future::pending().await
                }
            })
            .await;
        // A config of its own, so this host's contexts don't make the add remote.
        let config = tempfile::tempdir().unwrap();
        let config_path = config.path().join("config.yaml");
        let root = crate::cli::command()
            .try_get_matches_from([
                "ployz",
                "--ployz-config",
                config_path.to_str().unwrap(),
                "server",
                "add",
                "--yes",
            ])
            .unwrap();
        let matches = &EnrollPlan::from_matches(&root);
        let token = CloudEnrollToken::parse("pmet_unacknowledged_enroll").unwrap();
        let pairing = CloudPairing {
            secret: cloud::enroll::PairingCredential::parse("pairing").unwrap(),
        };
        let policy = ployz_core::InitialMachinePolicy {
            accepts_ingress: false,
            ..Default::default()
        };
        let mut report = SetupReport::start();
        let stages = RefCell::new(Vec::new());
        let progress = |stage| stages.borrow_mut().push(stage);
        let install = |_| async { Ok(()) };
        let enrollment = Enrollment {
            plan: matches,
            token: &token,
            cloud_url: "http://unused.invalid",
            install: &install,
            progress: &progress,
        };
        let identified = identified(&machine, details, policy);
        let enrolling = async {
            if founder {
                let initialize = Initialize {
                    mode: InitializeMode::New,
                    pairing,
                    storage: StorageChoice::None.into(),
                    recovery: None,
                };
                enrollment
                    .found(
                        client,
                        identified,
                        Default::default(),
                        initialize,
                        &mut report,
                    )
                    .await
            } else {
                let join = Join {
                    pairing,
                    storage: StorageChoice::None.into(),
                    registration: registered(machine),
                };
                enrollment
                    .join(client, identified, join, Default::default(), &mut report)
                    .await
            }
        };
        tokio::pin!(enrolling);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                outcome = &mut enrolling => panic!("unacknowledged enrollment finished: {outcome:?}"),
                () = reached.notified() => {}
            }
        })
        .await
        .expect("the RPC fixture receives enrollment");
        server.abort();
        assert_eq!(
            *stages.borrow(),
            vec![EnrollStage::Storage],
            "founder={founder}: an unacknowledged RPC must leave a cancellable saved stage"
        );
    }
}

#[test]
fn a_forgotten_server_rejoining_through_cloud_is_reset_first() {
    let listed = {
        let mut machine = crate::global_fixtures::machine('a', "web-3");
        machine.runtime.daemon_version = "0.3.0".into();
        machine
    };
    let registered = crate::global_fixtures::machine('a', "web-3");
    let details = |phase| MachineDetails {
        devices: Vec::new(),
        removed_keys: Vec::new(),
        removed_grants: Vec::new(),
        id: listed.id,
        phase,
        public_key: listed.public_key,
        advertised_endpoints: Vec::new(),
        machine: Some(listed.clone()),
        store_version: Default::default(),
        rtts: Vec::new(),
        passkeys: Vec::new(),
        management_clients: Vec::new(),
        telemetry: None,
        storage: None,
        upgrade: None,
        token: None,
        sealing: None,
        founding: Default::default(),
        grants: Vec::new(),
    };
    assert_eq!(
        already_assigned(&details(LocalMachinePhase::Participating), &listed),
        Some(&listed),
        "the Cluster lists the Machine as it published itself"
    );
    assert_eq!(
        already_assigned(&details(LocalMachinePhase::Participating), &registered),
        None,
        "Cloud registered it afresh, so its participation predates this enrollment"
    );
    assert_eq!(
        already_assigned(&details(LocalMachinePhase::Joining), &registered),
        Some(&listed),
        "a join under way hasn't published itself yet"
    );
    let refusal = unlisted_participation(&details(LocalMachinePhase::Participating), &registered)
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("run the same command again") && refusal.contains("--reset"),
        "an unlisted participant may be a rerun that outran its publication or a forgotten Server: {refusal}"
    );
    assert!(
        refusal.contains("make a new token with `ployz server add --command --reset`"),
        "this token now belongs to the forgotten identity, which --reset replaces: {refusal}"
    );
    assert!(
        unlisted_participation(&details(LocalMachinePhase::Uninitialized), &registered).is_ok()
    );
}

#[tokio::test]
async fn an_add_on_another_cluster_is_refused_only_with_different_observed_keys() {
    let held = |byte| ployz_core::Sealing::Held {
        public_key: ployz_core::SealingPublicKey::from_bytes([byte; 32]),
    };
    let awaits = |byte| ployz_core::Sealing::AwaitsRecovery {
        public_key: ployz_core::SealingPublicKey::from_bytes([byte; 32]),
    };
    for (local_key, peer_keys, another) in [
        (Some(held(1)), vec![Some(held(2))], true),
        (Some(awaits(1)), vec![Some(awaits(2)), None], true),
        (Some(held(1)), vec![Some(held(1))], false),
        (Some(awaits(1)), vec![Some(held(1))], false),
        (Some(held(1)), vec![Some(held(2)), Some(held(3))], false),
        (
            Some(ployz_core::Sealing::NeedsAccess),
            vec![Some(held(2))],
            false,
        ),
        (None, vec![Some(held(2))], false),
        (
            Some(held(1)),
            vec![None, Some(ployz_core::Sealing::NeedsAccess)],
            false,
        ),
    ] {
        let assigned = crate::global_fixtures::machine('a', "my-server");
        let details = MachineDetails {
            devices: Vec::new(),
            removed_keys: Vec::new(),
            removed_grants: Vec::new(),
            id: assigned.id,
            phase: LocalMachinePhase::Participating,
            machine: Some(assigned.clone()),
            public_key: assigned.public_key,
            advertised_endpoints: Vec::new(),
            store_version: Default::default(),
            rtts: Vec::new(),
            passkeys: Vec::new(),
            management_clients: Vec::new(),
            founding: Default::default(),
            telemetry: None,
            storage: None,
            upgrade: None,
            token: None,
            sealing: local_key,
            grants: Vec::new(),
        };
        let peers = peer_keys
            .into_iter()
            .zip(['b', 'c'])
            .map(|(sealing, id)| {
                let mut peer = crate::global_fixtures::machine(id, "other-server");
                peer.runtime.sealing = sealing;
                peer
            })
            .collect();
        let join = Join {
            pairing: CloudPairing {
                secret: cloud::enroll::PairingCredential::parse("ppair_fake").unwrap(),
            },
            storage: StorageChoice::None.into(),
            registration: ployz_core::Registered {
                assigned_machine: assigned,
                visible_peers: peers,
                target_versions: Default::default(),
                devices: Vec::new(),
                sealing_key: Some(ployz_core::SealedSealingKey::from_bytes(vec![7; 80])),
                grants: Vec::new(),
                restrictions: None,
            },
        };
        let identified = identified(
            &join.registration.assigned_machine,
            details,
            Default::default(),
        );
        let (client, daemon) = crate::connect::test_support::rpc_client(|_| async {
            panic!("a refused add sends no daemon RPC");
        })
        .await;
        // A config of its own, so this host's contexts don't make the add remote.
        let config = tempfile::tempdir().unwrap();
        let config_path = config.path().join("config.yaml");
        let root = crate::cli::command()
            .try_get_matches_from([
                "ployz",
                "--ployz-config",
                config_path.to_str().unwrap(),
                "server",
                "add",
                "--yes",
            ])
            .unwrap();
        let plan = EnrollPlan::from_matches(&root);
        let token = CloudEnrollToken::parse("pmet_fake").unwrap();
        let error = Enrollment {
            plan: &plan,
            token: &token,
            cloud_url: "http://127.0.0.1:9",
            install: &|_| async { Ok(()) },
            progress: &|_| {},
        }
        .join(
            client,
            identified,
            join,
            Default::default(),
            &mut SetupReport::start(),
        )
        .await
        .unwrap_err()
        .to_string();
        if another {
            assert!(error.contains("belongs to another Cluster"), "{error}");
            assert!(error.contains("new token"), "{error}");
            assert!(!error.contains("same command"), "{error}");
        } else {
            assert!(error.contains("already participates"), "{error}");
        }
        daemon.abort();
    }
}

#[test]
fn a_rerun_resumes_a_join_already_under_way_for_the_assigned_machine() {
    let mut assigned = crate::global_fixtures::machine('a', "web-3");
    assigned.runtime.daemon_version = "0.3.0".into();
    let other = crate::global_fixtures::machine('b', "web-4");
    let details = |phase, machine: Option<&Machine>| MachineDetails {
        devices: Vec::new(),
        removed_keys: Vec::new(),
        removed_grants: Vec::new(),
        id: assigned.id,
        phase,
        public_key: assigned.public_key,
        advertised_endpoints: Vec::new(),
        machine: machine.cloned(),
        store_version: Default::default(),
        rtts: Vec::new(),
        passkeys: Vec::new(),
        management_clients: Vec::new(),
        telemetry: None,
        storage: None,
        upgrade: None,
        token: None,
        sealing: None,
        founding: Default::default(),
        grants: Vec::new(),
    };
    for phase in [LocalMachinePhase::Joining, LocalMachinePhase::Participating] {
        assert_eq!(
            already_assigned(&details(phase.clone(), Some(&assigned)), &assigned),
            Some(&assigned),
            "{phase:?}"
        );
        assert_eq!(
            already_assigned(&details(phase, Some(&other)), &assigned),
            None
        );
    }
    assert_eq!(
        already_assigned(&details(LocalMachinePhase::Uninitialized, None), &assigned),
        None
    );
    assert_eq!(
        already_assigned(
            &details(LocalMachinePhase::Resetting, Some(&assigned)),
            &assigned
        ),
        None
    );
}
