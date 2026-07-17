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

impl DeployPreviewTarget {
    #[must_use]
    pub fn into_planning_target(self) -> (DeployRequest, BTreeSet<ServiceId>) {
        let Self {
            namespace_id,
            origin,
            volumes,
            services,
        } = self;
        let mut pending_builds = BTreeSet::new();
        let services = services
            .into_iter()
            .map(|service| service.into_planning_service(&mut pending_builds))
            .collect();
        (
            DeployRequest {
                namespace_id,
                origin,
                volumes,
                services,
            },
            pending_builds,
        )
    }
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

impl DeployPreviewService {
    fn into_planning_service(self, pending_builds: &mut BTreeSet<ServiceId>) -> DeployServiceSpec {
        let Self {
            service_id,
            image,
            replicas,
            keep,
            runtime,
            pre_start,
            depends_on,
            routes,
        } = self;
        let (image, image_source) = match image {
            DeployPreviewImage::Concrete {
                image,
                image_source,
            } => (image, image_source),
            DeployPreviewImage::PendingBuild => {
                pending_builds.insert(service_id.clone());
                (
                    ImageReference::try_new("ployz.invalid/pending-build:preview")
                        .expect("internal pending-build image reference is valid"),
                    ImageSource::Registry,
                )
            }
        };
        DeployServiceSpec {
            service_id,
            image,
            image_source,
            replicas,
            keep,
            runtime,
            pre_start,
            depends_on,
            routes,
        }
    }
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
}

impl From<DeployPlan> for DeployPreviewProjection {
    fn from(plan: DeployPlan) -> Self {
        let DeployPlan {
            namespace_id,
            namespace_revision_id: _,
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
        }
    }
}
