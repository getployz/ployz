use super::support::*;
use ployz::deploy::preview_deploy;
use ployz_core::{
    DeployWarning, HttpProtocol, IngressHost, Namespace, PortPublication, QualifiedService,
    ServiceAttempt,
};

fn plan_ingress<'a>(
    requested: impl IntoIterator<Item = &'a RequestedServiceSpec>,
    snapshot: &DeploySnapshot,
) -> Result<DeployPreview, PlanError> {
    preview_deploy(
        &DeployIntent::apply_all(
            Namespace::parse("app").unwrap(),
            requested,
            PlanOptions::default(),
        ),
        snapshot,
    )
}

#[test]
fn complete_snapshot_rejects_another_qualified_service_already_publishing_the_hostname() {
    let spec = custom_web();
    let snapshot = snapshot_with(vec![other_namespace_container(&spec, 1)]);
    let error = plan_ingress([&spec], &snapshot).unwrap_err();
    assert_eq!(
        error,
        PlanError::HostnameConflict {
            hostname: IngressHost::parse("api.example.com").unwrap(),
            owner: QualifiedService::parse("blog/web").unwrap(),
        }
    );
    assert_eq!(
        error.to_string(),
        "hostname api.example.com is already published by blog/web: remove it from that Service first, or use another hostname"
    );
}

#[test]
fn visible_conflict_rejects_even_when_the_snapshot_is_incomplete() {
    let spec = custom_web();
    let snapshot = DeploySnapshot {
        volume_snapshot: VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![machine_id('1')],
        )
        .expect("valid Volume Snapshot fixture"),
        ..snapshot_with(vec![other_namespace_container(&spec, 1)])
    };
    assert!(!snapshot.is_observer_complete());
    let error = plan_ingress([&spec], &snapshot).unwrap_err();
    assert_eq!(
        error,
        PlanError::HostnameConflict {
            hostname: IngressHost::parse("api.example.com").unwrap(),
            owner: QualifiedService::parse("blog/web").unwrap(),
        }
    );
}

#[test]
fn same_qualified_service_redeploy_keeps_the_hostname() {
    for spec in [custom_web(), named_web("web.example.com")] {
        let existing = container('c', '1', &spec, &service_id('a'));
        let snapshot = snapshot_with(vec![existing]);
        let plan = plan_ingress([&spec], &snapshot).unwrap();
        assert!(plan.warnings.is_empty());
    }
}

#[test]
fn full_deploy_renames_service_without_losing_its_hostname() {
    let lower = |name: &str, retries: u8| {
        let input = serde_json::from_value(serde_json::json!({
            "namespace": "app", "selected": [], "snapshots": [{
                "serviceId": "same-cloud-service",
                "config": {
                    "version": 2, "privateDns": name,
                    "source": {"version": 1, "type": "image", "image": "nginx:stable", "credentials": {"type": "none"}},
                    "healthcheck": {"type": "none"}, "restartPolicy": "on-failure", "maxRetries": retries,
                    "routes": [{"id": "00000000-0000-4000-8000-000000000001", "hostname": "api.example.com", "targetPort": 80}]
                }
            }]
        })).unwrap();
        ployz_core::config::lower_deployment(input).unwrap()
    };
    let previous = lower("ployz2", 10).target.remove(0);
    let existing = container('c', '1', &previous, &service_id('a'));
    let renamed = lower("ployz", 5);
    let plan = preview_deploy(&renamed, &snapshot_with(vec![existing]))
        .expect("a full deploy should transfer the hostname from the service it replaces");
    assert!(plan.warnings.is_empty());
    assert!(matches!(operations(&plan).as_slice(), [
        DeployOperation::RunContainer { spec, .. },
        DeployOperation::RemoveContainer { container_id: removed, .. },
    ] if spec.name.as_str() == "ployz" && *removed == container_id('c')));
}

#[test]
fn rename_cannot_take_hostname_when_old_service_will_remain() {
    let previous = custom_web();
    let mut renamed = previous.clone();
    renamed.name = ServiceName::parse("renamed").unwrap();
    let snapshot = snapshot_with(vec![container('c', '1', &previous, &service_id('a'))]);
    let full = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&renamed],
        PlanOptions::default(),
    );
    let mut partial = full.clone();
    partial.options.selected = vec![ServiceAttempt {
        name: renamed.name.clone(),
    }];
    let mut retained = full.clone();
    retained.target.push(previous);
    let incomplete = DeploySnapshot {
        container_omissions: vec![machine_id('1')],
        ..snapshot.clone()
    };
    for (intent, observed) in [
        (&partial, &snapshot),
        (&retained, &snapshot),
        (&full, &incomplete),
    ] {
        assert!(matches!(
            preview_deploy(intent, observed),
            Err(PlanError::HostnameConflict { .. })
        ));
    }
}

