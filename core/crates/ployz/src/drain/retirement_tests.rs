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
}

impl PointerClient for PointedServer {
    async fn read_pointer(
        &mut self,
        service: &QualifiedService,
    ) -> Result<Option<ployz_core::ServingPointer>, RpcError> {
        self.row.read_pointer(service).await
    }

    async fn publish_pointer(
        &mut self,
        request: ployz_core::PublishServingPointerRequest,
    ) -> Result<ployz_core::ServingPointerPublication, RpcError> {
        self.row.publish_pointer(request).await
    }
}

impl CopyLister for PointedServer {
    async fn copies_on(
        &mut self,
        _machine_id: &MachineId,
        _service: &QualifiedService,
    ) -> Result<Vec<ployz_core::ContainerId>, RpcError> {
        let copies = self.copies.clone();
        match &mut self.polls_left {
            Some(0) => self.copies.clear(),
            Some(left) => *left -= 1,
            None => {}
        }
        Ok(copies)
    }
}

fn pointed_server(row: &FakeRow, polls_left: Option<u32>) -> PointedServer {
    PointedServer {
        row: row.clone(),
        copies: vec![container('2')],
        polls_left,
    }
}

#[tokio::test(start_paused = true)]
async fn a_pointed_global_retires_through_the_pointer_and_waits_for_its_copies_to_leave() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    let mut server = pointed_server(&row, Some(2));
    let service = QualifiedService::parse("app/api").unwrap();
    let pointed = retire_pointed(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &service,
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(pointed);
    assert_eq!(*row.row.lock().unwrap(), Some(pointer('a', &['1'], &['2'])));
    assert!(server.copies.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_pointed_global_still_there_after_the_wait_is_not_retired() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    let mut server = pointed_server(&row, None);
    let error = retire_pointed(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &QualifiedService::parse("app/api").unwrap(),
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("still retiring"), "{error:?}");
}

#[tokio::test]
async fn a_global_with_no_pointer_is_left_to_today_s_removal() {
    let row = FakeRow::default();
    let mut server = pointed_server(&row, None);
    let pointed = retire_pointed(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &QualifiedService::parse("app/api").unwrap(),
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(!pointed);
    assert_eq!(*row.row.lock().unwrap(), None);
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
        retire_pointed(
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
    assert!(started.elapsed() < Duration::from_secs(6), "{:?}", started.elapsed());
}

#[tokio::test(start_paused = true)]
async fn a_pointed_global_lists_no_copies_past_its_deadline() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    let mut server = SlowLister {
        server: pointed_server(&row, None),
        listings: 0,
    };
    let error = retire_pointed(
        &mut server,
        &MachineId::parse("b".repeat(32)).unwrap(),
        &QualifiedService::parse("app/api").unwrap(),
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("still retiring"), "{error:?}");
    assert_eq!(server.listings, 2, "the first listing ran past the deadline");
}

/// Each listing takes 20 s, so a 30 s wait has time for the first two only.
struct SlowLister {
    server: PointedServer,
    listings: u32,
}

impl PointerClient for SlowLister {
    async fn read_pointer(
        &mut self,
        service: &QualifiedService,
    ) -> Result<Option<ployz_core::ServingPointer>, RpcError> {
        self.server.read_pointer(service).await
    }

    async fn publish_pointer(
        &mut self,
        request: ployz_core::PublishServingPointerRequest,
    ) -> Result<ployz_core::ServingPointerPublication, RpcError> {
        self.server.publish_pointer(request).await
    }
}

impl CopyLister for SlowLister {
    async fn copies_on(
        &mut self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<ployz_core::ContainerId>, RpcError> {
        self.listings += 1;
        tokio::time::sleep(Duration::from_secs(20)).await;
        self.server.copies_on(machine_id, service).await
    }
}
