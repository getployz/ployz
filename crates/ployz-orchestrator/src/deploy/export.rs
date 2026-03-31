use crate::error::{Error, Result};
use ployz_store_api::DeployReadStore;
use ployz_types::spec::{DeployManifest, Namespace, ServiceSpec};
use std::collections::BTreeMap;

pub async fn export_manifest(
    store: &dyn DeployReadStore,
    namespace: &Namespace,
) -> Result<DeployManifest> {
    let releases = store.list_service_releases(namespace).await?;
    let revisions = store.list_service_revisions(namespace).await?;
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
    for release in releases {
        let key = (
            release.service.clone(),
            release.release.primary_revision_hash.clone(),
        );
        let Some(spec_json) = revisions_by_key.get(&key) else {
            return Err(Error::operation(
                "deploy_export",
                format!(
                    "current release for service '{}' referenced missing revision '{}'",
                    release.service, release.release.primary_revision_hash
                ),
            ));
        };
        let spec: ServiceSpec = serde_json::from_str(spec_json).map_err(|error| {
            Error::operation(
                "deploy_export",
                format!(
                    "invalid stored spec for service '{}': {error}",
                    release.service
                ),
            )
        })?;
        if spec.name != release.service {
            return Err(Error::operation(
                "deploy_export",
                format!(
                    "stored spec service '{}' did not match release service '{}'",
                    spec.name, release.service
                ),
            ));
        }
        services.push(spec);
    }
    services.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(DeployManifest {
        namespace: namespace.clone(),
        services,
    })
}
