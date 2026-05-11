use std::collections::{BTreeMap, BTreeSet};

use ployz_api::{BranchNamespaceRequest, BranchResourceMode, MigrateServiceRequest};
use ployz_store_api::{DeployStore, StoreDriver};
use ployz_types::Error as PloyzError;
use ployz_types::error::DeployError;
use ployz_types::model::VolumeRecord;
use ployz_types::spec::{
    DeployIntent, DeployManifest, MountSource, Namespace, ServiceIntent, ServiceIntentHint,
    ServiceSpec, VolumeCloneConsistency, VolumeCloneDataPolicy, VolumeDeclaration, VolumeIntent,
    VolumeIntentHint, VolumeMode, VolumeOwner, VolumeQuota, VolumeScope, stable_hash_hex,
    valid_storage_segment,
};

#[derive(Debug, thiserror::Error)]
pub(super) enum MigrateRenderError {
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

impl MigrateRenderError {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest { .. } => "MIGRATE_INVALID_REQUEST",
            Self::ManifestEncodeFailed { .. } => "MIGRATE_MANIFEST_ENCODE_FAILED",
            Self::ExportFailed { .. } => "MIGRATE_EXPORT_FAILED",
            Self::EmptyTargetMachine
            | Self::ServiceMissing { .. }
            | Self::NoManagedVolumeMounts { .. }
            | Self::UnsupportedBindMount { .. }
            | Self::DuplicateManagedVolumeMount { .. }
            | Self::MissingCommittedVolume { .. }
            | Self::AlreadyOnTarget { .. }
            | Self::UnsupportedVolumeScope { .. } => "MIGRATE_RENDER_FAILED",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum BranchRenderError {
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

impl BranchRenderError {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest { .. } => "BRANCH_INVALID_REQUEST",
            Self::ExportFailed { .. } => "BRANCH_EXPORT_FAILED",
            Self::ManifestEncodeFailed { .. } => "BRANCH_MANIFEST_ENCODE_FAILED",
            Self::EmptySource { .. } => "BRANCH_SOURCE_EMPTY",
            Self::UnknownService { .. } => "BRANCH_UNKNOWN_SERVICE",
            Self::DuplicateService { .. } => "BRANCH_DUPLICATE_SERVICE",
            Self::UnknownVolume { .. } => "BRANCH_UNKNOWN_VOLUME",
            Self::DuplicateVolume { .. } => "BRANCH_DUPLICATE_VOLUME",
            Self::InvalidManifest { .. } => "BRANCH_INVALID_MANIFEST",
        }
    }
}
pub(super) async fn export_manifest(
    store: &StoreDriver,
    namespace: &Namespace,
) -> ployz_types::Result<DeployManifest> {
    Ok(export_manifest_with_evidence(store, namespace)
        .await?
        .manifest)
}

pub(super) struct ExportedManifest {
    manifest: DeployManifest,
    service_revision_hashes: BTreeMap<String, String>,
    volume_records: BTreeMap<String, VolumeRecord>,
}

pub(super) async fn export_manifest_with_evidence(
    store: &StoreDriver,
    namespace: &Namespace,
) -> ployz_types::Result<ExportedManifest> {
    let releases = store.list_deploy_releases(namespace).await?;
    let revisions = store.list_deploy_revisions(namespace).await?;
    let volume_records = store.list_volumes(namespace).await?;
    let revisions_by_key: BTreeMap<(String, String), String> = revisions
        .into_iter()
        .map(|revision| {
            (
                (revision.service.clone(), revision.revision_hash.clone()),
                revision.spec_json,
            )
        })
        .collect();

    let mut services = Vec::with_capacity(releases.len());
    let mut service_revision_hashes = BTreeMap::new();
    for release in releases {
        let primary_revision_hash = release.release.primary_revision_hash().to_string();
        let key = (release.service.clone(), primary_revision_hash.clone());
        let Some(spec_json) = revisions_by_key.get(&key) else {
            return Err(PloyzError::Deploy(
                DeployError::StoredReleaseMissingRevision {
                    service: release.service,
                    revision_hash: primary_revision_hash,
                },
            ));
        };
        let spec: ServiceSpec = serde_json::from_str(spec_json).map_err(|err| {
            PloyzError::Deploy(DeployError::CommittedServiceSpecDecode {
                namespace: namespace.as_str().to_string(),
                service: release.service.clone(),
                message: err.to_string(),
            })
        })?;
        if spec.name != release.service {
            return Err(PloyzError::Deploy(DeployError::StoredSpecServiceMismatch {
                stored_service: spec.name,
                release_service: release.service,
            }));
        }
        service_revision_hashes.insert(release.service.clone(), primary_revision_hash);
        services.push(spec);
    }
    services.sort_by(|left, right| left.name.cmp(&right.name));

    let volume_records = volume_records
        .into_iter()
        .map(|record| (record.volume_name.clone(), record))
        .collect::<BTreeMap<_, _>>();
    let mut volumes: Vec<VolumeDeclaration> = volume_records
        .values()
        .map(|record| VolumeDeclaration {
            name: record.volume_name.clone(),
            scope: record.scope,
            quota: VolumeQuota::new(record.quota.clone()),
            mode: VolumeMode::new(record.mode.clone()),
            owner: VolumeOwner::new(record.owner.clone()),
        })
        .collect();
    volumes.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(ExportedManifest {
        manifest: DeployManifest {
            namespace: namespace.clone(),
            intent: None,
            volumes,
            services,
        },
        service_revision_hashes,
        volume_records,
    })
}

pub(super) fn validate_branch_namespace_request(
    request: &BranchNamespaceRequest,
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

pub(super) fn encode_branch_manifest_json(
    manifest: &DeployManifest,
) -> Result<String, BranchRenderError> {
    serde_json::to_string_pretty(manifest).map_err(|error| {
        BranchRenderError::ManifestEncodeFailed {
            message: error.to_string(),
        }
    })
}

pub(super) fn stable_fingerprint<T: serde::Serialize>(value: &T) -> String {
    let json = serde_json::to_vec(value).expect("branch source evidence should serialize");
    stable_hash_hex(&json)
}

pub(super) async fn render_branch_namespace_manifest(
    store: &StoreDriver,
    request: &BranchNamespaceRequest,
) -> Result<DeployManifest, BranchRenderError> {
    validate_branch_namespace_request(request)?;
    let source_namespace = Namespace::new(request.source_namespace.clone());
    let target_namespace = Namespace::new(request.target_namespace.clone());
    let exported = export_manifest_with_evidence(store, &source_namespace)
        .await
        .map_err(|error| BranchRenderError::ExportFailed {
            namespace: request.source_namespace.clone(),
            message: error.to_string(),
        })?;
    let mut manifest = exported.manifest;
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
            Some(BranchResourceMode::Branch) => Some(ServiceIntentHint {
                service: service.name.clone(),
                intent: ServiceIntent::Branch {
                    source_namespace: source_namespace.clone(),
                    source_service: service.name.clone(),
                    expected_source_revision_hash: exported
                        .service_revision_hashes
                        .get(&service.name)
                        .cloned(),
                },
            }),
            Some(BranchResourceMode::Fresh) | None => None,
        })
        .collect::<Vec<_>>();
    let volumes = manifest
        .volumes
        .iter()
        .filter_map(|volume| match volume_modes.get(volume.name.as_str()) {
            Some(BranchResourceMode::Branch) => Some(VolumeIntentHint {
                volume: volume.name.clone(),
                intent: VolumeIntent::Clone {
                    source_namespace: source_namespace.clone(),
                    source_volume: volume.name.clone(),
                    data_policy: VolumeCloneDataPolicy::Raw,
                    consistency: VolumeCloneConsistency::CrashConsistent,
                    expected_source_record_fingerprint: exported
                        .volume_records
                        .get(&volume.name)
                        .map(stable_fingerprint),
                },
            }),
            Some(BranchResourceMode::Fresh) | None => None,
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
    request: &BranchNamespaceRequest,
) -> Result<BTreeMap<String, BranchResourceMode>, BranchRenderError> {
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
    request: &BranchNamespaceRequest,
) -> Result<BTreeMap<String, BranchResourceMode>, BranchRenderError> {
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

pub(super) fn validate_migrate_service_request(
    request: &MigrateServiceRequest,
) -> Result<(), MigrateRenderError> {
    validate_migrate_segment("namespace", &request.namespace)?;
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

pub(super) fn encode_migrate_manifest_json(
    manifest: &DeployManifest,
) -> Result<String, MigrateRenderError> {
    serde_json::to_string_pretty(manifest).map_err(|error| {
        MigrateRenderError::ManifestEncodeFailed {
            message: error.to_string(),
        }
    })
}

pub(super) async fn render_migrate_service_manifest(
    store: &StoreDriver,
    request: &MigrateServiceRequest,
) -> Result<DeployManifest, MigrateRenderError> {
    let target_machine = request.target_machine.trim();
    if target_machine.is_empty() {
        return Err(MigrateRenderError::EmptyTargetMachine);
    }
    let namespace = Namespace::new(request.namespace.clone());
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
