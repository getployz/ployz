//! Tests for bounded Global catch-up.

use ployz_core::error_chain;

use ployz_core::{
    Ciphertext, ContainerObservation, ContainerRuntimeObservation, MachineId, Placement,
    PrivateAccessSet, ResolvedUpdateConfig, ServiceMode, ServiceObservation,
};

use super::*;
use crate::enrollment::catch_up::joined_catch_up_error;
use crate::global_fixtures::{
    global_service, grouped, machine, qualified, requested, running_on, service_id,
};
use ployz_core::Hint;

#[tokio::test(start_paused = true)]
async fn partial_observations_reject_catch_up_before_any_placement() {
    let joiner = machine('1', "joiner");
    let peer = machine('f', "peer");
    let unlisted = machine('e', "unlisted");
    for failed in [true, false] {
        let mut client = fake(&joiner, Vec::new());
        client.names = ServerNames::listing([
            (joiner.id, joiner.name.clone()),
            (peer.id, peer.name.clone()),
        ]);
        if failed {
            client.failures = vec![ployz_core::MachineFailure {
                machine_id: peer.id,
                error: RpcError::unavailable("peer unavailable"),
            }];
        } else {
            client.omissions = vec![unlisted.id];
        }
        let error = catch_up(&mut client, &joiner).await.unwrap_err();
        let message = message(error, &joiner);
        assert!(
            message.contains("partial Service observations"),
            "{message}"
        );
        if failed {
            assert!(message.contains("peer: peer unavailable"), "{message}");
            assert!(!message.contains(peer.id.as_str()), "{message}");
        } else {
            // A Machine the membership does not list keeps its id.
            assert!(
                message.contains(&format!("{}: no terminal response", unlisted.id)),
                "{message}"
            );
        }
        assert!(client.door_calls.is_empty());
        assert_eq!(
            client.listings, OBSERVATION_ATTEMPTS,
            "a partial listing is retried within the run before it fails"
        );
    }
}