#[test]
fn retiring_hostname_winner_does_not_hide_another_namespaces_claim() {
    let previous = custom_web();
    let mut renamed = previous.clone();
    renamed.name = ServiceName::parse("renamed").unwrap();
    let snapshot = snapshot_with(vec![
        container('c', '1', &previous, &service_id('a')),
        other_namespace_container(&previous, 1),
    ]);
    assert_eq!(
        plan_ingress([&renamed], &snapshot).unwrap_err(),
        PlanError::HostnameConflict {
            hostname: IngressHost::parse("api.example.com").unwrap(),
            owner: QualifiedService::parse("blog/web").unwrap(),
        }
    );
}

#[test]
fn one_deploy_moves_a_hostname_from_the_service_that_drops_it() {
    let (web, api) = handover_specs();
    let snapshot = snapshot_with(vec![container('c', '1', &custom_web(), &service_id('a'))]);
    let plan = plan_ingress([&web, &api], &snapshot)
        .expect("the Service that publishes the hostname drops it in this Deploy");
    assert!(plan.warnings.is_empty());
    assert_eq!(
        plan.dependencies,
        vec![(api.name.clone(), web.name.clone())],
        "the Service gaining the hostname is published before the one dropping it"
    );
    let gainer_starts = first_operation(&plan, "api");
    let dropper_replaces = first_operation(&plan, "web");
    assert!(gainer_starts < dropper_replaces, "{:?}", operations(&plan));
}

#[test]
fn a_service_that_keeps_its_hostname_still_blocks_another_in_the_same_deploy() {
    let web = custom_web();
    let mut api = custom_web();
    api.name = ServiceName::parse("api").unwrap();
    let snapshot = snapshot_with(vec![container('c', '1', &web, &service_id('a'))]);
    assert_eq!(
        plan_ingress([&web, &api], &snapshot).unwrap_err(),
        PlanError::HostnameConflict {
            hostname: IngressHost::parse("api.example.com").unwrap(),
            owner: QualifiedService::parse("app/web").unwrap(),
        }
    );
}

#[test]
fn a_handover_to_a_service_that_depends_on_the_dropper_is_refused() {
    let (web, api) = handover_specs();
    let snapshot = snapshot_with(vec![container('c', '1', &custom_web(), &service_id('a'))]);
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&web, &api],
        PlanOptions::default(),
    )
    .with_dependencies(BTreeMap::from([(
        api.name.clone(),
        vec![ployz_core::ServiceDependency {
            service: web.name.clone(),
            condition: ployz_core::DependencyCondition::ServiceStarted,
        }],
    )]));
    let error = preview_deploy(&intent, &snapshot)
        .expect_err("web would drop the hostname for api's whole rollout");
    assert_eq!(
        error.to_string(),
        "hostname api.example.com is published by app/web and can't move to api in the same Deploy: api depends on web; remove it from web and deploy, then add it to api"
    );
}

#[test]
fn two_services_swapping_hostnames_are_refused() {
    let web_today = named_web("web.example.com");
    let mut api_today = custom_web();
    api_today.name = ServiceName::parse("api").unwrap();
    let web = custom_web();
    let mut api = named_web("web.example.com");
    api.name = ServiceName::parse("api").unwrap();
    let snapshot = snapshot_with(vec![
        container('c', '1', &web_today, &service_id('a')),
        container('d', '1', &api_today, &service_id('b')),
    ]);
    let error = plan_ingress([&web, &api], &snapshot)
        .expect_err("a swap can order only one of its two handovers");
    assert_eq!(
        error.to_string(),
        "hostname api.example.com is published by app/api and can't move to web in the same Deploy: web hands web.example.com to api; remove it from api and deploy, then add it to web"
    );
}

#[test]
fn a_named_deploy_of_the_gainer_alone_says_to_deploy_the_dropper_too() {
    let (web, api) = handover_specs();
    let snapshot = snapshot_with(vec![container('c', '1', &custom_web(), &service_id('a'))]);
    let mut options = PlanOptions::default();
    options.selected = vec![ServiceAttempt {
        name: api.name.clone(),
    }];
    let intent = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        vec![web.clone(), api.clone()],
        options,
    );
    let error = preview_deploy(&intent, &snapshot).unwrap_err();
    assert_eq!(
        error.to_string(),
        "hostname api.example.com is published by app/web until web deploys: deploy web too"
    );
}

#[test]
fn a_retiring_older_publisher_does_not_hide_a_handover() {
    let (web, api) = handover_specs();
    let mut old = custom_web();
    old.name = ServiceName::parse("old").unwrap();
    let mut newer = container('c', '1', &custom_web(), &service_id('a'));
    newer
        .try_update(|parts| parts.created_at_unix_nanos = 1)
        .unwrap();
    let snapshot = snapshot_with(vec![container('e', '1', &old, &service_id('c')), newer]);
    let plan = plan_ingress([&web, &api], &snapshot)
        .expect("old retires with this Deploy, so web is the publisher that hands the hostname on");
    assert_eq!(
        plan.dependencies,
        vec![(api.name.clone(), web.name.clone())]
    );
}

#[test]
fn a_handover_does_not_hide_another_namespaces_claim() {
    let (web, api) = handover_specs();
    let snapshot = snapshot_with(vec![
        container('c', '1', &custom_web(), &service_id('a')),
        other_namespace_container(&custom_web(), 1),
    ]);
    assert_eq!(
        plan_ingress([&web, &api], &snapshot).unwrap_err(),
        PlanError::HostnameConflict {
            hostname: IngressHost::parse("api.example.com").unwrap(),
            owner: QualifiedService::parse("blog/web").unwrap(),
        }
    );
}

