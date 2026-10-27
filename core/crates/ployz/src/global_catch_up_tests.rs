//! Tests for bounded Global catch-up and target storage eligibility.

use std::num::NonZeroU64;

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerPath, ContainerRefusal,
    ContainerRuntimeObservation, DEPLOYMENT_ID_LABEL, DeploymentLogId, DockerVolumeName, Gen,
    HealthObservation, MachineId, MachineStorageObservation, Namespace, Placement,
    ProvisionedVolumeMaximumBytes, RequestedServiceSpec, ResolvedUpdateConfig, ServiceMode,
    ServiceMount, ServiceObservation, ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference,
};

use super::*;
use crate::global_fixtures::{
    container_id, created_on, global_service, global_service_with_image, grouped, machine,
    qualified, requested, running_on, service_id,
};

fn rotated(mut observed: ContainerObservation, rotation: CopyRotation) -> ContainerObservation {
    observed
        .try_update(|parts| parts.rotation = Some(rotation))
        .unwrap();
    observed
}

fn with_gen(mut observed: ContainerObservation, r#gen: u64) -> ContainerObservation {
    observed
        .try_update(|parts| parts.r#gen = Some(Gen::new(r#gen)))
        .unwrap();
    observed
}

fn message(error: CatchUpError, joiner: &Machine) -> String {
    crate::ui::chain_text(&joined_catch_up_error(error, joiner, |args| {
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

#[tokio::test(start_paused = true)]
async fn partial_observations_reject_catch_up_before_any_placement() {
    let joiner = machine('1', "joiner");
    let peer = machine('f', "peer");
    let unlisted = machine('e', "unlisted");
    for failed in [true, false] {
        let mut client = fake(&joiner, Vec::new());
        client.names = vec![
            (joiner.id, joiner.name.clone()),
            (peer.id, peer.name.clone()),
        ];
        if failed {
            client.failures = vec![ployz_core::MachineFailure {
                machine_id: peer.id,
                error: RpcError {
                    code: ployz_core::RpcErrorCode::Unavailable,
                    message: "peer unavailable".into(),
                    details: serde_json::Value::Null,
                    cause: Vec::new(),
                },
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
        assert!(client.run_calls.is_empty());
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
        client.names = vec![(newer.id, newer.name.clone()), (down.id, down.name.clone())];
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
        assert!(client.run_calls.is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn only_omissions_that_are_not_unarrived_block_catch_up() {
    let joiner = machine('1', "joiner");
    let peer = machine('f', "peer");
    let claimed = machine('e', "claimed");
    let mut client = fake(&joiner, Vec::new());
    client.names = vec![
        (peer.id, peer.name.clone()),
        (claimed.id, claimed.name.clone()),
    ];
    client.unarrived = vec![(claimed.id, claimed.name.clone())];
    client.omissions = vec![claimed.id];
    catch_up(&mut client, &joiner).await.unwrap();

    client.omissions.push(peer.id);
    let message = message(catch_up(&mut client, &joiner).await.unwrap_err(), &joiner);
    assert!(message.contains("peer: no terminal response"), "{message}");
    assert!(!message.contains("claimed"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn partial_observation_is_retried_in_run() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let peer = machine('e', "peer");
    let service = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&founder, 'a'),
    );
    let mut client = fake(&joiner, Vec::new());
    client.omissions = vec![peer.id];
    client.relisted = Some(vec![service]);
    client.target_services = Some(vec![global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&joiner, 'b'),
    )]);
    client.run_result = Ok(Some(container_id('b')));
    catch_up(&mut client, &joiner).await.unwrap();
    assert_eq!(client.listings, 2);
    assert_eq!(client.run_calls.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_copy_still_starting_is_waited_for() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let api = |on| global_service(qualified("app", "api"), 'a', Placement::default(), on);
    let mut client = fake(&joiner, vec![api(running_on(&founder, 'a'))]);
    client.run_result = Ok(Some(container_id('b')));
    // The joiner shows the new copy created, not yet running, twice, then running.
    client.starting = [created_on(&joiner, 'b'), created_on(&joiner, 'b')]
        .into_iter()
        .map(|on| vec![api(on)])
        .collect();
    client.target_services = Some(vec![api(running_on(&joiner, 'b'))]);
    catch_up(&mut client, &joiner).await.unwrap();
    assert!(client.starting.is_empty());
}

#[tokio::test(start_paused = true)]
async fn successful_ensure_is_reobserved_before_success() {
    let joiner = machine('1', "joiner");
    let service = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        created_on(&joiner, 'a'),
    );
    let mut client = fake(&joiner, vec![service]);
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

#[tokio::test(start_paused = true)]
async fn initially_eligible_global_absent_from_target_inspection_remains_missing() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let service = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&founder, 'a'),
    );
    let mut client = fake(&joiner, vec![service]);
    client.target_services = Some(Vec::new());
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
    let message = message(error, &joiner);
    assert!(
        message.contains(&format!(
            "app/api: copy {} is not running on this Machine after catch-up",
            container_id('a').as_str()
        )),
        "{message}"
    );
}

#[tokio::test(start_paused = true)]
async fn initially_eligible_global_with_only_hook_visible_remains_missing() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let service = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&founder, 'a'),
    );
    let mut hook = service
        .containers
        .first()
        .expect("test Global has one Service container")
        .clone()
        .into_observation();
    hook.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
        .unwrap();
    let hook_only = ServiceObservation {
        identity: service.identity.clone(),
        service_id: service.service_id,
        containers: Vec::new(),
        hook_containers: vec![ployz_core::HookContainer::try_from(hook).unwrap()],
    };
    let mut client = fake(&joiner, vec![service]);
    client.target_services = Some(vec![hook_only]);
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

#[tokio::test(start_paused = true)]
async fn initially_eligible_generation_absent_from_target_inspection_remains_missing() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let stale = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&joiner, 'a'),
    );
    let current = global_service_with_image(
        qualified("app", "api"),
        'b',
        Placement::default(),
        running_on(&founder, 'b'),
        "ghcr.io/getployz/api:2",
    );
    let mut client = fake(&joiner, vec![stale.clone(), current]);
    client.target_services = Some(vec![stale]);
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

#[tokio::test(start_paused = true)]
async fn another_namespaces_matching_shape_does_not_satisfy_catch_up() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let shop = global_service(
        qualified("shop", "api"),
        'c',
        Placement::default(),
        running_on(&joiner, 'a'),
    );
    let prod = global_service(
        qualified("prod", "api"),
        'd',
        Placement::default(),
        running_on(&founder, 'e'),
    );
    let mut client = fake(&joiner, vec![shop.clone(), prod]);
    client.target_services = Some(vec![shop]);
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert_eq!(error.unresolved, [qualified("prod", "api")]);
}

#[tokio::test(start_paused = true)]
async fn a_held_source_is_outdated() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let held = rotated(
        running_on(&founder, 'a'),
        CopyRotation::Held(DeploymentLogId::parse("dep-a").unwrap()),
    );
    let service = global_service(qualified("app", "api"), 'a', Placement::default(), held);
    let mut client = fake(&joiner, vec![service]);
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
    assert!(client.run_calls.is_empty(), "a Held copy is never a source");
    let message = message(error, &joiner);
    assert!(message.contains("redeploy"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn an_out_source_is_outdated() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let out = rotated(running_on(&founder, 'a'), CopyRotation::Out);
    let service = global_service(qualified("app", "api"), 'a', Placement::default(), out);
    let mut client = fake(&joiner, vec![service]);
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
    assert!(client.run_calls.is_empty(), "an Out copy is never a source");
    let message = message(error, &joiner);
    assert!(message.contains("redeploy"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn a_pending_copy_older_than_an_in_gen_is_not_the_source() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let identity = qualified("app", "api");
    let current = with_gen(rotated(running_on(&founder, 'a'), CopyRotation::In), 2);
    let stale = with_gen(created_on(&founder, 'b'), 1);
    let mut client = fake(
        &joiner,
        vec![
            global_service(identity.clone(), 'a', Placement::default(), current),
            global_service(identity.clone(), 'a', Placement::default(), stale),
        ],
    );
    client.target_services = Some(vec![global_service(
        identity,
        'a',
        Placement::default(),
        running_on(&joiner, 'c'),
    )]);
    client.run_result = Ok(Some(container_id('c')));
    catch_up(&mut client, &joiner).await.unwrap();
    let [new] = client.run_calls.as_slice() else {
        panic!("one copy starts: {:?}", client.run_calls);
    };
    assert_eq!(
        new.r#gen,
        Gen::new(2),
        "the newest In copy wins over an older Pending one"
    );
}

#[tokio::test(start_paused = true)]
async fn a_copy_keeps_its_sources_label_and_gen() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let deploy = DeploymentLogId::parse("dep-a").unwrap();
    let mut source = with_gen(rotated(running_on(&founder, 'a'), CopyRotation::In), 3);
    source
        .try_update(|parts| {
            parts
                .labels
                .insert(DEPLOYMENT_ID_LABEL.to_owned(), deploy.to_string());
        })
        .unwrap();
    let service = global_service(qualified("app", "api"), 'a', Placement::default(), source);
    let mut client = fake(&joiner, vec![service]);
    client.target_services = Some(vec![global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&joiner, 'b'),
    )]);
    client.run_result = Ok(Some(container_id('b')));
    catch_up(&mut client, &joiner).await.unwrap();
    let [new] = client.run_calls.as_slice() else {
        panic!("one copy starts: {:?}", client.run_calls);
    };
    assert_eq!(new.r#gen, Gen::new(3));
    assert_eq!(new.deployment_id, Some(deploy));
    assert_eq!(
        client
            .brought_in
            .iter()
            .map(|copy| &copy.container_id)
            .collect::<Vec<_>>(),
        [&container_id('b')],
        "a Pending copy is brought In after it starts"
    );
}

#[tokio::test(start_paused = true)]
async fn an_owner_gen_refusal_discards_the_copy() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let service = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&founder, 'a'),
    );
    let mut client = fake(&joiner, vec![service]);
    client.target_services = Some(vec![global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&joiner, 'b'),
    )]);
    client.run_result = Ok(Some(container_id('b')));
    client.bring_in = FakeBringIn::Refused(ContainerRefusalReason::OlderThanIn);
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
    assert_eq!(
        client
            .removed
            .iter()
            .map(|copy| &copy.container_id)
            .collect::<Vec<_>>(),
        [&container_id('b')],
        "a copy the owner refuses In is discarded"
    );
    assert!(client.retired.is_empty());
    let message = message(error, &joiner);
    assert!(message.contains(OUTDATED), "{message}");
}

