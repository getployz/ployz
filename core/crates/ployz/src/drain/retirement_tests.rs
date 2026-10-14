//! Tests for retiring a Drain's chosen Globals on the drained Server.

use ployz_core::{
    Machine, MachineId, MachineName, ObservedGlobalSlotSpec, Placement, QualifiedService,
    ResolvedUpdateConfig, RpcError, RpcErrorCode, ServiceMode, ServiceObservation,
};

use super::*;
use crate::global_fixtures::{
    global_service, grouped, machine, qualified, requested, running_on, service_id,
};
use crate::serving_publish::tests::{FakeRow, container, pointer};

struct FakeRetireClient {
    machine_id: MachineId,
    services: Vec<ServiceObservation>,
    retire_calls: Vec<QualifiedService>,
    retire_error: Option<RpcError>,
    cancel_on_retire: Option<CancellationToken>,
    silent: Option<(MachineId, MachineName)>,
}

impl RetireClient for FakeRetireClient {
    async fn live_services(&mut self) -> Result<(LiveServices<RpcError>, Names), ConnectError> {
        let live = LiveServices {
            containers: ployz_core::PartialResult {
                successes: vec![ployz_core::MachineSuccess {
                    machine_id: self.machine_id,
                    value: self
                        .services
                        .iter()
                        .flat_map(ServiceObservation::members)
                        .map(|container| container.as_observation().clone())
                        .collect(),
                }],
                failures: Vec::new(),
                omissions: self.silent.iter().map(|(id, _)| *id).collect(),
            },
        };
        Ok((live, self.silent.iter().cloned().collect()))
    }

    async fn retire_slot(
        &mut self,
        _machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
        _cancellation: &CancellationToken,
    ) -> Result<(), RpcError> {
        self.retire_calls.push(slot.identity().clone());
        if let Some(cancellation) = &self.cancel_on_retire {
            cancellation.cancel();
        }
        self.retire_error.clone().map_or(Ok(()), Err)
    }
}

fn retiring(drained: &Machine, services: Vec<ServiceObservation>) -> FakeRetireClient {
    FakeRetireClient {
        machine_id: drained.id,
        services,
        retire_calls: Vec::new(),
        retire_error: None,
        cancel_on_retire: None,
        silent: None,
    }
}

fn global_on(drained: &Machine, namespace: &str, hex: char) -> ServiceObservation {
    global_service(
        qualified(namespace, "api"),
        hex,
        Placement::default(),
        running_on(drained, hex),
    )
}

#[tokio::test]
async fn retirement_touches_only_the_chosen_globals() {
    let drained = machine('1', "drained");
    let mut client = retiring(
        &drained,
        vec![
            global_on(&drained, "app", 'a'),
            global_on(&drained, "other", 'b'),
        ],
    );
    let chosen = qualified("app", "api");
    let never = CancellationToken::new();
    let retired =
        retire_globals(&mut client, &drained, std::slice::from_ref(&chosen), &never).await;
    assert_eq!(retired, [(chosen.clone(), Retirement::Retired)]);
    assert_eq!(client.retire_calls, [chosen]);

    client.retire_calls.clear();
    assert!(
        retire_globals(&mut client, &drained, &[], &never)
            .await
            .is_empty()
    );
    assert!(client.retire_calls.is_empty());
}

#[tokio::test]
async fn retirement_names_the_server_that_left_the_observation_partial() {
    let drained = machine('1', "drained");
    let beta = machine('2', "beta");
    let mut client = retiring(&drained, vec![global_on(&drained, "app", 'a')]);
    client.silent = Some((beta.id, beta.name.clone()));
    let chosen = qualified("app", "api");
    let outcomes = retire_globals(
        &mut client,
        &drained,
        std::slice::from_ref(&chosen),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes,
        [(
            chosen,
            Retirement::NotRetired(
                "cannot retire from partial Service observations: beta: no terminal response"
                    .into()
            )
        )]
    );
    assert!(client.retire_calls.is_empty());
}

