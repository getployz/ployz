use super::*;
use ployz_core::{
    AnswerWhen, ContractDescription, ImageIngestDestination, ImageIngestOpened, ImageSummary,
    Machine, MachineImages, MachineList, MachineName, MachineObservation, OpaquePayload,
    PROTOCOL_MAJOR, RpcRequestBody, RpcResponse, RunRotation, ServiceMode, WireGuardPublicKey,
};
use tonic::{Request, Response, Status};

#[derive(Clone, Copy, PartialEq)]
enum Silent {
    Ingest,
    PeerPull,
    RegistryPull,
    ClusterDown,
    NeverSettles,
}

fn holder(hex: char) -> Machine {
    Machine {
        id: machine(hex),
        name: MachineName::parse(format!("machine-{hex}")).unwrap(),
        subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
            .parse()
            .unwrap(),
        public_key: WireGuardPublicKey([hex as u8; 32]),
        public_ip: None,
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        advertised_endpoints: Vec::new(),
        runtime: ployz_core::MachineRuntime {
            architecture: "x86_64".into(),
            ..Default::default()
        },
        build_concurrency: None,
    }
}

fn images(image: &str, held: bool) -> MachineImages {
    MachineImages {
        containerd_store: true,
        images: held
            .then(|| ImageSummary {
                id: format!("sha256:{}", "4".repeat(64)),
                repo_tags: vec![image.into()],
                created: 0,
                size: 1,
                containers: 0,
                platforms: vec!["linux/amd64".into()],
                last_tagged: None,
            })
            .into_iter()
            .collect(),
        docker_root: None,
    }
}

async fn destination_and_holder(
    image: &'static str,
    silent: Silent,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let powered_off = || {
        ployz_core::rpc::target_unanswered_status(&std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "deadline has elapsed",
        ))
    };
    crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| async move {
        let target = rpc
            .metadata()
            .get("machine")
            .map(|value| MachineId::parse(value.to_str().unwrap()).unwrap());
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "the fixture answers only what an image pull and a start send"
        )]
        let response = match rpc.into_inner().decode_request().unwrap().body {
            RpcRequestBody::DescribeContract(_) => RpcResponse::from(ContractDescription {
                machine_id: machine('1'),
                protocol_major: PROTOCOL_MAJOR,
                daemon_version: "fixture".into(),
                capabilities: Default::default(),
            }),
            RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                enrollment: None,
                machines: ['1', '2']
                    .map(|hex| MachineObservation::new(holder(hex), MembershipObservation::Up))
                    .into(),
            }),
            RpcRequestBody::ListImages(_) => {
                RpcResponse::from(images(image, target == Some(machine('2'))))
            }
            RpcRequestBody::EnsureImageIngest(_) if silent == Silent::Ingest => {
                return Err(powered_off());
            }
            RpcRequestBody::EnsureImageIngest(_) => RpcResponse::from(ImageIngestOpened {
                destination: ImageIngestDestination {
                    management_address: holder('2').management_address(),
                    port: ployz_core::UNREGISTRY_PORT,
                },
            }),
            RpcRequestBody::PullImageFromMachine(_) if silent == Silent::PeerPull => {
                return Err(powered_off());
            }
            RpcRequestBody::PullImage(_) if silent == Silent::RegistryPull => {
                return Err(powered_off());
            }
            RpcRequestBody::PullImage(_) if silent == Silent::ClusterDown => {
                return Err(Status::unavailable("Cluster is not available"));
            }
            RpcRequestBody::ListContainers(_) | RpcRequestBody::EnsureContainer(_)
                if silent == Silent::NeverSettles =>
            {
                return std::future::pending().await;
            }
            other => panic!("unexpected request: {other:?}"),
        };
        Ok(Response::new(response.encode().unwrap()))
    })
    .await
}

fn silence(machine_id: MachineId, transport: &str) -> String {
    format!("Machine {machine_id} did not answer: {transport}")
}

