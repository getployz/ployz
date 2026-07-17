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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeDeclaredDeployPreviewTarget(DeployPreviewTarget);

impl VolumeDeclaredDeployPreviewTarget {
    pub fn try_new(target: DeployPreviewTarget) -> Result<Self, DeployTargetValidationError> {
        super::request::validate_deploy_target(
            &target.namespace_id,
            &target.volumes,
            target
                .services
                .iter()
                .map(|service| (&service.service_id, &service.runtime)),
        )?;
        Ok(Self(target))
    }

    #[must_use]
    pub fn namespace_id(&self) -> &NamespaceId {
        &self.0.namespace_id
    }

    #[must_use]
    pub fn target(&self) -> &DeployPreviewTarget {
        &self.0
    }

    #[must_use]
    pub fn services(&self) -> &[DeployPreviewService] {
        &self.0.services
    }

    #[must_use]
    pub fn status_service_id(&self) -> ServiceId {
        self.0
            .services
            .first()
            .map(|service| service.service_id.clone())
            .unwrap_or_else(|| {
                ServiceId::try_new(self.0.namespace_id.as_str().to_owned())
                    .expect("namespace id is a valid service id fallback")
            })
    }

    #[must_use]
    pub fn service(&self, service_id: &ServiceId) -> Option<&DeployPreviewService> {
        self.0
            .services
            .iter()
            .find(|service| service.service_id == *service_id)
    }

    pub fn pending_build_service_ids(&self) -> impl Iterator<Item = &ServiceId> {
        self.0.services.iter().filter_map(|service| {
            matches!(&service.image, DeployPreviewImage::PendingBuild)
                .then_some(&service.service_id)
        })
    }

    #[must_use]
    pub fn into_target(self) -> DeployPreviewTarget {
        self.0
    }
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
    pub route_binding_commits: Vec<RouteBindingState>,
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
        route_binding_commits: Vec<RouteBindingState>,
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
            route_binding_commits,
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
        let target = VolumeDeclaredDeployPreviewTarget::try_new(DeployPreviewTarget {
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
        })
        .expect("preview target validates");

        let [service] = target.services() else {
            panic!("one preview service");
        };
        assert_eq!(service.service_id, service_id);
        assert_eq!(service.image, DeployPreviewImage::PendingBuild);
        assert_eq!(
            target.pending_build_service_ids().collect::<Vec<_>>(),
            [&service_id]
        );
        let planning_service = DeployPlanningTarget::Preview(&target)
            .service(&service_id)
            .expect("planning service");
        assert_eq!(
            planning_service.namespace_revision_entry_id(target.namespace_id()),
            None
        );
    }

    #[test]
    fn preview_normalization_rejects_an_invalid_internal_service_name() {
        let service_id = ServiceId::try_new("a".repeat(64)).expect("service id");
        let namespace_id = NamespaceId::try_new("default").expect("namespace id");
        let error = VolumeDeclaredDeployPreviewTarget::try_new(DeployPreviewTarget {
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
        })
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
            vec![binding.clone()],
            vec![binding.clone()],
            vec![serving.clone()],
            vec![serving.clone()],
        );

        assert_eq!(
            projection.route_binding_commits,
            std::slice::from_ref(&binding)
        );
        assert_eq!(projection.route_binding_removals, [binding]);
        assert_eq!(
            projection.serving_target_commits,
            std::slice::from_ref(&serving)
        );
        assert_eq!(projection.serving_target_removals, [serving]);
    }
}
