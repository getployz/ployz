use super::support::*;
use ployz_core::{
    DependencyCondition, DeployWarning, MachineFailure, MembershipEvidence, ObservationGapReason,
    RpcError, RpcErrorCode, ServiceDependency, ServiceRemovals,
};

fn snapshot() -> DeploySnapshot {
    let mut down = machine('2', "unobserved");
    down.membership = MembershipObservation::Down;
    down.membership_evidence = Some(MembershipEvidence::Down);
    DeploySnapshot {
        machines: vec![machine('1', "available"), down],
        container_omissions: vec![machine_id('2')],
        ..Default::default()
    }
}

fn gaps(preview: &DeployPreview) -> Vec<(MachineId, ObservationGapReason)> {
    preview
        .warnings
        .iter()
        .filter_map(|warning| warning.observation_gap().map(|(id, gap)| (id, gap.reason)))
        .collect()
}

fn selected(name: &str) -> PlanOptions {
    PlanOptions {
        selected: vec![ServiceAttempt {
            name: ServiceName::parse(name).unwrap(),
        }],
        ..Default::default()
    }
}

#[test]
fn only_recognized_down_or_an_actual_failure_qualifies_a_relevant_gap() {
    let requested = spec("web");
    let snapshot = snapshot();
    let preview = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();
    assert_eq!(
        gaps(&preview),
        [(machine_id('2'), ObservationGapReason::Down)]
    );
    for evidence in [
        None,
        Some(MembershipEvidence::Unrecognized { raw: "down".into() }),
    ] {
        let mut legacy = snapshot.clone();
        legacy.machines.get_mut(1).unwrap().membership_evidence = evidence;
        let preview = plan_deploy([&requested], &legacy, PlanOptions::default()).unwrap();
        assert!(!preview.has_observation_gaps());
        assert!(!preview.warnings.is_empty());
    }
    let mut failed = snapshot;
    failed.machines.get_mut(1).unwrap().membership_evidence = None;
    failed.container_omissions.clear();
    failed.container_failures.push(MachineFailure {
        machine_id: machine_id('2'),
        error: RpcError {
            code: RpcErrorCode::Unavailable,
            message: "listing failed".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        },
    });
    assert_eq!(
        gaps(&plan_deploy([&requested], &failed, PlanOptions::default()).unwrap()),
        [(machine_id('2'), ObservationGapReason::Failed)]
    );
}

#[test]
fn qualification_uses_selected_work_and_its_dependency_expansion() {
    let mut web = spec("web");
    web.placement.constraints =
        [PlacementConstraint::parse(format!("node.id == {}", machine_id('1'))).unwrap()].into();
    let worker = spec("worker");
    let intent = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        vec![web.clone(), worker.clone()],
        selected("web"),
    );
    assert!(
        !preview_deploy(&intent, &snapshot())
            .unwrap()
            .has_observation_gaps()
    );
    let intent = intent.with_dependencies(BTreeMap::from([(
        web.name.clone(),
        vec![ServiceDependency {
            service: worker.name.clone(),
            condition: DependencyCondition::ServiceStarted,
        }],
    )]));
    assert!(
        preview_deploy(&intent, &snapshot())
            .unwrap()
            .has_observation_gaps()
    );
}

#[test]
fn acceptance_is_namespace_aware_and_independently_observed_work_still_counts() {
    let mut snapshot = snapshot();
    let peer = snapshot.machines.get_mut(1).unwrap();
    peer.machine.accepts_services = false;
    peer.machine.accepts_ingress = true;
    let ingress = ployz_core::caddy_service_spec("caddy:test".into(), Default::default());
    let app = DeployIntent::apply_one(
        Namespace::parse("app").unwrap(),
        ingress.clone(),
        PlanOptions::default(),
    );
    assert!(
        !preview_deploy(&app, &snapshot)
            .unwrap()
            .has_observation_gaps()
    );
    let system =
        DeployIntent::apply_one(Namespace::system(), ingress.clone(), PlanOptions::default());
    assert!(
        preview_deploy(&system, &snapshot)
            .unwrap()
            .has_observation_gaps()
    );
    snapshot
        .containers
        .push(container('a', '2', &ingress, &ServiceId::random()));
    assert!(
        preview_deploy(&app, &snapshot)
            .unwrap()
            .has_observation_gaps()
    );
    let mut other = snapshot.containers.pop().unwrap().into_parts();
    other.namespace = Namespace::parse("unrelated").unwrap();
    snapshot
        .containers
        .push(ContainerObservation::try_from(other).unwrap());
    assert!(
        !preview_deploy(&app, &snapshot)
            .unwrap()
            .has_observation_gaps()
    );
}

