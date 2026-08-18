use super::support::*;
use ployz::dns::expand_ingress_ports;
use ployz_core::{
    DeployWarning, HttpProtocol, IngressHost, IngressHostname, PortPublication, ProjectName,
    QualifiedService, ServiceAttempt,
};

const DOMAIN: &str = "opaque.ployz.example";

#[test]
fn complete_snapshot_rejects_another_qualified_service_already_publishing_the_hostname() {
    let spec = expanded(custom_web());
    let snapshot = snapshot_with(vec![other_project_container(&spec, 1)]);
    let error = plan_deploy([&spec], &snapshot, PlanOptions::default()).unwrap_err();
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
    let spec = expanded(custom_web());
    let snapshot = DeploySnapshot {
        volume_omissions: vec![machine_id('1')],
        ..snapshot_with(vec![other_project_container(&spec, 1)])
    };
    assert!(!snapshot.is_observer_complete());
    let error = plan_deploy([&spec], &snapshot, PlanOptions::default()).unwrap_err();
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
    for spec in [
        expanded(custom_web()),
        expanded(assigned_web()),
        expanded(chosen_web("api")),
    ] {
        let existing = container('c', '1', &spec, &service_id('a'));
        let snapshot = snapshot_with(vec![existing]);
        let plan = plan_deploy([&spec], &snapshot, PlanOptions::default()).unwrap();
        assert!(plan.warnings.is_empty());
    }
}

#[test]
fn incomplete_snapshot_without_a_visible_publisher_warns_that_detection_is_observer_relative() {
    for spec in [
        expanded(custom_web()),
        expanded(assigned_web()),
        expanded(chosen_web("api")),
    ] {
        let snapshot = DeploySnapshot {
            volume_omissions: vec![machine_id('1')],
            ..snapshot_with(Vec::new())
        };
        assert!(!snapshot.is_observer_complete());
        let plan = plan_deploy([&spec], &snapshot, PlanOptions::default()).unwrap();
        assert_eq!(
            plan.warnings,
            vec![DeployWarning::ObserverRelativeHostnameConflict]
        );
        assert!(
            plan.operations
                .iter()
                .any(|operation| { matches!(operation, DeployOperation::RunContainer { .. }) })
        );
    }
}

#[test]
fn complete_snapshot_without_a_conflict_does_not_warn() {
    for spec in [
        expanded(custom_web()),
        expanded(assigned_web()),
        expanded(chosen_web("api")),
    ] {
        let plan =
            plan_deploy([&spec], &snapshot_with(Vec::new()), PlanOptions::default()).unwrap();
        assert!(plan.warnings.is_empty());
    }
}

#[test]
fn two_applied_specs_that_expand_to_the_same_hostname_conflict() {
    let mut api = chosen_web("shared");
    api.name = ServiceName::parse("api").unwrap();
    let web = chosen_web("shared");
    let api = expanded(api);
    let web = expanded(web);
    let error = plan_deploy(
        [&api, &web],
        &snapshot_with(Vec::new()),
        PlanOptions::default(),
    )
    .unwrap_err();
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
    let spec = expanded(assigned_web());
    let mut owner = other_project_container(&spec, 1);
    owner.resolved_spec.ports = spec.ports.clone();
    let error =
        plan_deploy([&spec], &snapshot_with(vec![owner]), PlanOptions::default()).unwrap_err();
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
    let web = expanded(chosen_web("shared"));
    let api = expanded(api);
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
    let plan = ployz::deploy::plan_deploy(&intent, &snapshot_with(Vec::new())).unwrap();
    assert!(plan.warnings.is_empty());
    assert_eq!(
        plan.operations
            .iter()
            .filter(|operation| matches!(operation, DeployOperation::RunContainer { .. }))
            .count(),
        1
    );
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

fn expanded(mut spec: RequestedServiceSpec) -> RequestedServiceSpec {
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
    observation.project_name = ProjectName::parse("blog").unwrap();
    observation.created_at_unix_nanos = created_at;
    observation
}

fn snapshot_with(containers: Vec<ContainerObservation>) -> DeploySnapshot {
    DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers,
        ..Default::default()
    }
}
