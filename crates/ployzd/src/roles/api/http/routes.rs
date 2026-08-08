//! Route Binding HTTP and deploy integration.

mod adjudication;
mod allocation;
mod deploy_binding;
mod error;
mod http_attach;

pub(super) use deploy_binding::{CorrosionDeployRouteBindings, DeployRouteBindings};
#[cfg(test)]
pub(super) use error::AutomaticRouteBindingError;
pub(super) use http_attach::handle_attach;

#[cfg(test)]
mod tests {
    use ployz_core::RouteRemoveRequest;
    use ployz_core::corrosion::{
        CorrosionAutomaticRouteFailure, CorrosionDeployFailure, CorrosionDeployWarning,
    };
    use ployz_core::ids::{RouteBindingRowId, ServiceRowId};
    use ployz_core::operation::RouteHostname;

    use super::AutomaticRouteBindingError;

    #[test]
    fn collision_failure_names_the_exact_route_removal() {
        let route_id = RouteBindingRowId::try_new("01J00000000000000000000011").expect("route");
        let service_id = ServiceRowId::try_new("01J00000000000000000000013").expect("service");
        let hostname = RouteHostname::try_new("api.example.com").expect("hostname");

        let failure = AutomaticRouteBindingError::Collision {
            hostname: hostname.clone(),
            route_id: route_id.clone(),
        }
        .into_deploy_failure(service_id.clone());

        assert_eq!(
            failure,
            CorrosionDeployFailure::AutomaticRoute {
                service_id,
                failure: CorrosionAutomaticRouteFailure::HostnameCollision {
                    hostname: hostname.clone(),
                    route_id: route_id.clone(),
                    remove: RouteRemoveRequest {
                        hostname,
                        route_id: Some(route_id),
                    },
                },
            }
        );
    }

    #[test]
    fn committed_collision_is_warning_evidence() {
        let route_id = RouteBindingRowId::try_new("01J00000000000000000000011").expect("route");
        let service_id = ServiceRowId::try_new("01J00000000000000000000013").expect("service");
        let hostname = RouteHostname::try_new("api.example.com").expect("hostname");

        let warning = AutomaticRouteBindingError::Collision {
            hostname: hostname.clone(),
            route_id: route_id.clone(),
        }
        .into_deploy_warning(service_id.clone());

        assert_eq!(
            warning,
            CorrosionDeployWarning::AutomaticRouteActivation {
                service_id,
                failure: CorrosionAutomaticRouteFailure::HostnameCollision {
                    hostname: hostname.clone(),
                    route_id: route_id.clone(),
                    remove: RouteRemoveRequest {
                        hostname,
                        route_id: Some(route_id),
                    },
                },
            }
        );
    }
}
