use super::support::*;
use ployz::deploy::{IngressContext, preview_deploy};
use ployz::dns::expand_ingress_ports;
use ployz_core::{
    DeployWarning, HttpProtocol, IngressHost, IngressHostname, PortPublication, ProjectName,
    QualifiedService, ServiceAttempt,
};

const DOMAIN: &str = "opaque.ployz.example";

fn plan_ingress<'a>(
    requested: impl IntoIterator<Item = &'a RequestedServiceSpec>,
    snapshot: &DeploySnapshot,
) -> Result<DeployPreview, PlanError> {
    preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            requested,
            PlanOptions::default(),
        ),
        snapshot,
        IngressContext {
            cluster_domain: Some(DOMAIN),
        },
    )
}

#[test]
fn complete_snapshot_rejects_another_qualified_service_already_publishing_the_hostname() {
    let spec = custom_web();
    let snapshot = snapshot_with(vec![other_project_container(&spec, 1)]);
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
        "hostname api.example.com is already published by blog/web"
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
        ..snapshot_with(vec![other_project_container(&spec, 1)])
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
    for spec in [custom_web(), assigned_web(), chosen_web("api")] {
        let existing = container('c', '1', &expanded_owner(&spec), &service_id('a'));
        let snapshot = snapshot_with(vec![existing]);
        let plan = plan_ingress([&spec], &snapshot).unwrap();
        assert!(plan.warnings.is_empty());
    }
}

#[test]
fn incomplete_snapshot_without_a_visible_publisher_warns_that_detection_is_observer_relative() {
    for spec in [custom_web(), assigned_web(), chosen_web("api")] {
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
            vec![DeployWarning::ObserverRelativeHostnameConflict]
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
    for spec in [custom_web(), assigned_web(), chosen_web("api")] {
        let plan = plan_ingress([&spec], &snapshot_with(Vec::new())).unwrap();
        assert!(plan.warnings.is_empty());
    }
}

#[test]
fn two_applied_specs_that_expand_to_the_same_hostname_conflict() {
    let mut api = chosen_web("shared");
    api.name = ServiceName::parse("api").unwrap();
    let web = chosen_web("shared");
    let error = plan_ingress([&api, &web], &snapshot_with(Vec::new())).unwrap_err();
    assert_eq!(
        error,
        PlanError::HostnameConflict {
            hostname: IngressHost::parse("shared.opaque.ployz.example").unwrap(),
            owner: QualifiedService::parse("app/api").unwrap(),
        }
    );
}

#[test]
fn visible_owner_of_an_expanded_automatic_hostname_conflicts() {
    let spec = assigned_web();
    let mut owner = other_project_container(&expanded_owner(&spec), 1);
    owner
        .try_update(|parts| parts.resolved_spec.ports = expanded_owner(&spec).ports)
        .unwrap();
    let error = plan_ingress([&spec], &snapshot_with(vec![owner])).unwrap_err();
    assert_eq!(
        error,
        PlanError::HostnameConflict {
            hostname: IngressHost::parse("web-app.opaque.ployz.example").unwrap(),
            owner: QualifiedService::parse("blog/web").unwrap(),
        }
    );
}

#[test]
fn unselected_target_spec_is_not_an_applied_conflict() {
    let mut api = chosen_web("shared");
    api.name = ServiceName::parse("api").unwrap();
    let web = chosen_web("shared");
    let intent = DeployIntent::new(
        ProjectName::parse("app").unwrap(),
        vec![api, web.clone()],
        PlanOptions {
            selected: vec![ServiceAttempt {
                name: web.name.clone(),
            }],
            ..PlanOptions::default()
        },
    );
    let plan = preview_deploy(
        &intent,
        &snapshot_with(Vec::new()),
        IngressContext {
            cluster_domain: Some(DOMAIN),
        },
    )
    .unwrap();
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
    let spec = assigned_web();
    let intent = DeployIntent::apply_one(
        ProjectName::parse("app").unwrap(),
        spec.clone(),
        PlanOptions::default(),
    );
    preview_deploy(
        &intent,
        &snapshot_with(Vec::new()),
        IngressContext {
            cluster_domain: Some(DOMAIN),
        },
    )
    .unwrap();
    assert_eq!(intent.target, [spec]);
}

#[test]
fn cluster_domain_without_a_reserved_domain_is_a_plan_error() {
    let error = plan_deploy(
        [&assigned_web()],
        &snapshot_with(Vec::new()),
        PlanOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(error, PlanError::DomainRequired(_)));
}

#[test]
fn generated_ingress_label_over_63_characters_is_a_plan_error() {
    let mut spec = assigned_web();
    spec.name = ServiceName::parse("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
    let intent = DeployIntent::apply_one(
        ProjectName::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        spec,
        PlanOptions::default(),
    );
    let error = preview_deploy(
        &intent,
        &snapshot_with(Vec::new()),
        IngressContext {
            cluster_domain: Some(DOMAIN),
        },
    )
    .unwrap_err();
    assert!(matches!(error, PlanError::GeneratedLabel(_)));
    assert_eq!(
        error.to_string(),
        "generated Ingress Hostname label \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" exceeds the 63-character DNS label limit; shorten the Service Name or Project Name, or supply a custom hostname"
    );
}

#[test]
fn unselected_cluster_domain_spec_does_not_require_a_domain() {
    let mut api = assigned_web();
    api.name = ServiceName::parse("api").unwrap();
    let mut web = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    web.name = ServiceName::parse("web").unwrap();
    let intent = DeployIntent::new(
        ProjectName::parse("app").unwrap(),
        vec![api, web.clone()],
        PlanOptions {
            selected: vec![ServiceAttempt {
                name: web.name.clone(),
            }],
            ..PlanOptions::default()
        },
    );
    preview_deploy(
        &intent,
        &snapshot_with(Vec::new()),
        IngressContext::default(),
    )
    .unwrap();
}

fn custom_web() -> RequestedServiceSpec {
    ingress_web(IngressHostname::explicit("api.example.com").unwrap())
}

fn assigned_web() -> RequestedServiceSpec {
    ingress_web(IngressHostname::cluster_domain())
}

fn chosen_web(label: &str) -> RequestedServiceSpec {
    ingress_web(IngressHostname::cluster_domain_label(label).unwrap())
}

fn expanded_owner(spec: &RequestedServiceSpec) -> RequestedServiceSpec {
    let mut spec = spec.clone();
    expand_ingress_ports(&mut spec, &ProjectName::parse("app").unwrap(), Some(DOMAIN)).unwrap();
    spec
}

fn ingress_web(hostname: IngressHostname) -> RequestedServiceSpec {
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

fn other_project_container(spec: &RequestedServiceSpec, created_at: i64) -> ContainerObservation {
    let mut observation = container('d', '1', spec, &service_id('b'));
    observation
        .try_update(|parts| {
            parts.project_name = ProjectName::parse("blog").unwrap();
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