#[tokio::test(start_paused = true)]
async fn a_peer_that_cannot_open_ingest_is_named() {
    let image = "alpine:3.23.3";
    let (client, server) = destination_and_holder(image, Silent::Ingest).await;

    let error = crate::image::ensure_cluster_image(
        &client,
        &machine('1'),
        image,
        ployz_core::PullPolicy::Missing,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, silence(machine('2'), "deadline has elapsed"));
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_destination_silent_during_a_peer_pull_is_named() {
    let image = "alpine:3.23.3";
    let (client, server) = destination_and_holder(image, Silent::PeerPull).await;

    let error = crate::image::ensure_cluster_image(
        &client,
        &machine('1'),
        image,
        ployz_core::PullPolicy::Missing,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, silence(machine('1'), "deadline has elapsed"));
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_destination_silent_during_its_registry_pull_is_named() {
    let (client, server) = destination_and_holder("alpine:3.23.3", Silent::RegistryPull).await;
    let mut service = spec(None, None, None);
    service.container.pull_policy = ployz_core::PullPolicy::Always;

    let error = MachineOperations::ensure_image(&client, &machine('1'), &service)
        .await
        .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, silence(machine('1'), "deadline has elapsed"));
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_destination_that_answers_unavailable_keeps_its_words() {
    let (client, server) = destination_and_holder("alpine:3.23.3", Silent::ClusterDown).await;
    let mut service = spec(None, None, None);
    service.container.pull_policy = ployz_core::PullPolicy::Always;

    let error = MachineOperations::ensure_image(&client, &machine('1'), &service)
        .await
        .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "Cluster is not available");
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_never_settles_a_container_is_named() {
    let (client, server) = destination_and_holder("alpine:3.23.3", Silent::NeverSettles).await;

    let error = MachineOperations::ensure_container(
        &client,
        &machine('1'),
        ContainerWant::Running {
            container: RunTarget::Existing(ContainerLocator::Id(container('a'))),
            rotation: RunRotation::Pending,
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        silence(machine('1'), "target Machine RPC timed out")
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_machine_silent_while_a_global_start_checks_its_slot_is_named() {
    let (client, server) = destination_and_holder("alpine:3.23.3", Silent::NeverSettles).await;
    let mut global = spec(None, None, None);
    global.mode = ServiceMode::Global;

    let error = MachineOperations::ensure_container(
        &client,
        &machine('1'),
        ContainerWant::Running {
            container: RunTarget::New(NewContainer {
                r#gen: ployz_core::Gen::FIRST,
                slot: slot_key(
                    &test_namespace(),
                    &global.name,
                    ContainerKind::ServiceContainer,
                    crate::cluster::global_creation_key(&global),
                ),
                resolved_spec: global,
                deployment_id: None,
                registry_auth: None,
                image_prepared: true,
                answer: AnswerWhen::Started,
            }),
            rotation: RunRotation::Pending,
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        silence(machine('1'), "target Machine RPC timed out")
    );
    server.abort();
}

/// Route reads that still count Machine '2' as live before its membership reads Down.
const SUSPECT_READS: usize = 3;

/// The gen of the copy the entry Machine's store keeps on Machine '2'.
const STORED_GEN: u64 = 5;

/// How Machine '2' answers its Container listing.
#[derive(Clone, Copy)]
enum Lists {
    Never,
    WithError,
    /// The entry's router couldn't dial it.
    Undialed,
}

/// Machine '1' is Up and lists `listed`; Machine '2' is `membership` and answers its
/// listing as `lists`. Its proxy row stays stale until the membership sample reads it
/// Down.
#[derive(Clone)]
struct Pair {
    membership: MembershipObservation,
    lists: Lists,
    /// Whether the entry Machine's store answers. It keeps a copy at [`STORED_GEN`] on
    /// Machine '2'.
    store: bool,
    listed: Vec<ContainerObservation>,
    /// Whether the entry's path probes find large packets to Machine '2' dropped.
    dropped: bool,
}

impl Pair {
    fn suspect_silent() -> Self {
        Self {
            membership: MembershipObservation::Suspect,
            lists: Lists::Never,
            store: true,
            listed: Vec::new(),
            dropped: false,
        }
    }

    /// Machine '2' is Up, but the entry finds large packets to it dropped, so it never
    /// lists.
    fn up_dropped() -> Self {
        Self {
            membership: MembershipObservation::Up,
            dropped: true,
            ..Self::suspect_silent()
        }
    }
}

/// Each `EnsureContainer` that reaches Machine '1' is answered with the copy it asked
/// for, and recorded in `ensured` with its gen.
async fn live_and_second(
    ensured: std::sync::Arc<std::sync::Mutex<Vec<(MachineId, Gen)>>>,
    pair: Pair,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let copy = std::sync::Arc::new(std::sync::Mutex::new(None));
    let reads = std::sync::Arc::new(std::sync::Mutex::new(0));
    crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
        let (ensured, copy, reads, pair) =
            (ensured.clone(), copy.clone(), reads.clone(), pair.clone());
        async move {
            let target = rpc
                .metadata()
                .get("machine")
                .map(|value| MachineId::parse(value.to_str().unwrap()).unwrap());
            #[expect(
                clippy::wildcard_enum_match_arm,
                reason = "the fixture answers only what a run on the live Machine sends"
            )]
            let response = match rpc.into_inner().decode_request().unwrap().body {
                RpcRequestBody::DescribeContract(_) => RpcResponse::from(ContractDescription {
                    machine_id: machine('1'),
                    protocol_major: PROTOCOL_MAJOR,
                    daemon_version: "fixture".into(),
                    capabilities: Default::default(),
                }),
                RpcRequestBody::ListMachines(_) => {
                    let mut second = MachineObservation::new(holder('2'), pair.membership);
                    second.path = pair.dropped.then_some(ployz_core::PathObservation {
                        mtu: 1420,
                        large_packets: ployz_core::LargePackets::Dropped,
                        lowered: false,
                    });
                    RpcResponse::from(MachineList {
                        enrollment: None,
                        machines: vec![
                            MachineObservation::new(holder('1'), MembershipObservation::Up),
                            second,
                        ],
                    })
                }
                RpcRequestBody::ListImages(_) => RpcResponse::from(images("alpine:3.23.3", true)),
                RpcRequestBody::PullImage(_) => {
                    RpcResponse::from(ployz_core::ImagePulled::default())
                }
                RpcRequestBody::ListContainers(_) if target == Some(machine('2')) => {
                    match pair.lists {
                        Lists::WithError => {
                            return Err(Status::internal("Docker is not running"));
                        }
                        Lists::Never => return std::future::pending().await,
                        Lists::Undialed => {
                            return Err(ployz_core::rpc::target_unreached_status(
                                &std::io::Error::new(
                                    std::io::ErrorKind::ConnectionRefused,
                                    "tcp connect error",
                                ),
                            ));
                        }
                    }
                }
                RpcRequestBody::ListContainers(_) => RpcResponse::from(ployz_core::ContainerList {
                    containers: pair.listed,
                }),
                RpcRequestBody::ReadRoutes(_) => {
                    let read = {
                        let mut reads = reads.lock().unwrap();
                        *reads += 1;
                        *reads
                    };
                    let acked = copy.lock().unwrap().is_some().then(|| container('a'));
                    let (stale, down) = if read <= SUSPECT_READS {
                        (Some(Some(BTreeSet::new())), BTreeSet::new())
                    } else {
                        (None, BTreeSet::from([machine('2')]))
                    };
                    RpcResponse::from(RoutesRead {
                        routes: [(machine('1'), Some(acked.into_iter().collect()))]
                            .into_iter()
                            .chain(stale.map(|stale| (machine('2'), stale)))
                            .collect(),
                        complete: true,
                        down,
                    })
                }
                RpcRequestBody::RuntimeWatch(_) => {
                    if !pair.store {
                        return Err(Status::unavailable("the store is not ready"));
                    }
                    let mut stored = routed(&machine('2'), &container('b'), running());
                    stored
                        .try_update(|parts| parts.r#gen = Some(Gen::new(STORED_GEN)))
                        .unwrap();
                    let frame = ployz_core::RuntimeWatchFrame {
                        machines: Vec::new(),
                        containers: vec![stored],
                        volumes: Vec::new(),
                        certificates: Vec::new(),
                        incomplete_ids: Default::default(),
                        observed_at: String::new(),
                    };
                    return Ok(Response::new(
                        ployz_core::encode_runtime_watch_frame(&frame).unwrap(),
                    ));
                }
                RpcRequestBody::EnsureContainer(request) => {
                    let mut copy = copy.lock().unwrap();
                    let (spec, r#gen, rotation) = match (request.want, copy.take()) {
                        (
                            ContainerWant::Running {
                                container: RunTarget::New(new),
                                rotation: RunRotation::Pending,
                            },
                            None,
                        ) => (
                            new.resolved_spec,
                            new.r#gen,
                            ployz_core::CopyRotation::Pending,
                        ),
                        (
                            ContainerWant::Running {
                                container: RunTarget::Existing(_),
                                rotation: RunRotation::In { .. },
                            },
                            Some((spec, r#gen)),
                        ) => (spec, r#gen, ployz_core::CopyRotation::In),
                        (want, _) => panic!("unexpected want: {want:?}"),
                    };
                    ensured.lock().unwrap().push((target.unwrap(), r#gen));
                    *copy = Some((spec.clone(), r#gen));
                    let now =
                        ContainerObservation::try_from(ployz_core::ContainerObservationParts {
                            rotation: Some(rotation),
                            r#gen: Some(r#gen),
                            container_id: container('a'),
                            display_name: "api".into(),
                            created_at_unix_nanos: 0,
                            machine_id: target.unwrap(),
                            namespace: test_namespace(),
                            kind: ContainerKind::ServiceContainer,
                            runtime: running(),
                            effective_healthcheck: None,
                            resolved_spec: spec,
                            address: None,
                            labels: Default::default(),
                            stopped: false,
                            run: None,
                        })
                        .unwrap();
                    RpcResponse::from(ployz_core::EnsuredContainer::Ensured {
                        now: ployz_core::ContainerNow::Present(now),
                        health: None,
                    })
                }
                other => panic!("unexpected request: {other:?}"),
            };
            Ok(Response::new(response.encode().unwrap()))
        }
    })
    .await
}

/// Run `plan` with the plan's Machine names, keeping the warnings it emits.
async fn execute_named(
    plan: Vec<DeployOperation>,
    client: &Client,
) -> (DeployOutcome<ExecutionError>, Vec<DeployWarning>) {
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let outcome = super::super::execute_operation_sequence(
        &crate::deploy::DeployPlan::for_execution_test(plan, test_namespace())
            .with_machine_names(&[machine('1'), machine('2')]),
        &run_id(),
        client,
        &CancellationToken::new(),
        Some(tx),
    )
    .await;
    let mut warnings = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let DeployEvent::Warning { warning } = event {
            warnings.push(warning);
        }
    }
    (outcome, warnings)
}

fn failed_listing(outcome: &DeployOutcome<ExecutionError>) -> Option<&RpcError> {
    match outcome {
        DeployOutcome::Failed {
            failed:
                FailedOperation::Operation {
                    error:
                        ExecutionError::Machine {
                            action: MachineAction::ListContainers,
                            error,
                        },
                    ..
                },
            ..
        } => Some(error),
        DeployOutcome::Success { .. }
        | DeployOutcome::Failed { .. }
        | DeployOutcome::Interrupted { .. } => None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_suspect_machine_that_never_lists_does_not_stop_a_run_on_a_live_one() {
    let ensured = std::sync::Arc::default();
    let (client, server) =
        live_and_second(std::sync::Arc::clone(&ensured), Pair::suspect_silent()).await;
    let plan = vec![run(&machine('1'), spec(None, None, None), true)];

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let above = Gen::new(STORED_GEN + 1);
    assert_eq!(
        *ensured.lock().unwrap(),
        [(machine('1'), above), (machine('1'), above)],
        "the copy starts and goes In on the live Machine only, above the silent Machine's stored copy"
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_machine_silent_at_run_time_is_named_as_an_observation_gap() {
    let (client, server) = live_and_second(std::sync::Arc::default(), Pair::suspect_silent()).await;
    let plan = vec![run(&machine('1'), spec(None, None, None), true)];

    let (outcome, warnings) = execute_named(plan, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let gaps: Vec<_> = warnings
        .iter()
        .filter_map(DeployWarning::observation_gap)
        .map(|(machine_id, gap)| (machine_id, gap.machine_name.to_string(), gap.reason))
        .collect();
    assert_eq!(
        gaps,
        [(
            machine('2'),
            "machine-2".to_owned(),
            ployz_core::ObservationGapReason::Failed
        )]
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_suspect_machine_that_never_lists_fails_the_service_when_the_store_is_unread() {
    let ensured = std::sync::Arc::default();
    let (client, server) = live_and_second(
        std::sync::Arc::clone(&ensured),
        Pair {
            store: false,
            ..Pair::suspect_silent()
        },
    )
    .await;
    let plan = vec![run(&machine('1'), spec(None, None, None), true)];

    let (outcome, _) = execute_named(plan, &client).await;

    let error = failed_listing(&outcome).unwrap_or_else(|| panic!("{outcome:?}"));
    assert_eq!(
        error.message,
        "Machine 'machine-2' did not answer and the entry Machine's store could not be read, \
         so a new copy might not replace that Machine's copies"
    );
    assert!(ensured.lock().unwrap().is_empty(), "no copy starts");
    server.abort();
}

/// `error` names Machine '2' in its deepest line, the one a person sees, and in its
/// details.
fn names_the_second(error: &RpcError) {
    assert!(error.target_unanswered(), "{error:?}");
    assert_eq!(
        error.cause.last().unwrap_or(&error.message),
        "Machine machine-2 did not answer: target Machine RPC timed out"
    );
    assert_eq!(
        error.details.get("machine_id"),
        Some(&serde_json::json!(machine('2')))
    );
}

#[tokio::test(start_paused = true)]
async fn an_up_machine_that_never_lists_fails_the_service_by_name() {
    let ensured = std::sync::Arc::default();
    let (client, server) = live_and_second(
        std::sync::Arc::clone(&ensured),
        Pair {
            membership: MembershipObservation::Up,
            ..Pair::suspect_silent()
        },
    )
    .await;
    let plan = vec![run(&machine('1'), spec(None, None, None), true)];

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    let error = failed_listing(&outcome).unwrap_or_else(|| panic!("{outcome:?}"));
    names_the_second(error);
    assert!(ensured.lock().unwrap().is_empty(), "no copy starts");
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn an_up_machine_whose_large_packets_are_dropped_does_not_stop_a_run_on_a_live_one() {
    let ensured = std::sync::Arc::default();
    let (client, server) =
        live_and_second(std::sync::Arc::clone(&ensured), Pair::up_dropped()).await;
    let plan = vec![run(&machine('1'), spec(None, None, None), true)];

    let (outcome, warnings) = execute_named(plan, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let above = Gen::new(STORED_GEN + 1);
    assert_eq!(
        *ensured.lock().unwrap(),
        [(machine('1'), above), (machine('1'), above)],
        "the copy starts on the live Machine only, above the silent Machine's stored copy"
    );
    let gaps: Vec<_> = warnings
        .iter()
        .filter_map(DeployWarning::observation_gap)
        .map(|(machine_id, gap)| (machine_id, gap.reason))
        .collect();
    assert_eq!(
        gaps,
        [(machine('2'), ployz_core::ObservationGapReason::Failed)]
    );
    assert!(
        warnings.iter().any(|warning| matches!(warning,
            DeployWarning::ObservationFailed { message, .. }
                if message.contains("large packets to machine-2 are dropped"))),
        "{warnings:?}"
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn an_up_machine_whose_large_packets_are_dropped_still_needs_the_store_floor() {
    let ensured = std::sync::Arc::default();
    let (client, server) = live_and_second(
        std::sync::Arc::clone(&ensured),
        Pair {
            store: false,
            ..Pair::up_dropped()
        },
    )
    .await;
    let plan = vec![run(&machine('1'), spec(None, None, None), true)];

    let (outcome, _) = execute_named(plan, &client).await;

    assert!(failed_listing(&outcome).is_some(), "{outcome:?}");
    assert!(ensured.lock().unwrap().is_empty(), "no copy starts");
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_went_silent_at_run_time_is_never_a_destination() {
    let ensured = std::sync::Arc::default();
    let (client, server) =
        live_and_second(std::sync::Arc::clone(&ensured), Pair::up_dropped()).await;
    // Planning saw Machine '2' answer, so it placed a copy there.
    let plan = vec![run(&machine('2'), spec(None, None, None), true)];

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    let error = failed_listing(&outcome).unwrap_or_else(|| panic!("{outcome:?}"));
    names_the_second(error);
    assert!(ensured.lock().unwrap().is_empty(), "nothing is sent to it");
    server.abort();
}

/// Machine '2' is Up, but the entry's router couldn't dial it, as while its daemon is
/// stopped.
fn up_undialed() -> Pair {
    Pair {
        membership: MembershipObservation::Up,
        lists: Lists::Undialed,
        ..Pair::suspect_silent()
    }
}

#[tokio::test(start_paused = true)]
async fn an_up_machine_the_entry_couldnt_dial_does_not_stop_a_run_on_a_live_one() {
    let ensured = std::sync::Arc::default();
    let (client, server) = live_and_second(std::sync::Arc::clone(&ensured), up_undialed()).await;
    let plan = vec![run(&machine('1'), spec(None, None, None), true)];

    let (outcome, warnings) = execute_named(plan, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let above = Gen::new(STORED_GEN + 1);
    assert_eq!(
        *ensured.lock().unwrap(),
        [(machine('1'), above), (machine('1'), above)],
        "the copy starts on the live Machine only, above the silent Machine's stored copy"
    );
    let gaps: Vec<_> = warnings
        .iter()
        .filter_map(DeployWarning::observation_gap)
        .map(|(machine_id, gap)| (machine_id, gap.reason))
        .collect();
    assert_eq!(
        gaps,
        [(machine('2'), ployz_core::ObservationGapReason::Failed)]
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn an_up_machine_the_entry_couldnt_dial_still_needs_the_store_floor() {
    let ensured = std::sync::Arc::default();
    let (client, server) = live_and_second(
        std::sync::Arc::clone(&ensured),
        Pair {
            store: false,
            ..up_undialed()
        },
    )
    .await;
    let plan = vec![run(&machine('1'), spec(None, None, None), true)];

    let (outcome, _) = execute_named(plan, &client).await;

    assert!(failed_listing(&outcome).is_some(), "{outcome:?}");
    assert!(ensured.lock().unwrap().is_empty(), "no copy starts");
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_suspect_machine_that_answers_its_listing_with_an_error_fails_the_service() {
    let ensured = std::sync::Arc::default();
    let (client, server) = live_and_second(
        std::sync::Arc::clone(&ensured),
        Pair {
            lists: Lists::WithError,
            ..Pair::suspect_silent()
        },
    )
    .await;
    let plan = vec![run(&machine('1'), spec(None, None, None), true)];

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    let error = failed_listing(&outcome).unwrap_or_else(|| panic!("{outcome:?}"));
    assert_eq!(error.message, "Docker is not running");
    assert!(ensured.lock().unwrap().is_empty());
    server.abort();
}

/// A dependency check that waits on `app/api`, healthy and In on Machine '1'.
fn dependency_check(membership: MembershipObservation) -> (Pair, Vec<DeployOperation>) {
    let pair = Pair {
        membership,
        listed: vec![routed(&machine('1'), &container('d'), healthy())],
        ..Pair::suspect_silent()
    };
    let plan = vec![DeployOperation::WaitHealthy {
        machine_id: machine('1'),
        dependent: QualifiedService::parse("app/web").unwrap(),
        dependency: QualifiedService::parse("app/api").unwrap(),
    }];
    (pair, plan)
}

#[tokio::test(start_paused = true)]
async fn a_dependency_check_skips_a_suspect_machine_that_never_lists() {
    let (pair, plan) = dependency_check(MembershipObservation::Suspect);
    let (client, server) = live_and_second(std::sync::Arc::default(), pair).await;

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_dependency_check_names_a_suspect_machine_that_never_lists_as_an_observation_gap() {
    let (pair, plan) = dependency_check(MembershipObservation::Suspect);
    let (client, server) = live_and_second(std::sync::Arc::default(), pair).await;

    let (outcome, warnings) = execute_named(plan, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let gaps: Vec<_> = warnings
        .iter()
        .filter_map(DeployWarning::observation_gap)
        .map(|(machine_id, gap)| (machine_id, gap.machine_name.to_string(), gap.reason))
        .collect();
    assert_eq!(
        gaps,
        [(
            machine('2'),
            "machine-2".to_owned(),
            ployz_core::ObservationGapReason::Failed
        )]
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_dependency_check_names_an_up_machine_the_entry_couldnt_dial_as_an_observation_gap() {
    let (pair, plan) = dependency_check(MembershipObservation::Up);
    let pair = Pair {
        lists: Lists::Undialed,
        ..pair
    };
    let (client, server) = live_and_second(std::sync::Arc::default(), pair).await;

    let (outcome, warnings) = execute_named(plan, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let gaps: Vec<_> = warnings
        .iter()
        .filter_map(DeployWarning::observation_gap)
        .map(|(machine_id, gap)| (machine_id, gap.reason))
        .collect();
    assert_eq!(
        gaps,
        [(machine('2'), ployz_core::ObservationGapReason::Failed)]
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_dependency_check_fails_by_name_on_an_up_machine_that_never_lists() {
    let (pair, plan) = dependency_check(MembershipObservation::Up);
    let (client, server) = live_and_second(std::sync::Arc::default(), pair).await;

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    let DeployOutcome::Failed {
        failed:
            FailedOperation::Operation {
                error:
                    ExecutionError::DependencyHealth {
                        failure: DependencyHealthFailure::Observation { error },
                        ..
                    },
                ..
            },
        ..
    } = &outcome
    else {
        panic!("{outcome:?}");
    };
    names_the_second(error);
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_full_listing_names_a_suspect_machine_that_never_answers() {
    let (client, server) = live_and_second(std::sync::Arc::default(), Pair::suspect_silent()).await;

    let error = MachineOperations::service_containers(
        &client,
        &QualifiedService::parse("app/web").unwrap(),
    )
    .await
    .expect_err("a silent Machine leaves the full listing incomplete");

    names_the_second(&error);
    server.abort();
}
