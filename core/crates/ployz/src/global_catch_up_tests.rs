//! Tests for bounded Global catch-up and target storage eligibility.

use ployz_core::error_chain;
use std::num::NonZeroU64;

use ployz_core::{
    Ciphertext, ContainerId, ContainerKind, ContainerObservation, ContainerPath, ContainerRefusal,
    ContainerRefusalReason, ContainerRuntimeObservation, DeploymentLogId, DockerVolumeName, Gen,
    HealthObservation, MachineId, MachineStorageObservation, Namespace, Placement,
    PrivateAccessSet, ProvisionedVolumeMaximumBytes, RequestedServiceSpec, ResolvedUpdateConfig,
    ServiceMode, ServiceMount, ServiceObservation, ServiceVolume, ServiceVolumeGraph,
    ServiceVolumeReference, labels,
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
async fn a_copy_that_never_runs_fails_after_the_wait_naming_what_it_is() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let api = |on| global_service(qualified("app", "api"), 'a', Placement::default(), on);
    let mut client = fake(&joiner, vec![api(running_on(&founder, 'a'))]);
    client.run_result = Ok(Some(container_id('b')));
    client.target_services = Some(vec![api(created_on(&joiner, 'b'))]);
    let began = tokio::time::Instant::now();
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert!(began.elapsed() >= std::time::Duration::from_secs(30));
    assert!(began.elapsed() < std::time::Duration::from_secs(32));
    let message = message(error, &joiner);
    assert!(
        message.contains(&format!(
            "copy {} is not running on this Machine after catch-up; it is created",
            container_id('b').as_str()
        )),
        "{message}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_copy_that_is_gone_fails_without_waiting() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let api = |on| global_service(qualified("app", "api"), 'a', Placement::default(), on);
    let mut client = fake(&joiner, vec![api(running_on(&founder, 'a'))]);
    client.run_result = Ok(Some(container_id('b')));
    client.target_services = Some(Vec::new());
    let began = tokio::time::Instant::now();
    let error = catch_up(&mut client, &joiner).await.unwrap_err();
    assert!(began.elapsed() < std::time::Duration::from_secs(1));
    assert!(message(error, &joiner).contains("; it is gone"));
}

#[tokio::test(start_paused = true)]
async fn a_cancel_while_waiting_reports_the_copy_still_starting_unresolved() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let api = |on| global_service(qualified("app", "api"), 'a', Placement::default(), on);
    let mut client = fake(&joiner, vec![api(running_on(&founder, 'a'))]);
    client.run_result = Ok(Some(container_id('b')));
    client.target_services = Some(vec![api(created_on(&joiner, 'b'))]);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancelling = cancel.clone();
    let stop = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        cancelling.cancel();
    });
    let began = tokio::time::Instant::now();
    let error = catch_up_globals(&mut client, &joiner, &cancel, |_| {})
        .await
        .unwrap_err();
    stop.await.unwrap();
    assert!(began.elapsed() < std::time::Duration::from_secs(6));
    assert!(error.cause.is_interrupted(), "{:?}", error.cause);
    assert_eq!(error.unresolved, [qualified("app", "api")]);
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