#[test]
fn unrelated_existing_work_and_converged_removal_do_not_invent_a_gap() {
    let mut snapshot = snapshot();
    let old = spec("old");
    let mut web = spec("web");
    web.placement.constraints =
        [PlacementConstraint::parse(format!("node.id == {}", machine_id('1'))).unwrap()].into();
    snapshot
        .containers
        .push(container('a', '2', &old, &ServiceId::random()));
    let selected = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        vec![web, old],
        selected("web"),
    );
    assert!(
        !preview_deploy(&selected, &snapshot)
            .unwrap()
            .has_observation_gaps()
    );
    let removal =
        DeployIntent::apply_all(Namespace::parse("app").unwrap(), [], PlanOptions::default());
    assert!(
        preview_deploy(&removal, &snapshot)
            .unwrap()
            .has_observation_gaps()
    );
    snapshot.containers.clear();
    assert!(
        !preview_deploy(&removal, &snapshot)
            .unwrap()
            .has_observation_gaps()
    );
    snapshot
        .machines
        .retain(|machine| machine.machine.id != machine_id('2'));
    assert!(
        !preview_deploy(&removal, &snapshot)
            .unwrap()
            .has_observation_gaps()
    );
}

/// A named removal must reach every copy of its Service, and a Server this Deploy
/// could not check may hold one it never saw. So that Server is a gap even when
/// the Deploy plans nothing: the report names it, and the next full Deploy prunes
/// whatever copy it holds.
#[test]
fn a_named_removal_makes_every_server_the_deploy_could_not_check_a_gap() {
    let removal = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        Vec::new(),
        PlanOptions {
            removals: ServiceRemovals::from_names([ServiceName::parse("rmme").unwrap()]),
            ..Default::default()
        },
    );
    let preview = preview_deploy(&removal, &snapshot()).unwrap();
    assert!(preview.operations.is_empty(), "{:?}", preview.operations);
    assert_eq!(
        gaps(&preview),
        [(machine_id('2'), ObservationGapReason::Down)]
    );
    assert!(
        preview.warnings.contains(&DeployWarning::MixedAfterReturn {
            machine: "unobserved".into(),
        }),
        "{:?}",
        preview.warnings
    );

    let unread = DeploySnapshot {
        machines: vec![machine('1', "available"), machine('2', "unread")],
        container_failures: vec![MachineFailure {
            machine_id: machine_id('2'),
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "listing failed".into(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            },
        }],
        ..Default::default()
    };
    let preview = preview_deploy(&removal, &unread).unwrap();
    assert!(preview.operations.is_empty(), "{:?}", preview.operations);
    assert_eq!(
        gaps(&preview),
        [(machine_id('2'), ObservationGapReason::Failed)]
    );
}

#[test]
fn old_warning_payloads_stay_generic_and_new_facts_roundtrip() {
    let old: DeployWarning = serde_json::from_value(serde_json::json!({
        "type":"observation_omitted", "kind":"container", "machine_id":machine_id('2'),
    }))
    .unwrap();
    assert!(old.observation_gap().is_none());
    let preview = plan_deploy([&spec("web")], &snapshot(), PlanOptions::default()).unwrap();
    let decoded: DeployPreview =
        serde_json::from_value(serde_json::to_value(&preview).unwrap()).unwrap();
    assert_eq!(gaps(&decoded), gaps(&preview));
    assert!(
        decoded
            .warnings
            .iter()
            .any(|warning| warning.to_string().contains("unobserved"))
    );
}

#[test]
fn a_machine_the_deploy_could_not_observe_is_named_as_one_that_can_serve_old_copies_on_return() {
    let requested = spec("web");
    let preview = plan_deploy([&requested], &snapshot(), PlanOptions::default()).unwrap();
    assert!(
        preview.warnings.contains(&DeployWarning::MixedAfterReturn {
            machine: "unobserved".into(),
        }),
        "{:?}",
        preview.warnings
    );

    let mut legacy = snapshot();
    legacy.machines.get_mut(1).unwrap().membership_evidence = None;
    let preview = plan_deploy([&requested], &legacy, PlanOptions::default()).unwrap();
    assert!(
        !preview
            .warnings
            .iter()
            .any(|warning| matches!(warning, DeployWarning::MixedAfterReturn { .. })),
        "only a qualified gap names the Machine: {:?}",
        preview.warnings
    );
}
