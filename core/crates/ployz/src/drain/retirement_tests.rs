//! Tests for retiring a Drain's chosen Globals on the drained Server.

use ployz_core::{
    Machine, MachineId, ObservedGlobalSlotSpec, Placement, QualifiedService, ResolvedUpdateConfig,
    RpcError, RpcErrorCode, ServiceMode, ServiceObservation,
};

use super::*;
use crate::global_fixtures::{
    global_service, grouped, machine, qualified, requested, running_on, service_id,
};

struct FakeRetireClient {
    machine_id: MachineId,
    services: Vec<ServiceObservation>,
    retire_calls: Vec<QualifiedService>,
    retire_error: Option<RpcError>,
    cancel_on_retire: Option<CancellationToken>,
}

impl RetireClient for FakeRetireClient {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, ConnectError> {
        Ok(LiveServices {
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
                omissions: Vec::new(),
            },
        })
    }

    async fn retire_slot(
        &mut self,
        _machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
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