// Catches the newest-copy tie-break drifting: of two copies created in the same
// nanosecond, the larger Container ID says whether the Service is a Global.
#[test]
fn a_creation_tie_goes_to_the_larger_container_id_when_naming_a_global() {
    let founder = machine('f', "founder");
    let identity = qualified("app", "api");
    let global = |hex| {
        global_service(
            identity.clone(),
            'a',
            Placement::default(),
            running_on(&founder, hex),
        )
        .containers
        .remove(0)
        .into_observation()
    };
    let replicated = |hex| {
        let mut copy = global(hex);
        copy.try_update(|parts| {
            parts.resolved_spec.mode = ServiceMode::Replicated {
                replicas: std::num::NonZeroU32::MIN,
            };
        })
        .unwrap();
        copy
    };

    for (smaller, larger, named) in [
        (global('a'), replicated('b'), 0),
        (replicated('a'), global('b'), 1),
    ] {
        for copies in [
            [smaller.clone(), larger.clone()],
            [larger.clone(), smaller.clone()],
        ] {
            let services = ployz_core::derive_services(copies);
            assert_eq!(sources(&services, false, None).len(), named);
        }
    }
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
                .insert(labels::DEPLOYMENT_ID.to_owned(), deploy.to_string());
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

fn unconfirmed(ids: &[MachineId]) -> cleanup::Unconfirmed {
    cleanup::Unconfirmed {
        machines: ids.to_vec(),
        unanswered: Vec::new(),
    }
}

fn refusal(reason: ContainerRefusalReason) -> ContainerRefusal {
    ContainerRefusal {
        reason,
        message: format!("owner refused: {reason:?}"),
        capability: None,
    }
}

#[test]
fn a_step_reads_as_the_error_each_outcome_names() {
    let (one, two) = (machine('1', "one").id, machine('2', "two").id);
    step_error(Rotated::Done).unwrap();
    let refused = refusal(ContainerRefusalReason::LastIn);
    assert_eq!(
        step_error(Rotated::Refused(refused.clone())).unwrap_err(),
        refused.into_error()
    );
    let unconfirmed = step_error(Rotated::NotConfirmed(unconfirmed(&[one, two]))).unwrap_err();
    assert_eq!(unconfirmed.code, ployz_core::RpcErrorCode::Unavailable);
    assert_eq!(
        unconfirmed.message,
        format!("{one}, {two} did not confirm the copy left its routes")
    );
    let cancelled = step_error(Rotated::Cancelled).unwrap_err();
    assert_eq!(cancelled.code, ployz_core::RpcErrorCode::Internal);
    assert_eq!(cancelled.message, "Cancelled.");
}

/// A superseded copy that does not retire stays, and the Service is named with the reason
/// its outcome gave. Cancellation ends the retirement without a reason of its own.
#[tokio::test(start_paused = true)]
async fn a_superseded_copy_that_does_not_retire_names_why_it_stays() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let identity = qualified("app", "api");
    let stuck = machine('2', "stuck").id;
    type Outcome = (fn() -> Rotated, Option<String>);
    let outcomes: [Outcome; 4] = [
        (
            || Rotated::Refused(refusal(ContainerRefusalReason::LastIn)),
            Some("superseded copy {old} stays: owner refused: LastIn".into()),
        ),
        (
            || Rotated::NotConfirmed(unconfirmed(&[machine('2', "stuck").id])),
            Some(format!(
                "superseded copy {{old}} stays: {stuck} did not confirm it left the routes"
            )),
        ),
        (|| Rotated::Cancelled, None),
        (|| Rotated::Done, None),
    ];
    for (retire, stays) in outcomes {
        let service = global_service(
            identity.clone(),
            'a',
            Placement::default(),
            rotated(running_on(&founder, 'b'), CopyRotation::In),
        );
        let mut client = fake(&joiner, vec![service]);
        client.target_services = Some(vec![
            global_service(
                identity.clone(),
                'a',
                Placement::default(),
                running_on(&joiner, 'c'),
            ),
            global_service(
                identity.clone(),
                'a',
                Placement::default(),
                running_on(&joiner, 'a'),
            ),
        ]);
        client.run_result = Ok(Some(container_id('c')));
        client.retire_step = retire;
        let old = container_id('a');
        let result = catch_up(&mut client, &joiner).await;
        match stays {
            None => result.unwrap(),
            Some(stays) => {
                let error = result.unwrap_err();
                let text = message(error, &joiner);
                let stays = stays.replace("{old}", old.as_str());
                assert!(text.contains(&format!("app/api: {stays}")), "{text}");
            }
        }
    }
}

