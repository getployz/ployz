//! Which upload each Service without a source of its own builds from, and which
//! uploads Cloud keeps for that.
//!
//! A Deployment carries one upload, the one Cloud extracts for it. Each Service
//! without a source it targets builds from that upload when it sent one, and
//! otherwise from the one it last built from, which may be older. Cloud keeps, per
//! Service, that upload and its latest upload sent, and every upload a Deployment
//! that may still run builds from.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{SavedEnvironmentIntent, ServiceSource};
use ployz_core::{RpcError, UploadDigest};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{COLUMNS, Frozen, Stored, UploadedSource, stored};
use crate::Actor;
use crate::error;
use crate::id::{DeploymentId, EnvironmentId};
use crate::storage::Tx;

/// Which of these Deployments' uploads still build something. Cloud keeps each
/// upload under the Deployment that sent it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct UploadsQuery {
    pub deployments: Vec<DeploymentId>,
}

/// One answer per Deployment the Store has; one it doesn't have is left out.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct UploadsView {
    pub uploads: Vec<HeldUpload>,
}

/// A Deployment's upload, and whether anything still builds from it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct HeldUpload {
    pub deployment: DeploymentId,
    pub environment_id: EnvironmentId,
    /// The upload's digest; none when the Deployment carries none.
    pub digest: Option<String>,
    /// Whether a Service still builds from it, or a Deployment that may still run does.
    pub kept: bool,
}

/// Every Deployment of `environment` that carried an upload, oldest first.
fn uploaded(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<Vec<Stored>, RpcError> {
    let columns = COLUMNS.as_str();
    tx.query(
        &format!(
            "SELECT {columns} FROM config_deployment \
             WHERE environment_id = ?1 AND number IS NOT NULL AND upload IS NOT NULL ORDER BY number"
        ),
        &[environment.as_str().into()],
    )?
    .iter()
    .map(stored)
    .collect()
}

fn targets(deployment: &Stored, service: &str) -> bool {
    deployment.nodes.iter().any(|node| node.id() == service)
}

/// The digest of the upload `deployment` builds `service` from, by lineage, if it
/// targets it with one.
pub(crate) fn own<'a>(deployment: &'a Stored, service: &str) -> Option<&'a UploadDigest> {
    if !targets(deployment, service) {
        return None;
    }
    match &deployment.service_uploads {
        Some(uploads) => uploads.get(service),
        None => deployment
            .summary
            .upload
            .as_ref()
            .map(|upload| &upload.digest),
    }
}

/// The digest of the upload each Service without a source builds from, by lineage.
pub(crate) type ServiceUploads = BTreeMap<String, UploadDigest>;

/// What a Deployment admitted with `upload` builds its target Services without a
/// source from: the upload it carries, and each such Service's digest by lineage.
/// A new upload is every one's. Without one, each builds from the upload it last
/// built from, and the Deployment carries the newest of those: the one Cloud
/// extracts. One with none yet builds from the carried one.
pub(crate) fn admitted(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    intent: &SavedEnvironmentIntent,
    frozen: &Frozen,
    upload: Option<UploadedSource>,
) -> Result<(Option<UploadedSource>, Option<ServiceUploads>), RpcError> {
    let sourceless: Vec<&str> = intent
        .services
        .iter()
        .filter(|service| matches!(service.config.source, ServiceSource::Empty { .. }))
        .filter(|service| frozen.targets(&service.id))
        .map(|service| service.id.as_str())
        .collect();
    if sourceless.is_empty() {
        return Ok((upload, None));
    }
    if let Some(upload) = upload {
        let uploads = sourceless
            .iter()
            .map(|service| ((*service).to_owned(), upload.digest.clone()))
            .collect();
        return Ok((Some(upload), Some(uploads)));
    }
    let deployments = uploaded(tx, environment)?;
    // Each Service's own upload, with where its latest Deployment stands.
    let owned: Vec<(&str, usize, &UploadDigest)> = sourceless
        .iter()
        .filter_map(|service| {
            deployments
                .iter()
                .enumerate()
                .rev()
                .find_map(|(at, deployment)| own(deployment, service).map(|digest| (at, digest)))
                .map(|(at, digest)| (*service, at, digest))
        })
        .collect();
    // The newest, preferring what that Deployment carried itself.
    let carries = |at: usize, digest: &UploadDigest| {
        deployments
            .get(at)
            .and_then(|deployment| deployment.summary.upload.as_ref())
            .is_some_and(|upload| &upload.digest == digest)
    };
    let Some(newest) = owned
        .iter()
        .max_by_key(|(_, at, digest)| (*at, carries(*at, digest)))
        .map(|(_, _, digest)| *digest)
    else {
        return Ok((None, None));
    };
    let Some(carried) = deployments
        .iter()
        .rev()
        .filter_map(|deployment| deployment.summary.upload.as_ref())
        .find(|upload| &upload.digest == newest)
        .cloned()
    else {
        return Err(error::corrupt("Deployment upload"));
    };
    let uploads = sourceless
        .iter()
        .map(|service| {
            let digest = owned
                .iter()
                .find(|(owner, _, _)| owner == service)
                .map_or(newest, |(_, _, digest)| *digest);
            ((*service).to_owned(), digest.clone())
        })
        .collect();
    Ok((Some(carried), Some(uploads)))
}

