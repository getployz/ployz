use super::*;

pub(super) fn select_standalone_service(
    request: &ployz_core::deploy::DeployRequest,
    selector: Option<&str>,
) -> Result<ServiceId, PloyzctlExecutionError> {
    let services = request
        .services
        .iter()
        .filter(|service| selector.is_none_or(|value| value == service.service_id.as_str()))
        .map(|service| service.service_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    match services.len() {
        0 => Err(current_tree_error(
            "no matching service exists in deploy history",
        )),
        1 => Ok(services.into_iter().next().expect("one service")),
        count => Err(current_tree_error(format!(
            "standalone service selection is ambiguous ({count} matches); pass --service"
        ))),
    }
}

pub(super) fn validate_standalone_template(
    request: &ployz_core::deploy::DeployRequest,
    live_services: impl IntoIterator<Item = ServingTargetEntry>,
    automatic_hostname_configuration: &AutomaticHostnameConfiguration,
    live_routes: &[RouteBindingState],
) -> Result<(), PloyzctlExecutionError> {
    let mut expected_services = request
        .services
        .iter()
        .map(|service| {
            (
                service.service_id.clone(),
                service.namespace_revision_entry_id(&request.namespace_id),
            )
        })
        .collect::<Vec<_>>();
    expected_services.sort();
    let mut actual_services = live_services
        .into_iter()
        .filter(|service| service.namespace_id == request.namespace_id)
        .map(|service| (service.service_id, service.namespace_revision_entry_id))
        .collect::<Vec<_>>();
    actual_services.sort();
    if expected_services != actual_services {
        return Err(current_tree_error(
            "newest successful deploy no longer matches the active namespace service set",
        ));
    }

    let target = DeployPlanningTarget::try_from_deploy(request).map_err(current_tree_error)?;
    let automatic_suffix = match automatic_hostname_configuration {
        AutomaticHostnameConfiguration::Disabled => None,
        AutomaticHostnameConfiguration::Ployz => Some(
            ployz_core::operation::RouteHostname::try_new(
                ployz_core::certificate::MANAGED_LEASE_DOMAIN_SUFFIX,
            )
            .map_err(current_tree_error)?,
        ),
        AutomaticHostnameConfiguration::Custom { suffix } => Some(suffix.as_hostname().clone()),
    };
    let additions = validate_deploy_route_bindings(&target, automatic_suffix.as_ref(), &[])
        .map_err(current_tree_error)?;
    let mut prospective_id = 0_u64;
    let expected_routes = commit_deploy_route_bindings(additions, &[], |_| {
        prospective_id += 1;
        RouteBindingId::try_new(format!("prospective_{prospective_id}"))
            .expect("generated route binding id is valid")
    });
    let mut expected_routes = expected_routes
        .into_iter()
        .map(|route| route_shape(&route))
        .collect::<Vec<_>>();
    expected_routes.sort();
    let mut actual_routes = live_routes
        .iter()
        .filter(|route| route.namespace_id == request.namespace_id)
        .map(route_shape)
        .collect::<Vec<_>>();
    actual_routes.sort();
    if expected_routes != actual_routes {
        return Err(current_tree_error(
            "newest successful deploy no longer matches active namespace route bindings",
        ));
    }
    Ok(())
}

fn route_shape(
    route: &RouteBindingState,
) -> (
    ployz_core::operation::RouteTarget,
    ployz_core::operation::RoutePort,
    ServiceId,
    u8,
) {
    (
        route.target.clone(),
        route.endpoint_port,
        route.service_id.clone(),
        match route.origin {
            ployz_core::ingress::RouteBindingOrigin::Declared => 0,
            ployz_core::ingress::RouteBindingOrigin::Automatic => 1,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{
        ContainerRuntimeSpec, DeployRequest, DeployServiceSpec, ImageReference, ImageSource,
        ReplicaCount, ServiceMode,
    };
    use ployz_core::ids::NamespaceRevisionEntryId;
    use ployz_core::ingress::RouteBindingOrigin;
    use ployz_core::operation::{RouteHostname, RoutePort, RouteTarget};

    fn request(service_ids: &[&str]) -> DeployRequest {
        DeployRequest {
            namespace_id: NamespaceId::try_new("production").expect("namespace"),
            origin: None,
            volumes: std::collections::BTreeMap::new(),
            services: service_ids
                .iter()
                .map(|service_id| DeployServiceSpec {
                    service_id: ServiceId::try_new(*service_id).expect("service"),
                    image: ImageReference::try_new(format!("ghcr.io/acme/{service_id}:current"))
                        .expect("image"),
                    image_source: ImageSource::Registry,
                    mode: ServiceMode::Replicated {
                        replicas: ReplicaCount::try_new(1).expect("replicas"),
                    },
                    keep: None,
                    runtime: ContainerRuntimeSpec::image_defaults(),
                    pre_start: None,
                    depends_on: Vec::new(),
                    routes: Vec::new(),
                })
                .collect(),
        }
    }

    fn live_service(request: &DeployRequest, index: usize) -> ServingTargetEntry {
        let service = request.services.get(index).expect("service index");
        ServingTargetEntry {
            namespace_id: request.namespace_id.clone(),
            service_id: service.service_id.clone(),
            namespace_revision_entry_id: service.namespace_revision_entry_id(&request.namespace_id),
            image: service.image.clone(),
            mode: service.mode,
            volume_names: Vec::new(),
        }
    }

    #[test]
    fn exact_active_projection_accepts_the_newest_template() {
        let request = request(&["api", "worker"]);
        let live = [live_service(&request, 0), live_service(&request, 1)];

        validate_standalone_template(
            &request,
            live,
            &AutomaticHostnameConfiguration::Disabled,
            &[],
        )
        .expect("exact projection");
    }

    #[test]
    fn changed_entry_identity_rejects_stale_history() {
        let request = request(&["api"]);
        let mut live = live_service(&request, 0);
        live.namespace_revision_entry_id =
            NamespaceRevisionEntryId::try_new("entry_stale").expect("entry id");

        let error = validate_standalone_template(
            &request,
            [live],
            &AutomaticHostnameConfiguration::Disabled,
            &[],
        )
        .expect_err("stale entry rejected");
        assert!(error.to_string().contains("active namespace service set"));
    }

    #[test]
    fn extra_active_service_rejects_stale_history() {
        let target = request(&["api"]);
        let newer = request(&["api", "worker"]);
        let live = [live_service(&newer, 0), live_service(&newer, 1)];

        let error = validate_standalone_template(
            &target,
            live,
            &AutomaticHostnameConfiguration::Disabled,
            &[],
        )
        .expect_err("extra service rejected");
        assert!(error.to_string().contains("active namespace service set"));
    }

    #[test]
    fn extra_active_route_rejects_stale_history() {
        let target = request(&["api"]);
        let live_service = live_service(&target, 0);
        let live_route = RouteBindingState {
            id: RouteBindingId::try_new("route_old").expect("route id"),
            namespace_id: target.namespace_id.clone(),
            target: RouteTarget::new(RouteHostname::try_new("old.example.com").expect("hostname")),
            endpoint_port: RoutePort::try_new(8080).expect("port"),
            service_id: ServiceId::try_new("api").expect("service"),
            origin: RouteBindingOrigin::Declared,
        };

        let error = validate_standalone_template(
            &target,
            [live_service],
            &AutomaticHostnameConfiguration::Disabled,
            &[live_route],
        )
        .expect_err("extra route rejected");
        assert!(
            error
                .to_string()
                .contains("active namespace route bindings")
        );
    }
}
