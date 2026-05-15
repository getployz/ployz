use std::collections::BTreeMap;

use ployz_error::DeployError;
use ployz_error::Error as PloyzError;
use ployz_model::VolumeRecord;
use ployz_spec::{
    DeployManifest, Namespace, ServiceSpec, VolumeDeclaration, VolumeMode, VolumeOwner, VolumeQuota,
};
use ployz_store_api::{DeployStore, StoreDriver};

pub async fn export_manifest(
    store: &StoreDriver,
    namespace: &Namespace,
) -> ployz_error::Result<DeployManifest> {
    Ok(export_manifest_with_evidence(store, namespace)
        .await?
        .manifest)
}

pub struct ExportedManifest {
    manifest: DeployManifest,
    service_revision_hashes: BTreeMap<String, String>,
    volume_records: BTreeMap<String, VolumeRecord>,
}

impl ExportedManifest {
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        DeployManifest,
        BTreeMap<String, String>,
        BTreeMap<String, VolumeRecord>,
    ) {
        (
            self.manifest,
            self.service_revision_hashes,
            self.volume_records,
        )
    }
}

pub async fn export_manifest_with_evidence(
    store: &StoreDriver,
    namespace: &Namespace,
) -> ployz_error::Result<ExportedManifest> {
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
        .map(volume_declaration_from_record)
        .collect::<ployz_error::Result<_>>()?;
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

fn volume_declaration_from_record(record: &VolumeRecord) -> ployz_error::Result<VolumeDeclaration> {
    let quota = VolumeQuota::try_new(record.quota.as_str()).map_err(|message| {
        PloyzError::Deploy(DeployError::StoredVolumeMetadataInvalid {
            volume: record.volume_name.clone(),
            field: "quota",
            message,
        })
    })?;
    let mode = VolumeMode::try_new(record.mode.as_str()).map_err(|message| {
        PloyzError::Deploy(DeployError::StoredVolumeMetadataInvalid {
            volume: record.volume_name.clone(),
            field: "mode",
            message,
        })
    })?;
    let owner = VolumeOwner::try_new(record.owner.as_str()).map_err(|message| {
        PloyzError::Deploy(DeployError::StoredVolumeMetadataInvalid {
            volume: record.volume_name.clone(),
            field: "owner",
            message,
        })
    })?;

    Ok(VolumeDeclaration {
        name: record.volume_name.clone(),
        scope: record.scope,
        quota,
        mode,
        owner,
    })
}