/// The digests Cloud keeps for `environment`, given `held`, the Deployments Cloud
/// holds an upload for: those that sent one. Each Service without a source keeps
/// the upload it last built from and its latest upload sent, and a Deployment that
/// may still run keeps every upload it builds from.
fn kept(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    held: &BTreeSet<DeploymentId>,
) -> Result<BTreeSet<UploadDigest>, RpcError> {
    let deployments = uploaded(tx, environment)?;
    let mut kept: BTreeSet<UploadDigest> = BTreeSet::new();
    for deployment in deployments
        .iter()
        .filter(|deployment| deployment.summary.in_flight)
    {
        kept.extend(
            deployment
                .summary
                .upload
                .iter()
                .map(|upload| upload.digest.clone()),
        );
        kept.extend(
            deployment
                .service_uploads
                .iter()
                .flat_map(|uploads| uploads.values().cloned()),
        );
    }
    let Some(saved) = crate::review::latest_saved(tx, environment)? else {
        return Ok(kept);
    };
    for service in saved
        .intent
        .services
        .iter()
        .filter(|service| matches!(service.config.source, ServiceSource::Empty { .. }))
    {
        let mut latest = deployments.iter().rev();
        kept.extend(
            latest
                .clone()
                .find_map(|deployment| own(deployment, &service.id))
                .cloned(),
        );
        kept.extend(
            latest
                .find(|deployment| {
                    held.contains(&deployment.summary.id) && targets(deployment, &service.id)
                })
                .and_then(|deployment| own(deployment, &service.id))
                .cloned(),
        );
    }
    Ok(kept)
}

pub(crate) fn uploads(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &UploadsQuery,
) -> Result<UploadsView, RpcError> {
    let held: BTreeSet<DeploymentId> = query.deployments.iter().cloned().collect();
    let mut environments: BTreeMap<EnvironmentId, BTreeSet<UploadDigest>> = BTreeMap::new();
    let mut uploads = Vec::new();
    for id in &query.deployments {
        let deployment = match super::owned(tx, who, id) {
            Ok(deployment) => deployment,
            Err(refused) if refused.code == ployz_core::RpcErrorCode::NotFound => continue,
            Err(refused) => return Err(refused),
        };
        let environment = deployment.summary.environment_id.clone();
        let digests = match environments.entry(environment.clone()) {
            std::collections::btree_map::Entry::Occupied(known) => known.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(kept(tx, &environment, &held)?)
            }
        };
        let digest = deployment.summary.upload.map(|upload| upload.digest);
        let kept = digest
            .as_ref()
            .is_some_and(|digest| digests.contains(digest));
        let digest = digest.map(|digest| digest.as_str().to_owned());
        uploads.push(HeldUpload {
            deployment: id.clone(),
            environment_id: environment,
            digest,
            kept,
        });
    }
    Ok(UploadsView { uploads })
}