#[tokio::test(start_paused = true)]
async fn own_superseded_copies_retire_after_in() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let identity = qualified("app", "api");
    let service = global_service(
        identity.clone(),
        'a',
        Placement::default(),
        rotated(running_on(&founder, 'b'), CopyRotation::In),
    );
    let old = running_on(&joiner, 'a');
    let mut client = fake(&joiner, vec![service]);
    client.target_services = Some(vec![
        global_service(
            identity.clone(),
            'a',
            Placement::default(),
            running_on(&joiner, 'c'),
        ),
        global_service(identity.clone(), 'a', Placement::default(), old.clone()),
    ]);
    client.run_result = Ok(Some(container_id('c')));
    catch_up(&mut client, &joiner).await.unwrap();
    assert_eq!(
        client
            .brought_in
            .iter()
            .map(|copy| &copy.container_id)
            .collect::<Vec<_>>(),
        [&container_id('c')]
    );
    let retired = client.retired.clone();
    let [retired] = retired.as_slice() else {
        panic!("only this Machine's superseded copy retires: {retired:?}");
    };
    assert_eq!(retired.container_id, container_id('a'));
    assert_eq!(retired.machine_id, joiner.id);
    assert_eq!(retired.service, identity);
    assert!(client.removed.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_proxy_copy_skips_rotation() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let ingress = QualifiedService::system_ingress();
    let held = rotated(
        running_on(&founder, 'a'),
        CopyRotation::Held(DeploymentLogId::parse("dep-a").unwrap()),
    );
    let service = global_service(ingress.clone(), 'a', Placement::default(), held);
    let mut client = fake(&joiner, vec![service]);
    client.target_services = Some(vec![
        global_service(
            ingress.clone(),
            'a',
            Placement::default(),
            running_on(&joiner, 'b'),
        ),
        global_service(ingress, 'a', Placement::default(), running_on(&joiner, 'c')),
    ]);
    client.run_result = Ok(Some(container_id('b')));
    catch_up(&mut client, &joiner).await.unwrap();
    assert_eq!(
        client.run_calls.len(),
        1,
        "a Held proxy copy is still the source"
    );
    assert!(
        client.brought_in.is_empty(),
        "the proxy stays Pending for life"
    );
    assert!(client.retired.is_empty());
    assert!(client.removed.is_empty());
}