/// A newer Server's answer this ployz can't decode, as a Sealed `EnvValue` is to
/// ployz 0.3.3, asks for an update here rather than blaming connectivity.
#[tokio::test(start_paused = true)]
async fn unreadable_answer_asks_to_update_ployz_not_connectivity() {
    let joiner = machine('1', "joiner");
    let newer = machine('f', "newer");
    let down = machine('e', "down");
    let decode = serde_json::from_str::<String>(r#"{"sealed":"..."}"#).unwrap_err();
    let unreadable = crate::connect::rpc_error(crate::connect::ConnectError::Codec(
        ployz_core::CodecError::DecodeJson(decode),
    ));
    for omitted in [false, true] {
        let mut client = fake(&joiner, Vec::new());
        client.names =
            ServerNames::listing([(newer.id, newer.name.clone()), (down.id, down.name.clone())]);
        client.failures = vec![ployz_core::MachineFailure {
            machine_id: newer.id,
            error: unreadable.clone(),
        }];
        if omitted {
            client.omissions = vec![down.id];
        }
        let message = message(catch_up(&mut client, &joiner).await.unwrap_err(), &joiner);
        assert!(
            message.contains("this ployz can't read newer's answer, so update ployz on this host"),
            "{message}"
        );
        assert_eq!(
            message.contains("restore peer connectivity"),
            omitted,
            "only an unanswering Server asks for connectivity: {message}"
        );
        assert!(client.door_calls.is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn only_omissions_that_are_not_unarrived_block_catch_up() {
    let joiner = machine('1', "joiner");
    let peer = machine('f', "peer");
    let claimed = machine('e', "claimed");
    let mut client = fake(&joiner, Vec::new());
    client.names = ServerNames::listing([
        (peer.id, peer.name.clone()),
        (claimed.id, claimed.name.clone()),
    ]);
    client.unarrived = ServerNames::listing([(claimed.id, claimed.name.clone())]);
    client.omissions = vec![claimed.id];
    catch_up(&mut client, &joiner).await.unwrap();

    client.omissions.push(peer.id);
    let message = message(catch_up(&mut client, &joiner).await.unwrap_err(), &joiner);
    assert!(message.contains("peer: no terminal response"), "{message}");
    assert!(!message.contains("claimed"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn the_ingress_proxy_is_set_once_through_its_doors() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let caddy = global_service(
        QualifiedService::system_ingress(),
        'a',
        Placement::default(),
        running_on(&founder, 'a'),
    );
    let holder = grouped(
        QualifiedService::system_ingress_holder(),
        ployz_core::holder_service_spec("pause:1", ployz_core::IngressPorts::Public)
            .to_resolved(service_id('b'), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped"),
        running_on(&founder, 'b'),
    );
    let mut client = fake(&joiner, vec![caddy, holder]);

    catch_up(&mut client, &joiner).await.unwrap();

    assert_eq!(
        client.door_calls,
        [(service_id('a'), ClusterDoors::public())],
        "one call, for the observed Caddy, covers both doors"
    );
}

// Catches a joiner with the ingress role going without an Ingress Proxy because no Server
// runs one yet.
#[tokio::test(start_paused = true)]
async fn a_joiner_with_the_role_gets_doors_when_no_server_runs_them() {
    let joiner = machine('1', "joiner");
    let mut client = fake(&joiner, Vec::new());

    catch_up(&mut client, &joiner).await.unwrap();

    assert_eq!(client.door_calls.len(), 1);

    let mut without_role = machine('2', "plain");
    without_role.accepts_ingress = false;
    let mut client = fake(&without_role, Vec::new());
    catch_up(&mut client, &without_role).await.unwrap();
    assert!(client.door_calls.is_empty());
}

fn cluster_on_the_tunnel(founder: &Machine) -> Vec<ServiceObservation> {
    let sealed = ployz_core::Ciphertext::from_bytes(vec![9; 48]);
    let spec = ployz_core::tunnel_service_spec("cloudflared:1", &sealed)
        .to_resolved(service_id('b'), ResolvedUpdateConfig::default())
        .expect("volume graph is scoped");
    vec![
        global_service(
            QualifiedService::system_ingress(),
            'a',
            Placement::default(),
            running_on(founder, 'a'),
        ),
        grouped(
            QualifiedService::system_tunnel(),
            spec,
            running_on(founder, 'c'),
        ),
    ]
}

// Catches catch-up sending the Tunnel Door to a joiner whose daemon predates it, which would
// answer UnsupportedDoors instead of the client naming the capability first.
#[tokio::test(start_paused = true)]
async fn a_joiner_whose_daemon_predates_the_tunnel_door_is_named_and_not_asked() {
    let founder = machine('f', "founder");
    let mut joiner = machine('1', "joiner");
    joiner.runtime.capabilities = [ployz_core::SET_INGRESS_DOORS_CAPABILITY.to_owned()].into();
    let mut client = fake(&joiner, cluster_on_the_tunnel(&founder));

    let error = catch_up(&mut client, &joiner).await.unwrap_err();

    assert!(client.door_calls.is_empty());
    assert!(
        message(error, &joiner).contains(ployz_core::SET_INGRESS_TUNNEL_CAPABILITY),
        "the refusal names the capability the joiner lacks"
    );
}

// Catches the capability check refusing a joiner that can open the Tunnel Door, or one whose
// row predates capabilities and so claims nothing.
#[tokio::test(start_paused = true)]
async fn a_joiner_that_can_open_the_tunnel_door_is_asked() {
    let founder = machine('f', "founder");
    for capabilities in [
        vec![
            ployz_core::SET_INGRESS_DOORS_CAPABILITY.to_owned(),
            ployz_core::SET_INGRESS_TUNNEL_CAPABILITY.to_owned(),
        ],
        Vec::new(),
    ] {
        let mut joiner = machine('1', "joiner");
        joiner.runtime.capabilities = capabilities.into_iter().collect();
        let mut client = fake(&joiner, cluster_on_the_tunnel(&founder));

        catch_up(&mut client, &joiner).await.unwrap();

        let [(_, asked)] = client.door_calls.as_slice() else {
            panic!("one call sets the doors: {:?}", client.door_calls);
        };
        assert!(asked.doors.has(ployz_core::IngressDoor::Tunnel));
    }
}

// Catches a joiner opening ports 80 and 443 on a Cluster founded closed, whose tunnel has
// not opened yet.
#[tokio::test(start_paused = true)]
async fn a_joiner_on_a_cluster_founded_closed_opens_no_door() {
    let founder = machine('f', "founder");
    let joiner = machine('1', "joiner");
    let holder = ployz_core::holder_service_spec("pause:1", ployz_core::IngressPorts::Closed)
        .to_resolved(service_id('d'), ResolvedUpdateConfig::default())
        .expect("volume graph is scoped");
    let cluster = vec![
        global_service(
            QualifiedService::system_ingress(),
            'a',
            Placement::default(),
            running_on(&founder, 'a'),
        ),
        grouped(
            QualifiedService::system_ingress_holder(),
            holder,
            running_on(&founder, 'e'),
        ),
    ];
    let mut client = fake(&joiner, cluster);

    catch_up(&mut client, &joiner).await.unwrap();

    let [(_, asked)] = client.door_calls.as_slice() else {
        panic!("one call sets the doors: {:?}", client.door_calls);
    };
    assert_eq!(asked.doors, ployz_core::IngressDoors::closed());
}

// Catches catch-up following the newest holder when the Cluster declares its Doors: a closed
// Cluster stays closed whatever its Servers last ran, and one declared Public opens.
#[tokio::test(start_paused = true)]
async fn a_joiner_follows_the_declared_doors_over_the_newest_holder() {
    let founder = machine('f', "founder");
    let joiner = machine('1', "joiner");
    let holder = |ports| {
        ployz_core::holder_service_spec("pause:1", ports)
            .to_resolved(service_id('d'), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped")
    };
    let cluster = |ports| {
        vec![
            global_service(
                QualifiedService::system_ingress(),
                'a',
                Placement::default(),
                running_on(&founder, 'a'),
            ),
            grouped(
                QualifiedService::system_ingress_holder(),
                holder(ports),
                running_on(&founder, 'e'),
            ),
        ]
    };
    for (running, declared) in [
        (
            ployz_core::IngressPorts::Public,
            ployz_core::IngressDoors::closed(),
        ),
        (
            ployz_core::IngressPorts::Closed,
            ployz_core::IngressDoors::public(),
        ),
    ] {
        let mut client = fake(&joiner, cluster(running));
        client.declared = Some(declared.clone());
        catch_up(&mut client, &joiner).await.unwrap();
        let [(_, asked)] = client.door_calls.as_slice() else {
            panic!("one call sets the doors: {:?}", client.door_calls);
        };
        assert_eq!(asked.doors, declared);
    }
}

// Catches catch-up calling a Caddy that crashes after the daemon set it caught up.
#[tokio::test(start_paused = true)]
async fn a_caddy_that_is_not_running_after_its_doors_fails_catch_up() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let mut client = fake(
        &joiner,
        vec![global_service(
            QualifiedService::system_ingress(),
            'a',
            Placement::default(),
            running_on(&founder, 'a'),
        )],
    );
    let mut set = doors_on(&joiner);
    if let Some(proxy) = &mut set.proxy {
        proxy
            .now
            .try_update(|parts| parts.runtime = ContainerRuntimeObservation::Restarting)
            .unwrap();
    }
    client.doors_result = Ok(Some(set));

    let error = catch_up(&mut client, &joiner).await.unwrap_err();

    assert!(message(error, &joiner).contains("keeps crashing"));
}

// Catches a joiner going without the router, or getting it without its key.
#[tokio::test(start_paused = true)]
async fn a_joiner_gets_the_router_through_the_sealed_key_an_observed_one_carries() {
    let mut joiner = machine('1', "joiner");
    joiner.accepts_ingress = false;
    let founder = machine('f', "founder");
    let sealed = Ciphertext::from_bytes(vec![5; 48]);
    let mut client = fake(&joiner, vec![router_service(&founder, &sealed)]);

    catch_up(&mut client, &joiner).await.unwrap();

    assert_eq!(client.private_access_calls, [sealed]);
    assert!(client.door_calls.is_empty());
}

// Catches catch-up sending the router to a joiner whose daemon predates Private Access,
// which would answer an unknown route instead of the client naming the capability.
#[tokio::test(start_paused = true)]
async fn a_joiner_whose_daemon_predates_private_access_is_named_and_not_asked() {
    let founder = machine('f', "founder");
    let mut joiner = machine('1', "joiner");
    joiner.accepts_ingress = false;
    joiner.runtime.capabilities = [ployz_core::SET_INGRESS_DOORS_CAPABILITY.to_owned()].into();
    let mut client = fake(
        &joiner,
        vec![router_service(
            &founder,
            &Ciphertext::from_bytes(vec![5; 48]),
        )],
    );

    let error = catch_up(&mut client, &joiner).await.unwrap_err();

    assert!(client.private_access_calls.is_empty());
    assert!(
        message(error, &joiner).contains(ployz_core::SET_PRIVATE_ACCESS_CAPABILITY),
        "the refusal names the capability the joiner lacks"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cluster_with_no_router_asks_the_joiner_for_none() {
    let mut joiner = machine('1', "joiner");
    joiner.accepts_ingress = false;
    let mut client = fake(&joiner, Vec::new());

    catch_up(&mut client, &joiner).await.unwrap();

    assert!(client.private_access_calls.is_empty());
}

// Catches a joiner that Tailscale refused counting as caught up.
#[tokio::test(start_paused = true)]
async fn a_router_tailscale_refused_fails_catch_up_and_names_the_way_to_retry() {
    let mut joiner = machine('1', "joiner");
    joiner.accepts_ingress = false;
    let founder = machine('f', "founder");
    let mut client = fake(
        &joiner,
        vec![router_service(
            &founder,
            &Ciphertext::from_bytes(vec![5; 48]),
        )],
    );
    client.private_access_result = Ok(PrivateAccessSet {
        failure: Some(ployz_core::PrivateAccessFailure {
            problem: ployz_core::PrivateAccessProblem::KeyRefused,
            detail: "invalid key".into(),
        }),
        ..router_on(&joiner)
    });

    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    let failure = joined_catch_up_error(error, &joiner, |args| {
        shell_words::join(std::iter::once("ployz").chain(args.iter().copied()))
    });
    let said = error_chain::row(&failure);

    assert!(said.contains("refused the auth key"), "{said}");
    assert!(!said.contains("redeploy"), "{said}");
    assert_eq!(
        failure.hints(),
        [Hint::Retry(
            "ployz private-access on --auth-key-stdin".to_owned()
        )]
    );
}

fn message(error: CatchUpError, joiner: &Machine) -> String {
    error_chain::row(&joined_catch_up_error(error, joiner, |args| {
        shell_words::join(std::iter::once("ployz").chain(args.iter().copied()))
    }))
}

async fn catch_up(client: &mut FakeCatchUpClient, joiner: &Machine) -> Result<(), CatchUpError> {
    catch_up_globals(
        client,
        joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
}

fn fake(joiner: &Machine, services: Vec<ServiceObservation>) -> FakeCatchUpClient {
    FakeCatchUpClient {
        machine_id: joiner.id,
        services,
        door_calls: Vec::new(),
        doors_result: Ok(Some(doors_on(joiner))),
        private_access_calls: Vec::new(),
        private_access_result: Ok(router_on(joiner)),
        failures: Vec::new(),
        omissions: Vec::new(),
        names: ServerNames::default(),
        unarrived: ServerNames::default(),
        declared: None,
        listings: 0,
    }
}

/// The answer of a daemon that created the Private Access router on `machine`, running.
fn router_on(machine: &Machine) -> PrivateAccessSet {
    PrivateAccessSet {
        router: Some(ployz_core::DoorOutcome {
            now: running_on(machine, 'c'),
            change: ployz_core::DoorChange::Created,
        }),
        ..PrivateAccessSet::default()
    }
}

/// The router a Server runs, carrying `sealed` as its auth key.
fn router_service(server: &Machine, sealed: &Ciphertext) -> ServiceObservation {
    let identity = QualifiedService::system_tailscale();
    let mut spec = requested(ServiceMode::Global);
    spec.name = identity.name.clone();
    spec.container.labels =
        ployz_core::ContainerLabels::parse(std::collections::BTreeMap::from([(
            ployz_core::PRIVATE_ACCESS_KEY_LABEL.to_owned(),
            String::from(sealed.clone()),
        )]))
        .unwrap();
    grouped(
        identity,
        spec.to_resolved(service_id('c'), ResolvedUpdateConfig::default())
            .unwrap(),
        running_on(server, 'c'),
    )
}

/// The answer of a daemon that created the holder and Caddy on `machine`, both running.
fn doors_on(machine: &Machine) -> IngressDoorsSet {
    let created = |hex| ployz_core::DoorOutcome {
        now: running_on(machine, hex),
        change: ployz_core::DoorChange::Created,
    };
    IngressDoorsSet {
        holder: Some(created('d')),
        proxy: Some(created('e')),
        ..IngressDoorsSet::default()
    }
}

fn flatten(services: &[ServiceObservation]) -> Vec<ContainerObservation> {
    services
        .iter()
        .flat_map(ServiceObservation::members)
        .map(|container| container.as_observation().clone())
        .collect()
}

/// A user Global left from Compose before 0.1.2-beta.42 is reported, not copied, and does
/// not fail the join: no Machine admits it, and its Deploy with a replica count is the way
/// out.
#[tokio::test(start_paused = true)]
async fn a_legacy_user_global_is_reported_and_not_copied() {
    let mut joiner = machine('1', "joiner");
    joiner.accepts_ingress = false;
    let founder = machine('f', "founder");
    let metrics = qualified("app", "metrics");
    let mut client = fake(
        &joiner,
        vec![global_service(
            metrics.clone(),
            'a',
            Placement::default(),
            running_on(&founder, 'a'),
        )],
    );
    let mut identified = Vec::new();
    let mut legacy = Vec::new();

    catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |fact| match fact {
            CatchUpFact::Identified(services) => identified = services,
            CatchUpFact::Legacy(services) => legacy = services,
            CatchUpFact::Creating(_)
            | CatchUpFact::Excluded(_)
            | CatchUpFact::Running(_)
            | CatchUpFact::Failed(_)
            | CatchUpFact::NotArrived(_) => {}
        },
    )
    .await
    .unwrap();

    assert_eq!(legacy, [metrics]);
    assert!(identified.is_empty(), "{identified:?}");
}

#[tokio::test(start_paused = true)]
async fn interruption_reports_only_the_unresolved_globals() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let mut client = fake(
        &joiner,
        vec![router_service(
            &founder,
            &Ciphertext::from_bytes(vec![5; 48]),
        )],
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut running = Vec::new();

    let error = catch_up_globals(&mut client, &joiner, &cancel, |fact| match fact {
        CatchUpFact::Running(service) => {
            running.push(service);
            cancel.cancel();
        }
        CatchUpFact::Identified(_)
        | CatchUpFact::Creating(_)
        | CatchUpFact::Excluded(_)
        | CatchUpFact::Failed(_)
        | CatchUpFact::NotArrived(_)
        | CatchUpFact::Legacy(_) => {}
    })
    .await
    .unwrap_err();

    assert!(error.cause.is_interrupted());
    assert_eq!(running, [QualifiedService::system_ingress()]);
    assert_eq!(error.unresolved, [QualifiedService::system_tailscale()]);
    assert!(client.private_access_calls.is_empty());
}

struct FakeCatchUpClient {
    machine_id: MachineId,
    services: Vec<ServiceObservation>,
    door_calls: Vec<(ServiceId, ClusterDoors)>,
    doors_result: Result<Option<IngressDoorsSet>, RpcError>,
    private_access_calls: Vec<Ciphertext>,
    private_access_result: Result<PrivateAccessSet, RpcError>,
    failures: Vec<ployz_core::MachineFailure<RpcError>>,
    omissions: Vec<MachineId>,
    names: ServerNames,
    unarrived: ServerNames,
    declared: Option<ployz_core::IngressDoors>,
    listings: u32,
}

impl CatchUpClient for FakeCatchUpClient {
    async fn live_services(&mut self) -> Result<Observed, Failure> {
        self.listings += 1;
        let services = LiveServices {
            containers: ployz_core::PartialResult {
                successes: vec![ployz_core::MachineSuccess {
                    machine_id: self.machine_id,
                    value: flatten(&self.services),
                }],
                failures: self.failures.clone(),
                omissions: self.omissions.clone(),
            },
        };
        Ok(Observed {
            live: Live {
                services,
                names: self.names.clone(),
            },
            unarrived: self.unarrived.clone(),
            declared: self.declared.clone(),
        })
    }

    async fn run_doors(
        &mut self,
        machine_id: &MachineId,
        proxy_service: ServiceId,
        doors: ClusterDoors,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Option<IngressDoorsSet>, RpcError> {
        assert_eq!(machine_id, &self.machine_id);
        self.door_calls.push((proxy_service, doors));
        self.doors_result.clone()
    }

    async fn run_private_access(
        &mut self,
        machine_id: &MachineId,
        sealed: Ciphertext,
    ) -> Result<PrivateAccessSet, RpcError> {
        assert_eq!(machine_id, &self.machine_id);
        self.private_access_calls.push(sealed);
        self.private_access_result.clone()
    }
}

#[path = "global_catch_up_rpc_tests.rs"]
mod rpc_tests;
