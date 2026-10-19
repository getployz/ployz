//! Which upload each Service without a source of its own builds from, and which
//! uploads Cloud keeps for that.
//!
//! A Deployment carries one upload, for the Services without a source it targets.
//! Each such Service builds from the upload of the latest Deployment that targeted
//! it with one. Cloud keeps, per Service, that upload and its latest new one, and
//! every upload a Deployment that may still run builds from.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::{SavedEnvironmentIntent, ServiceSource};
use serde::{Deserialize, Serialize};
use serde_json::json;
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
    tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_deployment \
             WHERE environment_id = ?1 AND upload IS NOT NULL ORDER BY number"
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

/// The upload a Deployment without a new one builds from: the one each of its target
/// Services without a source last built from. Services that last built from different
/// uploads can't share one Deployment, so it refuses.
pub(crate) fn inherited(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    intent: &SavedEnvironmentIntent,
    frozen: &Frozen,
) -> Result<Option<UploadedSource>, RpcError> {
    let sourceless: Vec<_> = intent
        .services
        .iter()
        .filter(|service| matches!(service.config.source, ServiceSource::Empty { .. }))
        .filter(|service| frozen.targets(&service.id))
        .collect();
    if sourceless.is_empty() {
        return Ok(None);
    }
    let deployments = uploaded(tx, environment)?;
    let mut by_digest: BTreeMap<String, (UploadedSource, Vec<String>)> = BTreeMap::new();
    for service in sourceless {
        let Some(upload) = deployments
            .iter()
            .rev()
            .find(|deployment| targets(deployment, &service.id))
            .and_then(|deployment| deployment.summary.upload.clone())
        else {
            continue;
        };
        by_digest
            .entry(upload.digest.as_str().to_owned())
            .or_insert_with(|| (upload, Vec::new()))
            .1
            .push(service.config.private_dns.as_str().to_owned());
    }
    let mut uploads = by_digest.into_values();
    let Some((upload, first)) = uploads.next() else {
        return Ok(None);
    };
    if let Some((_, second)) = uploads.next() {
        let (a, b) = (&first[0], &second[0]);
        return Err(error::invalid(
            format!(
                "{a} and {b} were uploaded separately, so one Deployment can't build both. \
                 Deploy them one at a time: ployz deploy {a}, then ployz deploy {b}"
            ),
            json!({ "services": [a, b] }),
        ));
    }
    Ok(Some(upload))
}

/// The digests Cloud keeps for `environment`: each Service without a source keeps
/// the upload it last built from and its latest new upload, and a Deployment that
/// may still run keeps its own.
fn kept(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<BTreeSet<String>, RpcError> {
    let deployments = uploaded(tx, environment)?;
    let digest = |deployment: &Stored| {
        deployment
            .summary
            .upload
            .as_ref()
            .map(|upload| upload.digest.as_str().to_owned())
    };
    let mut kept: BTreeSet<String> = deployments
        .iter()
        .filter(|deployment| deployment.summary.in_flight)
        .filter_map(digest)
        .collect();
    // An upload is new where it first appears; a retry or a settings-only Deploy
    // carries an earlier one.
    let mut seen = BTreeSet::new();
    let new: Vec<bool> = deployments
        .iter()
        .map(|deployment| digest(deployment).is_some_and(|digest| seen.insert(digest)))
        .collect();
    let Some(saved) = crate::review::latest_saved(tx, environment)? else {
        return Ok(kept);
    };
    for service in saved
        .intent
        .services
        .iter()
        .filter(|service| matches!(service.config.source, ServiceSource::Empty { .. }))
    {
        let mut covering = deployments
            .iter()
            .zip(&new)
            .rev()
            .filter(|(deployment, _)| targets(deployment, &service.id));
        kept.extend(
            covering
                .clone()
                .next()
                .and_then(|(deployment, _)| digest(deployment)),
        );
        kept.extend(
            covering
                .find(|(_, new)| **new)
                .and_then(|(deployment, _)| digest(deployment)),
        );
    }
    Ok(kept)
}

pub(crate) fn uploads(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &UploadsQuery,
) -> Result<UploadsView, RpcError> {
    let mut environments: BTreeMap<EnvironmentId, BTreeSet<String>> = BTreeMap::new();
    let mut uploads = Vec::new();
    for id in &query.deployments {
        let deployment = match super::owned(tx, who, id) {
            Ok(deployment) => deployment,
            Err(refused) if refused.code == ployz_core::RpcErrorCode::NotFound => continue,
            Err(refused) => return Err(refused),
        };
        let environment = deployment.summary.environment_id.clone();
        if !environments.contains_key(&environment) {
            let digests = kept(tx, &environment)?;
            environments.insert(environment.clone(), digests);
        }
        let digest = deployment
            .summary
            .upload
            .map(|upload| upload.digest.as_str().to_owned());
        let kept = digest
            .as_ref()
            .is_some_and(|digest| environments[&environment].contains(digest));
        uploads.push(HeldUpload {
            deployment: id.clone(),
            environment_id: environment,
            digest,
            kept,
        });
    }
    Ok(UploadsView { uploads })
}