#[tokio::test]
async fn retirement_reports_every_global_and_stops_at_the_next_once_cancelled() {
    let drained = machine('1', "drained");
    let globals = [qualified("app", "api"), qualified("other", "api")];
    let services = vec![
        global_on(&drained, "app", 'a'),
        global_on(&drained, "other", 'b'),
    ];

    let cancellation = CancellationToken::new();
    let mut client = retiring(&drained, services.clone());
    client.cancel_on_retire = Some(cancellation.clone());
    assert_eq!(
        retire_globals(&mut client, &drained, &globals, &cancellation).await,
        [
            (globals[0].clone(), Retirement::Retired),
            (globals[1].clone(), Retirement::NotAttempted),
        ]
    );
    assert_eq!(client.retire_calls, [globals[0].clone()]);

    let mut client = retiring(&drained, services);
    client.retire_error = Some(RpcError {
        code: RpcErrorCode::Conflict,
        message: "the Server accepts it again".into(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    });
    let gone = qualified("gone", "api");
    let outcomes = retire_globals(
        &mut client,
        &drained,
        &[globals[0].clone(), gone.clone()],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes,
        [
            (
                globals[0].clone(),
                Retirement::NotRetired("the Server accepts it again".into())
            ),
            (gone, Retirement::Retired),
        ],
        "a Global a full observation no longer finds has nothing left to retire"
    );
}

#[tokio::test]
async fn a_global_whose_newest_container_is_not_global_is_reported_not_retired() {
    let drained = machine('1', "drained");
    let identity = qualified("app", "api");
    let mut spec = requested(ServiceMode::Replicated {
        replicas: std::num::NonZeroU32::MIN,
    });
    spec.name = identity.name.clone();
    let replicated = grouped(
        identity.clone(),
        spec.to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped"),
        running_on(&drained, 'a'),
    );
    let mut client = retiring(&drained, vec![replicated]);
    let outcomes = retire_globals(
        &mut client,
        &drained,
        std::slice::from_ref(&identity),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes,
        [(
            identity,
            Retirement::NotRetired("its newest Container is not Global; deploy it first".into())
        )]
    );
    assert!(client.retire_calls.is_empty());
}

struct PointedServer {
    row: FakeRow,
    copies: Vec<ployz_core::ContainerId>,
    polls_left: Option<u32>,
    /// A newer deploy's pointer and the copy it names, both arriving at the second listing.
    later: Option<(ployz_core::ServingPointer, ployz_core::ContainerId)>,
    listings: u32,
    listed: Vec<QualifiedService>,
}

impl PointerClient for PointedServer {
    async fn pointer_publisher(&mut self) -> Result<ployz_core::MachineTarget, RpcError> {
        self.row.pointer_publisher().await
    }

    async fn read_pointer_at(
        &mut self,
        at: &mut ployz_core::MachineTarget,
        service: &QualifiedService,
    ) -> Result<Option<ployz_core::ServingPointer>, RpcError> {
        self.row.read_pointer_at(at, service).await
    }

    async fn publish_pointer_at(
        &mut self,
        at: &mut ployz_core::MachineTarget,
        request: ployz_core::PublishServingPointerRequest,
    ) -> Result<ployz_core::ServingPointerPublication, RpcError> {
        self.row.publish_pointer_at(at, request).await
    }
}

impl CopyLister for PointedServer {
    async fn copies_on(
        &mut self,
        _machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<ployz_core::ContainerId>, RpcError> {
        self.listings += 1;
        self.listed.push(service.clone());
        if self.listings == 2
            && let Some((row, copy)) = self.later.take()
        {
            *self.row.row.lock().unwrap() = Some(row);
            self.copies.push(copy);
        }
        let copies = self.copies.clone();
        match &mut self.polls_left {
            Some(0) => self.copies.clear(),
            Some(left) => *left -= 1,
            None => {}
        }
        Ok(copies)
    }

    async fn remove_copy(
        &mut self,
        _machine_id: &MachineId,
        copy: &ployz_core::ContainerId,
    ) -> Result<(), RpcError> {
        self.copies.retain(|listed| listed != copy);
        Ok(())
    }
}

fn pointed_server(row: &FakeRow, polls_left: Option<u32>) -> PointedServer {
    PointedServer {
        row: row.clone(),
        copies: vec![container('2')],
        polls_left,
        later: None,
        listings: 0,
        listed: Vec::new(),
    }
}

#[tokio::test(start_paused = true)]
async fn a_newer_deploys_pointer_during_the_wait_ends_the_retirement_unretired() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    let mut server = pointed_server(&row, None);
    let newer = pointer('b', &['5'], &['2']);
    server.later = Some((newer.clone(), container('5')));
    let error = retire_copies(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &QualifiedService::parse("app/api").unwrap(),
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
    assert!(error.message.contains("newer deploy"), "{error:?}");
    assert_eq!(
        *row.row.lock().unwrap(),
        Some(newer),
        "the newer deploy's copy is not amended into retiring"
    );
    assert_eq!(
        server.copies,
        [container('2'), container('5')],
        "the newer deploy's copy is not removed"
    );
}

#[tokio::test(start_paused = true)]
async fn a_newer_deploys_pointer_ends_a_retirement_that_began_on_a_retiring_copy() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1'], &['2']));
    let mut server = pointed_server(&row, None);
    let newer = pointer('b', &['5'], &['2']);
    server.later = Some((newer.clone(), container('5')));
    let error = retire_copies(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &QualifiedService::parse("app/api").unwrap(),
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
    assert!(error.message.contains("newer deploy"), "{error:?}");
    assert_eq!(
        *row.row.lock().unwrap(),
        Some(newer),
        "the newer deploy's copy is not amended into retiring"
    );
    assert_eq!(
        server.copies,
        [container('2'), container('5')],
        "the newer deploy's copy is not removed"
    );
}

#[tokio::test(start_paused = true)]
async fn a_pointed_global_retires_through_the_pointer_and_waits_for_its_copies_to_leave() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    let mut server = pointed_server(&row, Some(2));
    let service = QualifiedService::parse("app/api").unwrap();
    retire_copies(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &service,
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(*row.row.lock().unwrap(), Some(pointer('a', &['1'], &['2'])));
    assert!(server.copies.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_pointed_global_still_there_after_the_wait_is_not_retired() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    let mut server = pointed_server(&row, None);
    let error = retire_copies(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &QualifiedService::parse("app/api").unwrap(),
        LISTING + LISTING / 2,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("still retiring"), "{error:?}");
}

#[tokio::test]
async fn an_unnamed_global_copy_is_removed_at_once() {
    let row = FakeRow::default();
    let mut server = pointed_server(&row, None);
    retire_copies(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &QualifiedService::parse("app/api").unwrap(),
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(*row.row.lock().unwrap(), None);
    assert!(server.copies.is_empty());
}

#[tokio::test]
async fn stopping_ingress_removes_an_unpointed_ingress_proxy_at_once() {
    let row = FakeRow::default();
    let mut server = pointed_server(&row, None);
    retire_ingress_proxy(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(server.copies.is_empty(), "the proxy copy is removed");
    assert!(
        server
            .listed
            .iter()
            .all(|service| *service == QualifiedService::system_ingress()),
        "only the system Ingress Proxy is listed: {:?}",
        server.listed
    );
    assert_eq!(*row.row.lock().unwrap(), None, "no pointer is written");
}

#[tokio::test(start_paused = true)]
async fn a_pointed_copy_is_never_removed_while_an_unnamed_one_beside_it_is() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    let mut server = pointed_server(&row, None);
    server.copies.push(container('9'));
    let error = retire_copies(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &QualifiedService::parse("app/api").unwrap(),
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("still retiring"), "{error:?}");
    assert_eq!(
        *row.row.lock().unwrap(),
        Some(pointer('a', &['1'], &['2'])),
        "only the id the pointer named is amended into retiring"
    );
    assert_eq!(server.copies, [container('2')], "the pointed copy stays");
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_drain_stops_waiting_for_a_pointed_global_at_once() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    let mut server = pointed_server(&row, None);
    let cancellation = CancellationToken::new();
    let machine = MachineId::parse("b".repeat(32)).unwrap();
    let service = QualifiedService::parse("app/api").unwrap();
    let started = tokio::time::Instant::now();
    let (outcome, ()) = tokio::join!(
        retire_copies(
            &mut server,
            &machine,
            &service,
            POINTED_RETIRE_WAIT,
            &cancellation,
        ),
        async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            cancellation.cancel();
        },
    );
    let error = outcome.unwrap_err();
    assert!(error.message.contains("cancelled"), "{error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn a_pointed_global_lists_no_copies_past_its_deadline() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    let mut server = SlowLister {
        server: pointed_server(&row, None),
        listings: 0,
    };
    let error = retire_copies(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &QualifiedService::parse("app/api").unwrap(),
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("still retiring"), "{error:?}");
    assert_eq!(
        server.listings, 2,
        "the first listing ran past the deadline"
    );
}

const LISTING: Duration = Duration::from_secs(20);

struct SlowLister {
    server: PointedServer,
    listings: u32,
}

impl PointerClient for SlowLister {
    async fn pointer_publisher(&mut self) -> Result<ployz_core::MachineTarget, RpcError> {
        self.server.pointer_publisher().await
    }

    async fn read_pointer_at(
        &mut self,
        at: &mut ployz_core::MachineTarget,
        service: &QualifiedService,
    ) -> Result<Option<ployz_core::ServingPointer>, RpcError> {
        self.server.read_pointer_at(at, service).await
    }

    async fn publish_pointer_at(
        &mut self,
        at: &mut ployz_core::MachineTarget,
        request: ployz_core::PublishServingPointerRequest,
    ) -> Result<ployz_core::ServingPointerPublication, RpcError> {
        self.server.publish_pointer_at(at, request).await
    }
}

impl CopyLister for SlowLister {
    async fn copies_on(
        &mut self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<ployz_core::ContainerId>, RpcError> {
        self.listings += 1;
        tokio::time::sleep(LISTING).await;
        self.server.copies_on(machine_id, service).await
    }

    async fn remove_copy(
        &mut self,
        machine_id: &MachineId,
        copy: &ployz_core::ContainerId,
    ) -> Result<(), RpcError> {
        self.server.remove_copy(machine_id, copy).await
    }
}

mod rpc {
    use std::sync::{Arc, Mutex};

    use ployz_core::{
        ContainerList, ContainerLocator, ContainerNow, EnsuredContainer, LocalMachinePhase,
        MachineDetails, MachineList, MachineObservation, MembershipObservation, OpaquePayload,
        RpcRequestBody, RpcResponse, ServingPointer, ServingPointerPublication, ServingPointerRead,
    };
    use tonic::{Request, Response};

    use super::*;

    async fn retire_from_ineligible(
        initial: Option<ServingPointer>,
        published_after_first_listing: Option<ServingPointer>,
    ) -> (Result<(), RpcError>, Option<ServingPointer>, u32) {
        let mut drained = machine('1', "drained");
        drained.accepts_services = false;
        let service = global_on(&drained, "app", 'a');
        let copy = running_on(&drained, 'a');
        let row = Arc::new(Mutex::new(initial));
        let published = Arc::new(Mutex::new(published_after_first_listing));
        let removed = Arc::new(Mutex::new(false));
        let ensured = Arc::new(Mutex::new(0_u32));
        let observed = drained.clone();
        let shared = row.clone();
        let removals = ensured.clone();
        let gone = removed.clone();
        let (mut client, server) =
            crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
                let drained = observed.clone();
                let copy = copy.clone();
                let row = shared.clone();
                let published = published.clone();
                let removals = removals.clone();
                let removed = gone.clone();
                async move {
                    #[expect(
                        clippy::wildcard_enum_match_arm,
                        reason = "fixture answers only what pointed retirement sends"
                    )]
                    let response = match rpc.into_inner().decode_request().unwrap().body {
                        RpcRequestBody::DescribeContract(_) => {
                            RpcResponse::from(ployz_core::ContractDescription {
                                machine_id: drained.id,
                                protocol_major: ployz_core::PROTOCOL_MAJOR,
                                daemon_version: "fixture".into(),
                                capabilities: Default::default(),
                            })
                        }
                        RpcRequestBody::Inspect(_) => RpcResponse::from(MachineDetails {
                            id: drained.id,
                            public_key: drained.public_key,
                            machine: Some(drained.clone()),
                            phase: LocalMachinePhase::Participating,
                            advertised_endpoints: Vec::new(),
                            store_version: Default::default(),
                            rtts: Vec::new(),
                            management_clients: Vec::new(),
                            storage: None,
                            upgrade: None,
                            telemetry: None,
                            token: None,
                        }),
                        RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                            enrollment: None,
                            machines: vec![MachineObservation::new(
                                drained,
                                MembershipObservation::Up,
                            )],
                        }),
                        RpcRequestBody::ListContainers(_) => {
                            let mut row = row.lock().unwrap();
                            let retired = row
                                .as_ref()
                                .is_some_and(|row| row.retiring.contains(&copy.container_id));
                            let listed = if retired || *removed.lock().unwrap() {
                                Vec::new()
                            } else {
                                vec![copy.clone()]
                            };
                            if let Some(first) = published.lock().unwrap().take() {
                                *row = Some(first);
                            }
                            RpcResponse::from(ContainerList { containers: listed })
                        }
                        RpcRequestBody::ReadServingPointer(_) => {
                            RpcResponse::from(ServingPointerRead {
                                pointer: row.lock().unwrap().clone(),
                            })
                        }
                        RpcRequestBody::PublishServingPointer(request) => {
                            let mut row = row.lock().unwrap();
                            RpcResponse::from(if *row == request.read {
                                *row = Some(request.pointer);
                                ServingPointerPublication::Published
                            } else {
                                ServingPointerPublication::Superseded {
                                    current: row.clone(),
                                }
                            })
                        }
                        RpcRequestBody::EnsureContainer(_) => {
                            *removals.lock().unwrap() += 1;
                            let pointed = row
                                .lock()
                                .unwrap()
                                .as_ref()
                                .is_some_and(|row| row.containers.contains(&copy.container_id));
                            if pointed {
                                RpcResponse::from(RpcError {
                                    code: RpcErrorCode::Conflict,
                                    message: "a pointed Global is not removed directly".into(),
                                    details: serde_json::Value::Null,
                                    cause: Vec::new(),
                                })
                            } else {
                                *removed.lock().unwrap() = true;
                                RpcResponse::from(EnsuredContainer::Ensured {
                                    now: ContainerNow::Absent {
                                        of: ContainerLocator::Id(copy.container_id),
                                    },
                                    health: None,
                                })
                            }
                        }
                        other => panic!("unexpected RPC: {other:?}"),
                    };
                    Ok(Response::new(response.encode().unwrap()))
                }
            })
            .await;
        let slot = service.observed_global_slot().unwrap();
        let retired = client
            .retire_slot(&drained.id, &slot, &CancellationToken::new())
            .await;
        server.abort();
        let row = row.lock().unwrap().clone();
        let ensured = *ensured.lock().unwrap();
        (retired, row, ensured)
    }

    #[tokio::test]
    async fn a_pointed_global_on_an_ineligible_server_retires_through_its_pointer() {
        let (retired, row, ensured) =
            retire_from_ineligible(Some(pointer('d', &['a'], &[])), None).await;
        retired.unwrap();
        assert_eq!(row, Some(pointer('d', &[], &['a'])));
        assert_eq!(ensured, 0);
    }

    #[tokio::test]
    async fn a_first_pointer_published_after_the_listing_retires_the_global_through_it() {
        let (retired, row, ensured) =
            retire_from_ineligible(None, Some(pointer('d', &['a'], &[]))).await;
        retired.unwrap();
        assert_eq!(row, Some(pointer('d', &[], &['a'])));
        assert_eq!(ensured, 0, "the pointed copy is never removed directly");
    }

    #[tokio::test]
    async fn an_unpointed_global_on_an_ineligible_server_is_removed() {
        let (retired, row, ensured) = retire_from_ineligible(None, None).await;
        retired.unwrap();
        assert_eq!(row, None);
        assert_eq!(ensured, 1);
    }
}