/// `web` publishes `api.example.com` today and drops it; `api` takes it.
fn handover_specs() -> (RequestedServiceSpec, RequestedServiceSpec) {
    let mut web = custom_web();
    web.ports.clear();
    let mut api = custom_web();
    api.name = ServiceName::parse("api").unwrap();
    (web, api)
}

fn first_operation(plan: &DeployPreview, service: &str) -> usize {
    plan.operations
        .iter()
        .position(|row| {
            row.service_name
                .as_ref()
                .is_some_and(|name| name.as_str() == service)
        })
        .unwrap_or_else(|| panic!("{service} has an operation in {:?}", operations(plan)))
}

#[test]
fn incomplete_snapshot_without_a_visible_publisher_warns_that_detection_is_observer_relative() {
    for spec in [custom_web(), named_web("web.example.com")] {
        let snapshot = DeploySnapshot {
            volume_snapshot: VolumeSnapshot::try_from_parts(
                Vec::new(),
                Vec::new(),
                Vec::new(),
                vec![machine_id('1')],
            )
            .expect("valid Volume Snapshot fixture"),
            ..snapshot_with(Vec::new())
        };
        assert!(!snapshot.is_observer_complete());
        let plan = plan_ingress([&spec], &snapshot).unwrap();
        assert_eq!(
            plan.warnings,
            vec![
                DeployWarning::ObservationOmitted {
                    kind: ployz_core::ObservationKind::Volume,
                    machine_id: machine_id('1'),
                    gap: None,
                },
                DeployWarning::ObserverRelativeHostnameConflict,
            ]
        );
        assert!(
            plan.operations
                .iter()
                .any(|row| { matches!(row.operation, DeployOperation::RunContainer { .. }) })
        );
    }
}

#[test]
fn complete_snapshot_without_a_conflict_does_not_warn() {
    for spec in [custom_web(), named_web("web.example.com")] {
        let plan = plan_ingress([&spec], &snapshot_with(Vec::new())).unwrap();
        assert!(plan.warnings.is_empty());
    }
}

#[test]
fn two_applied_specs_with_the_same_hostname_conflict() {
    let mut api = named_web("shared.example.com");
    api.name = ServiceName::parse("api").unwrap();
    let web = named_web("shared.example.com");
    let error = plan_ingress([&api, &web], &snapshot_with(Vec::new())).unwrap_err();
    assert_eq!(
        error,
        PlanError::HostnameConflict {
            hostname: IngressHost::parse("shared.example.com").unwrap(),
            owner: QualifiedService::parse("app/api").unwrap(),
        }
    );
}

#[test]
fn unselected_target_spec_is_not_an_applied_conflict() {
    let mut api = named_web("shared.example.com");
    api.name = ServiceName::parse("api").unwrap();
    let web = named_web("shared.example.com");
    let intent = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        vec![api, web.clone()],
        PlanOptions {
            selected: vec![ServiceAttempt { name: web.name }],
            ..PlanOptions::default()
        },
    );
    let plan = preview_deploy(&intent, &snapshot_with(Vec::new())).unwrap();
    assert!(plan.warnings.is_empty());
    assert_eq!(
        plan.operations
            .iter()
            .filter(|row| matches!(row.operation, DeployOperation::RunContainer { .. }))
            .count(),
        1
    );
}

#[test]
fn preview_does_not_mutate_the_intent() {
    let spec = custom_web();
    let intent = DeployIntent::apply_one(
        Namespace::parse("app").unwrap(),
        spec.clone(),
        PlanOptions::default(),
    );
    preview_deploy(&intent, &snapshot_with(Vec::new())).unwrap();
    assert_eq!(intent.target, [spec]);
}

fn custom_web() -> RequestedServiceSpec {
    ingress_web(IngressHost::parse("api.example.com").unwrap())
}

fn named_web(hostname: &str) -> RequestedServiceSpec {
    ingress_web(IngressHost::parse(hostname).unwrap())
}

fn ingress_web(hostname: IngressHost) -> RequestedServiceSpec {
    let mut spec = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    spec.name = ServiceName::parse("web").unwrap();
    spec.ports = vec![PortPublication::Ingress {
        hostname,
        load_balancer_port: NonZeroU16::new(80).unwrap(),
        container_port: NonZeroU16::new(80).unwrap(),
        http_protocol: HttpProtocol::Http,
    }];
    spec
}

fn other_namespace_container(spec: &RequestedServiceSpec, created_at: i64) -> ContainerObservation {
    let mut observation = container('d', '1', spec, &service_id('b'));
    observation
        .try_update(|parts| {
            parts.namespace = Namespace::parse("blog").unwrap();
            parts.created_at_unix_nanos = created_at;
        })
        .unwrap();
    observation
}

fn snapshot_with(containers: Vec<ContainerObservation>) -> DeploySnapshot {
    DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers,
        ..Default::default()
    }
}
