//! Read-only deploy planning contracts.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPreviewTarget {
    pub namespace_id: NamespaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<DeployOrigin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub volumes: BTreeMap<VolumeName, VolumeSpec>,
    pub services: Vec<DeployPreviewService>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPreviewService {
    pub service_id: ServiceId,
    pub image: DeployPreviewImage,
    pub replicas: ReplicaCount,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<ContainerRetentionCount>,
    pub runtime: ContainerRuntimeSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_start: Option<PreStartHook>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<ServiceDependency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<DeployRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployPreviewImage {
    Concrete {
        image: ImageReference,
        image_source: ImageSource,
    },
    PendingBuild,
}

/// A read-only placement projection. It deliberately omits revision identity
/// and commit semantics because pending builds change the authoritative target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPreviewProjection {
    pub namespace_id: NamespaceId,
    pub phases: Vec<DeployPhasePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_pins: Vec<VolumePinState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_preparations: Vec<VolumePinState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_candidates: Vec<DeployCleanupAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_binding_additions: Vec<DeployRouteBindingAddition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_binding_removals: Vec<RouteBindingState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serving_target_commits: Vec<ServingTargetEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serving_target_removals: Vec<ServingTargetEntry>,
}

impl DeployPreviewProjection {
    #[must_use]
    pub fn from_plan(
        plan: DeployPlacementPlan,
        route_binding_additions: Vec<DeployRouteBindingAddition>,
        route_binding_removals: Vec<RouteBindingState>,
        serving_target_commits: Vec<ServingTargetEntry>,
        serving_target_removals: Vec<ServingTargetEntry>,
    ) -> Self {
        let DeployPlacementPlan {
            namespace_id,
            phases,
            volume_pin_commits: volume_pins,
            volume_ensures: volume_preparations,
            cleanup_actions: cleanup_candidates,
        } = plan;
        Self {
            namespace_id,
            phases,
            volume_pins,
            volume_preparations,
            cleanup_candidates,
            route_binding_additions,
            route_binding_removals,
            serving_target_commits,
            serving_target_removals,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress::RouteBindingOrigin;
    use crate::operation::{RouteHostname, RoutePort, RouteTarget};

    #[test]
    fn pending_build_survives_preview_normalization_without_an_image_reference() {
        let service_id = ServiceId::try_new("api").expect("service id");
        let target = DeployPreviewTarget {
            namespace_id: NamespaceId::try_new("default").expect("namespace id"),
            origin: None,
            volumes: BTreeMap::new(),
            services: vec![DeployPreviewService {
                service_id: service_id.clone(),
                image: DeployPreviewImage::PendingBuild,
                replicas: ReplicaCount::try_new(1).expect("replicas"),
                keep: None,
                runtime: ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        };

        let [service] = target.services.as_slice() else {
            panic!("one preview service");
        };
        assert_eq!(service.service_id, service_id);
        assert_eq!(service.image, DeployPreviewImage::PendingBuild);
        let planning_target =
            DeployPlanningTarget::try_from_preview(&target).expect("preview target validates");
        let planning_service = planning_target
            .service(&service_id)
            .expect("planning service");
        assert_eq!(
            planning_service.namespace_revision_entry_id(planning_target.namespace_id()),
            None
        );
    }

    #[test]
    fn preview_normalization_rejects_an_invalid_internal_service_name() {
        let service_id = ServiceId::try_new("a".repeat(64)).expect("service id");
        let namespace_id = NamespaceId::try_new("default").expect("namespace id");
        let target = DeployPreviewTarget {
            namespace_id: namespace_id.clone(),
            origin: None,
            volumes: BTreeMap::new(),
            services: vec![DeployPreviewService {
                service_id: service_id.clone(),
                image: DeployPreviewImage::PendingBuild,
                replicas: ReplicaCount::try_new(1).expect("replicas"),
                keep: None,
                runtime: ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        };
        let error = DeployPlanningTarget::try_from_preview(&target)
            .expect_err("internal service name is invalid");

        assert_eq!(
            error,
            DeployTargetValidationError::InvalidInternalServiceName {
                service_id,
                namespace_id,
            }
        );
    }

    #[test]
    fn preview_projection_keeps_route_and_serving_changes() {
        let namespace_id = NamespaceId::try_new("default").expect("namespace id");
        let service_id = ServiceId::try_new("api").expect("service id");
        let binding = RouteBindingState {
            id: RouteBindingId::try_new("route_api").expect("route id"),
            namespace_id: namespace_id.clone(),
            target: RouteTarget::new(RouteHostname::try_new("api.example.com").expect("hostname")),
            endpoint_port: RoutePort::try_new(8080).expect("port"),
            service_id: service_id.clone(),
            origin: RouteBindingOrigin::Declared,
        };
        let addition = DeployRouteBindingAddition {
            namespace_id: namespace_id.clone(),
            target: binding.target.clone(),
            endpoint_port: binding.endpoint_port,
            service_id: service_id.clone(),
            origin: binding.origin,
        };
        let serving = ServingTargetEntry {
            namespace_id: namespace_id.clone(),
            service_id,
            namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("entry_api")
                .expect("entry id"),
            image: ImageReference::try_new("ghcr.io/acme/api:current").expect("image"),
            desired_replicas: ReplicaCount::try_new(1).expect("replicas"),
            volume_names: Vec::new(),
        };
        let plan = DeployPlacementPlan {
            namespace_id,
            phases: Vec::new(),
            volume_pin_commits: Vec::new(),
            volume_ensures: Vec::new(),
            cleanup_actions: Vec::new(),
        };

        let projection = DeployPreviewProjection::from_plan(
            plan,
            vec![addition.clone()],
            vec![binding.clone()],
            vec![serving.clone()],
            vec![serving.clone()],
        );

        assert_eq!(
            projection.route_binding_additions,
            std::slice::from_ref(&addition)
        );
        assert_eq!(projection.route_binding_removals, [binding]);
        assert_eq!(
            projection.serving_target_commits,
            std::slice::from_ref(&serving)
        );
        assert_eq!(projection.serving_target_removals, [serving]);
    }
}
