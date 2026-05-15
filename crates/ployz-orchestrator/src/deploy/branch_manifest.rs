use std::collections::{BTreeMap, BTreeSet};

use ployz_spec::{
    DeployIntent, DeployManifest, Namespace, ServiceIntent, ServiceIntentHint,
    VolumeCloneConsistency, VolumeCloneDataPolicy, VolumeIntent, VolumeIntentHint, stable_hash_hex,
    valid_storage_segment,
};
use ployz_store_api::StoreDriver;

use super::export_manifest_with_evidence;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchManifestResourceMode {
    Fresh,
    Branch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchManifestResourceModeOverride {
    pub name: String,
    pub mode: BranchManifestResourceMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchNamespaceManifestRequest {
    pub source_namespace: String,
    pub target_namespace: String,
    pub default_service_mode: BranchManifestResourceMode,
    pub default_volume_mode: BranchManifestResourceMode,
    pub services: Vec<BranchManifestResourceModeOverride>,
    pub volumes: Vec<BranchManifestResourceModeOverride>,
}

#[derive(Debug, thiserror::Error)]
pub enum BranchRenderError {
    #[error("invalid branch request: {message}")]
    InvalidRequest { message: String },
    #[error("failed to export namespace '{namespace}' for branch: {message}")]
    ExportFailed { namespace: String, message: String },
    #[error("branch source namespace '{namespace}' has no committed services or volumes")]
    EmptySource { namespace: String },
    #[error("branch request references unknown service '{service}'")]
    UnknownService { service: String },
    #[error("branch request references service '{service}' more than once")]
    DuplicateService { service: String },
    #[error("branch request references unknown volume '{volume}'")]
    UnknownVolume { volume: String },
    #[error("branch request references volume '{volume}' more than once")]
    DuplicateVolume { volume: String },
    #[error("compiled branch manifest is invalid: {message}")]
    InvalidManifest { message: String },
    #[error("failed to encode branch manifest: {message}")]
    ManifestEncodeFailed { message: String },
}

pub fn validate_branch_namespace_manifest_request(
    request: &BranchNamespaceManifestRequest,
) -> Result<(), BranchRenderError> {
    validate_branch_segment("source_namespace", &request.source_namespace)?;
    validate_branch_segment("target_namespace", &request.target_namespace)?;
    if request.source_namespace == request.target_namespace {
        return Err(BranchRenderError::InvalidRequest {
            message: "source_namespace and target_namespace must differ".into(),
        });
    }
    for service in &request.services {
        validate_branch_segment("service override name", &service.name)?;
    }
    for volume in &request.volumes {
        validate_branch_segment("volume override name", &volume.name)?;
    }
    Ok(())
}

fn validate_branch_segment(name: &'static str, value: &str) -> Result<(), BranchRenderError> {
    if !valid_storage_segment(value) {
        return Err(BranchRenderError::InvalidRequest {
            message: format!(
                "{name} must be 1-63 chars of [a-z0-9_-], starting with a letter or digit"
            ),
        });
    }
    Ok(())
}

pub fn encode_branch_manifest_json(manifest: &DeployManifest) -> Result<String, BranchRenderError> {
    serde_json::to_string_pretty(manifest).map_err(|error| {
        BranchRenderError::ManifestEncodeFailed {
            message: error.to_string(),
        }
    })
}

pub fn stable_fingerprint<T: serde::Serialize>(value: &T) -> String {
    let json = serde_json::to_vec(value).expect("branch source evidence should serialize");
    stable_hash_hex(&json)
}

pub async fn render_branch_namespace_manifest(
    store: &StoreDriver,
    request: &BranchNamespaceManifestRequest,
) -> Result<DeployManifest, BranchRenderError> {
    validate_branch_namespace_manifest_request(request)?;
    let source_namespace = Namespace::new(request.source_namespace.clone());
    let target_namespace = Namespace::new(request.target_namespace.clone());
    let exported = export_manifest_with_evidence(store, &source_namespace)
        .await
        .map_err(|error| BranchRenderError::ExportFailed {
            namespace: request.source_namespace.clone(),
            message: error.to_string(),
        })?;
    let (mut manifest, service_revision_hashes, volume_records) = exported.into_parts();
    if manifest.services.is_empty() && manifest.volumes.is_empty() {
        return Err(BranchRenderError::EmptySource {
            namespace: request.source_namespace.clone(),
        });
    }
    manifest.namespace = target_namespace;

    let service_modes = branch_service_modes(&manifest, request)?;
    let volume_modes = branch_volume_modes(&manifest, request)?;

    let services = manifest
        .services
        .iter()
        .filter_map(|service| match service_modes.get(service.name.as_str()) {
            Some(BranchManifestResourceMode::Branch) => Some(ServiceIntentHint {
                service: service.name.clone(),
                intent: ServiceIntent::Branch {
                    source_namespace: source_namespace.clone(),
                    source_service: service.name.clone(),
                    expected_source_revision_hash: service_revision_hashes
                        .get(&service.name)
                        .cloned(),
                },
            }),
            Some(BranchManifestResourceMode::Fresh) | None => None,
        })
        .collect::<Vec<_>>();
    let volumes = manifest
        .volumes
        .iter()
        .filter_map(|volume| match volume_modes.get(volume.name.as_str()) {
            Some(BranchManifestResourceMode::Branch) => Some(VolumeIntentHint {
                volume: volume.name.clone(),
                intent: VolumeIntent::Clone {
                    source_namespace: source_namespace.clone(),
                    source_volume: volume.name.clone(),
                    data_policy: VolumeCloneDataPolicy::Raw,
                    consistency: VolumeCloneConsistency::CrashConsistent,
                    expected_source_record_fingerprint: volume_records
                        .get(&volume.name)
                        .map(stable_fingerprint),
                },
            }),
            Some(BranchManifestResourceMode::Fresh) | None => None,
        })
        .collect::<Vec<_>>();

    manifest.intent = if services.is_empty() && volumes.is_empty() {
        None
    } else {
        Some(DeployIntent {
            services,
            volumes,
            phases: Vec::new(),
        })
    };
    manifest
        .validate()
        .map_err(|message| BranchRenderError::InvalidManifest { message })?;
    Ok(manifest)
}

fn branch_service_modes(
    manifest: &DeployManifest,
    request: &BranchNamespaceManifestRequest,
) -> Result<BTreeMap<String, BranchManifestResourceMode>, BranchRenderError> {
    let mut modes = manifest
        .services
        .iter()
        .map(|service| (service.name.clone(), request.default_service_mode))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for override_mode in &request.services {
        if !seen.insert(override_mode.name.as_str()) {
            return Err(BranchRenderError::DuplicateService {
                service: override_mode.name.clone(),
            });
        }
        let Some(mode) = modes.get_mut(override_mode.name.as_str()) else {
            return Err(BranchRenderError::UnknownService {
                service: override_mode.name.clone(),
            });
        };
        *mode = override_mode.mode;
    }
    Ok(modes)
}

fn branch_volume_modes(
    manifest: &DeployManifest,
    request: &BranchNamespaceManifestRequest,
) -> Result<BTreeMap<String, BranchManifestResourceMode>, BranchRenderError> {
    let mut modes = manifest
        .volumes
        .iter()
        .map(|volume| (volume.name.clone(), request.default_volume_mode))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for override_mode in &request.volumes {
        if !seen.insert(override_mode.name.as_str()) {
            return Err(BranchRenderError::DuplicateVolume {
                volume: override_mode.name.clone(),
            });
        }
        let Some(mode) = modes.get_mut(override_mode.name.as_str()) else {
            return Err(BranchRenderError::UnknownVolume {
                volume: override_mode.name.clone(),
            });
        };
        *mode = override_mode.mode;
    }
    Ok(modes)
}
