use super::support::*;
use ployz::deploy::preview_deploy;
use ployz_core::{
    DeployWarning, HttpProtocol, IngressHost, PortPublication, ProjectName, QualifiedService,
    ServiceAttempt,
};

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
            "projectName": "app", "selected": [], "snapshots": [{
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
        ProjectName::parse("app").unwrap(),
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
fn retiring_hostname_winner_does_not_hide_another_projects_claim() {
    let previous = custom_web();
    let mut renamed = previous.clone();
    renamed.name = ServiceName::parse("renamed").unwrap();
    let snapshot = snapshot_with(vec![
        container('c', '1', &previous, &service_id('a')),
        other_project_container(&previous, 1),
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
        ProjectName::parse("app").unwrap(),
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
        ProjectName::parse("app").unwrap(),
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