#[tokio::test(start_paused = true)]
async fn interruption_settles_started_work_and_only_reports_unresolved_globals() {
    for excluded in [false, true] {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let first = qualified("app", "api");
        let second = qualified("app", "worker");
        let mut client = fake(
            &joiner,
            vec![
                global_service(
                    first.clone(),
                    'a',
                    Placement::default(),
                    running_on(&founder, 'a'),
                ),
                global_service(
                    second.clone(),
                    'b',
                    Placement::default(),
                    running_on(&founder, 'b'),
                ),
            ],
        );
        client.target_services = Some(vec![global_service(
            first.clone(),
            'a',
            Placement::default(),
            running_on(&joiner, 'a'),
        )]);
        client.run_result = Ok((!excluded).then(|| container_id('a')));
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut running = Vec::new();
        let error = catch_up_globals(&mut client, &joiner, &cancel, |fact| match fact {
            CatchUpFact::Creating(_) => cancel.cancel(),
            CatchUpFact::Running(service) => running.push(service),
            CatchUpFact::Identified(_)
            | CatchUpFact::Excluded(_)
            | CatchUpFact::Failed(_)
            | CatchUpFact::NotArrived(_) => {}
        })
        .await
        .unwrap_err();
        assert!(error.cause.is_interrupted());
        assert_eq!(error.unresolved, [second]);
        assert_eq!(
            client.run_calls.len(),
            1,
            "interruption stops before the next Global"
        );
        if excluded {
            assert!(running.is_empty());
        } else {
            assert_eq!(
                running,
                [first],
                "final target observation preserves completed work"
            );
        }
    }
}