/// A started copy the owner refuses In is removed; when that removal does not finish, the
/// reason says the copy stays and how its removal ended.
#[tokio::test(start_paused = true)]
async fn a_refused_started_copy_whose_removal_does_not_finish_says_it_stays() {
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
    client.remove_step = || Removal::Refused(refusal(ContainerRefusalReason::InRotation));
    let text = message(catch_up(&mut client, &joiner).await.unwrap_err(), &joiner);
    let stays = format!(
        "{OUTDATED}; the started copy stays: Refused(ContainerRefusal {{ reason: InRotation, message: \"owner refused: InRotation\", capability: None }})"
    );
    assert!(text.contains(&stays), "{text}");
}

#[tokio::test(start_paused = true)]
async fn the_ingress_proxy_is_set_once_through_its_doors() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let held = rotated(
        running_on(&founder, 'a'),
        CopyRotation::Held(DeploymentLogId::parse("dep-a").unwrap()),
    );
    let caddy = global_service(
        QualifiedService::system_ingress(),
        'a',
        Placement::default(),
        held,
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
    assert!(
        client.run_calls.is_empty(),
        "no door identity reaches the ordinary copy path"
    );
    assert!(client.brought_in.is_empty());
    assert!(client.retired.is_empty());
    assert!(client.removed.is_empty());
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

// Catches the router reaching the ordinary copy path, which would run it on the joiner
// without its key file, or without any key at all.
#[tokio::test(start_paused = true)]
async fn a_joiner_gets_the_router_through_the_sealed_key_an_observed_one_carries() {
    let mut joiner = machine('1', "joiner");
    joiner.accepts_ingress = false;
    let founder = machine('f', "founder");
    let sealed = Ciphertext::from_bytes(vec![5; 48]);
    let mut client = fake(&joiner, vec![router_service(&founder, &sealed)]);

    catch_up(&mut client, &joiner).await.unwrap();

    assert_eq!(client.private_access_calls, [sealed]);
    assert!(
        client.run_calls.is_empty(),
        "the router never reaches the ordinary copy path"
    );
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

#[tokio::test(start_paused = true)]
async fn interruption_settles_started_work_and_only_reports_unresolved_globals() {
    for excluded in [false, true] {
        let mut joiner = machine('1', "joiner");
        // No Ingress Proxy work, so the two Globals are the only ones.
        joiner.accepts_ingress = false;
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
    door_calls: Vec<(ServiceId, ClusterDoors)>,
    doors_result: Result<Option<IngressDoorsSet>, RpcError>,
    private_access_calls: Vec<Ciphertext>,
    private_access_result: Result<PrivateAccessSet, RpcError>,
    bring_in: FakeBringIn,
    brought_in: Vec<Copy>,
    retired: Vec<Copy>,
    removed: Vec<Copy>,
    retire_step: fn() -> Rotated,
    remove_step: fn() -> Removal,
    failures: Vec<ployz_core::MachineFailure<RpcError>>,
    omissions: Vec<MachineId>,
    names: crate::drain::retirement::Names,
    unarrived: crate::drain::retirement::Names,
    declared: Option<ployz_core::IngressDoors>,
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
        door_calls: Vec::new(),
        doors_result: Ok(Some(doors_on(joiner))),
        private_access_calls: Vec::new(),
        private_access_result: Ok(router_on(joiner)),
        bring_in: FakeBringIn::In,
        brought_in: Vec::new(),
        retired: Vec::new(),
        removed: Vec::new(),
        retire_step: || Rotated::Done,
        remove_step: || Removal::Removed,
        failures: Vec::new(),
        omissions: Vec::new(),
        names: Vec::new(),
        unarrived: Vec::new(),
        declared: None,
        relisted: None,
        listings: 0,
        starting: std::collections::VecDeque::new(),
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
            declared: self.declared.clone(),
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
                capability: None,
            }),
        })
    }

    async fn retire_copy(
        &mut self,
        copy: &Copy,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Rotated, RpcError> {
        self.retired.push(copy.clone());
        Ok((self.retire_step)())
    }

    async fn remove_copy(&mut self, copy: &Copy) -> Result<Removal, RpcError> {
        self.removed.push(copy.clone());
        Ok((self.remove_step)())
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
