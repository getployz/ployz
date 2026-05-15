use std::collections::BTreeSet;

use ployz_spec::{
    DeployIntent, DeployManifest, MountSource, Namespace, VolumeIntent, VolumeIntentHint,
    VolumeScope,
};
use ployz_store_api::{DeployStore, StoreDriver};

use super::export_manifest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateServiceManifestRequest {
    pub namespace: String,
    pub service: String,
    pub target_machine: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MigrateRenderError {
    #[error("invalid migrate request: {message}")]
    InvalidRequest { message: String },
    #[error("migrate target machine cannot be empty")]
    EmptyTargetMachine,
    #[error("service '{namespace}/{service}' is not deployed")]
    ServiceMissing { namespace: String, service: String },
    #[error("service '{namespace}/{service}' has no managed volume mounts")]
    NoManagedVolumeMounts { namespace: String, service: String },
    #[error(
        "service '{namespace}/{service}' uses bind mount '{bind_source}' at '{target}', which migrate service cannot transfer"
    )]
    UnsupportedBindMount {
        namespace: String,
        service: String,
        bind_source: String,
        target: String,
    },
    #[error("service '{namespace}/{service}' mounts volume '{volume}' more than once")]
    DuplicateManagedVolumeMount {
        namespace: String,
        service: String,
        volume: String,
    },
    #[error("service '{namespace}/{service}' mounted volume '{volume}' has no committed record")]
    MissingCommittedVolume {
        namespace: String,
        service: String,
        volume: String,
    },
    #[error("volume '{volume}' is already on target machine '{machine}'")]
    AlreadyOnTarget { volume: String, machine: String },
    #[error("volume '{volume}' must have scope=single to migrate a service")]
    UnsupportedVolumeScope { volume: String },
    #[error("failed to export namespace '{namespace}' for migration: {message}")]
    ExportFailed { namespace: String, message: String },
    #[error("failed to encode migration manifest: {message}")]
    ManifestEncodeFailed { message: String },
}

pub fn validate_migrate_service_manifest_request(
    request: &MigrateServiceManifestRequest,
) -> Result<(), MigrateRenderError> {
    validate_migrate_segment("namespace", &request.namespace)?;
    Namespace::try_new(request.namespace.as_str()).map_err(|message| {
        MigrateRenderError::InvalidRequest {
            message: format!("namespace is invalid: {message}"),
        }
    })?;
    validate_migrate_segment("service", &request.service)?;
    if request.target_machine.trim().is_empty() {
        return Err(MigrateRenderError::InvalidRequest {
            message: "target_machine cannot be empty".into(),
        });
    }
    Ok(())
}

fn validate_migrate_segment(name: &'static str, value: &str) -> Result<(), MigrateRenderError> {
    if value.is_empty() || value.contains('/') {
        return Err(MigrateRenderError::InvalidRequest {
            message: format!("{name} must be a non-empty path segment"),
        });
    }
    Ok(())
}

pub fn encode_migrate_manifest_json(
    manifest: &DeployManifest,
) -> Result<String, MigrateRenderError> {
    serde_json::to_string_pretty(manifest).map_err(|error| {
        MigrateRenderError::ManifestEncodeFailed {
            message: error.to_string(),
        }
    })
}

pub async fn render_migrate_service_manifest(
    store: &StoreDriver,
    request: &MigrateServiceManifestRequest,
) -> Result<DeployManifest, MigrateRenderError> {
    let target_machine = request.target_machine.trim();
    if target_machine.is_empty() {
        return Err(MigrateRenderError::EmptyTargetMachine);
    }
    let namespace = Namespace::try_new(request.namespace.as_str()).map_err(|message| {
        MigrateRenderError::InvalidRequest {
            message: format!("namespace is invalid: {message}"),
        }
    })?;
    let mut manifest = export_manifest(store, &namespace).await.map_err(|error| {
        MigrateRenderError::ExportFailed {
            namespace: request.namespace.clone(),
            message: error.to_string(),
        }
    })?;
    let Some(service) = manifest
        .services
        .iter()
        .find(|candidate| candidate.name == request.service)
    else {
        return Err(MigrateRenderError::ServiceMissing {
            namespace: request.namespace.clone(),
            service: request.service.clone(),
        });
    };

    let mut volume_names = BTreeSet::new();
    for mount in &service.template.mounts {
        match &mount.source {
            MountSource::Volume(volume) => {
                if !volume_names.insert(volume.clone()) {
                    return Err(MigrateRenderError::DuplicateManagedVolumeMount {
                        namespace: request.namespace.clone(),
                        service: request.service.clone(),
                        volume: volume.clone(),
                    });
                }
            }
            MountSource::Bind(source) => {
                return Err(MigrateRenderError::UnsupportedBindMount {
                    namespace: request.namespace.clone(),
                    service: request.service.clone(),
                    bind_source: source.clone(),
                    target: mount.target.clone(),
                });
            }
            MountSource::Tmpfs => {}
        }
    }
    if volume_names.is_empty() {
        return Err(MigrateRenderError::NoManagedVolumeMounts {
            namespace: request.namespace.clone(),
            service: request.service.clone(),
        });
    }

    let mut move_hints = Vec::with_capacity(volume_names.len());
    for volume in &volume_names {
        let record = store
            .get_volume(&namespace, volume)
            .await
            .map_err(|error| MigrateRenderError::ExportFailed {
                namespace: request.namespace.clone(),
                message: error.to_string(),
            })?
            .ok_or_else(|| MigrateRenderError::MissingCommittedVolume {
                namespace: request.namespace.clone(),
                service: request.service.clone(),
                volume: volume.clone(),
            })?;
        if record.scope != VolumeScope::Single {
            return Err(MigrateRenderError::UnsupportedVolumeScope {
                volume: volume.clone(),
            });
        }
        if record.machine_id.as_str() == target_machine {
            return Err(MigrateRenderError::AlreadyOnTarget {
                volume: volume.clone(),
                machine: target_machine.to_string(),
            });
        }
        move_hints.push(VolumeIntentHint {
            volume: volume.clone(),
            intent: VolumeIntent::Move {
                from_machine: record.machine_id.as_str().to_string(),
                to_machine: target_machine.to_string(),
            },
        });
    }

    let moving_volume_names = move_hints
        .iter()
        .map(|hint| hint.volume.as_str())
        .collect::<BTreeSet<_>>();
    let mut intent = manifest.intent.take().unwrap_or_else(|| DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: Vec::new(),
    });
    intent
        .volumes
        .retain(|hint| !moving_volume_names.contains(hint.volume.as_str()));
    intent.volumes.extend(move_hints);
    intent
        .volumes
        .sort_by(|left, right| left.volume.cmp(&right.volume));
    manifest.intent = Some(intent);

    Ok(manifest)
}