enum FakeBringIn {
    In,
    Refused(ContainerRefusalReason),
}

struct FakeCatchUpClient {
    machine_id: MachineId,
    services: Vec<ServiceObservation>,
    target_services: Option<Vec<ServiceObservation>>,
    run_calls: Vec<NewContainer>,
    run_result: Result<Option<ContainerId>, RpcError>,
    bring_in: FakeBringIn,
    brought_in: Vec<Copy>,
    retired: Vec<Copy>,
    removed: Vec<Copy>,
    failures: Vec<ployz_core::MachineFailure<RpcError>>,
    omissions: Vec<MachineId>,
    names: crate::drain::retirement::Names,
    unarrived: crate::drain::retirement::Names,
    relisted: Option<Vec<ServiceObservation>>,
    listings: u32,
    /// What the joiner's Containers show, call by call, before `target_services`.
    starting: std::collections::VecDeque<Vec<ServiceObservation>>,
}

fn fake(joiner: &Machine, services: Vec<ServiceObservation>) -> FakeCatchUpClient {
    FakeCatchUpClient {
        machine_id: joiner.id,
        services,
        target_services: None,
        run_calls: Vec::new(),
        run_result: Ok(Some(container_id('a'))),
        bring_in: FakeBringIn::In,
        brought_in: Vec::new(),
        retired: Vec::new(),
        removed: Vec::new(),
        failures: Vec::new(),
        omissions: Vec::new(),
        names: Vec::new(),
        unarrived: Vec::new(),
        relisted: None,
        listings: 0,
        starting: std::collections::VecDeque::new(),
    }
}

fn flatten(services: &[ServiceObservation]) -> Vec<ContainerObservation> {
    services
        .iter()
        .flat_map(ServiceObservation::members)
        .map(|container| container.as_observation().clone())
        .collect()
}

impl CatchUpClient for FakeCatchUpClient {
    async fn live_services(&mut self) -> Result<Observed, Failure> {
        self.listings += 1;
        let (services, failures, omissions) = match &self.relisted {
            Some(relisted) if self.listings > 1 => (relisted, Vec::new(), Vec::new()),
            _ => (
                &self.services,
                self.failures.clone(),
                self.omissions.clone(),
            ),
        };
        let live = LiveServices {
            containers: ployz_core::PartialResult {
                successes: vec![ployz_core::MachineSuccess {
                    machine_id: self.machine_id,
                    value: flatten(services),
                }],
                failures,
                omissions,
            },
        };
        Ok(Observed {
            live,
            names: self.names.clone(),
            unarrived: self.unarrived.clone(),
        })
    }

    async fn bridge_capacity(
        &mut self,
        _machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure> {
        panic!("orchestration delegates capacity checks to run_slot")
    }

    async fn retire_if_ineligible(
        &mut self,
        machine_id: &MachineId,
        _new: &NewContainer,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<bool, RpcError> {
        assert_eq!(machine_id, &self.machine_id);
        Ok(matches!(self.run_result, Ok(None)))
    }

    async fn run_slot(
        &mut self,
        machine_id: &MachineId,
        new: NewContainer,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Option<Copy>, RpcError> {
        assert_eq!(machine_id, &self.machine_id);
        let service =
            QualifiedService::new(new.slot.namespace.clone(), new.resolved_spec.name.clone());
        self.run_calls.push(new);
        Ok(self.run_result.clone()?.map(|container_id| Copy {
            machine_id: self.machine_id,
            service,
            container_id,
            rotation: CopyRotation::Pending,
        }))
    }

    async fn bring_in(
        &mut self,
        copy: &Copy,
        _spec: &ResolvedServiceSpec,
    ) -> Result<BroughtIn, RpcError> {
        self.brought_in.push(copy.clone());
        Ok(match &self.bring_in {
            FakeBringIn::In => BroughtIn::In,
            FakeBringIn::Refused(reason) => BroughtIn::Refused(ContainerRefusal {
                reason: *reason,
                message: format!("owner refused In: {reason:?}"),
            }),
        })
    }

    async fn retire_copy(
        &mut self,
        copy: &Copy,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Step, RpcError> {
        self.retired.push(copy.clone());
        Ok(Step::Done)
    }

    async fn remove_copy(&mut self, copy: &Copy) -> Result<Step, RpcError> {
        self.removed.push(copy.clone());
        Ok(Step::Done)
    }

    async fn target_containers(
        &mut self,
        _machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure> {
        if let Some(starting) = self.starting.pop_front() {
            return Ok(flatten(&starting));
        }
        Ok(flatten(
            self.target_services.as_ref().unwrap_or(&self.services),
        ))
    }
}

fn provisioned_global_spec() -> RequestedServiceSpec {
    let mut spec = requested(ServiceMode::Global);
    let reference = ServiceVolumeReference::parse("data").unwrap();
    spec.set_volume_graph(
        ServiceVolumeGraph::parse(
            vec![ServiceVolume {
                reference: reference.clone(),
                source: ployz_core::RawVolumeSource::Provisioned {
                    name: DockerVolumeName::parse("data").unwrap(),
                    maximum_bytes: ProvisionedVolumeMaximumBytes::new(
                        NonZeroU64::new(100).unwrap(),
                    ),
                    labels: Default::default(),
                }
                .admit()
                .expect("valid volume declaration"),
            }],
            vec![ServiceMount {
                volume: reference,
                target: ContainerPath::parse("/data").unwrap(),
                read_only: false,
                no_copy: false,
                subpath: None,
            }],
        )
        .unwrap()
        .scope_to_namespace(&ployz_core::Namespace::parse("app").unwrap())
        .unwrap(),
    )
    .unwrap();
    spec
}

#[tokio::test(start_paused = true)]
async fn failed_placement_is_reported_even_if_final_observation_is_running() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let mut client = fake(
        &joiner,
        vec![global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&founder, 'a'),
        )],
    );
    client.target_services = Some(vec![global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&joiner, 'b'),
    )]);
    client.run_result = Err(RpcError {
        code: ployz_core::RpcErrorCode::Conflict,
        message: "creation key conflict".into(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    });
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert!(message(error, &joiner).contains("creation key conflict"));
}

#[path = "global_catch_up_rpc_tests.rs"]
mod rpc_tests;
