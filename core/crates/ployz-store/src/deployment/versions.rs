//! Signed versions the Store keeps for its runners and drains, and what each Deployment
//! ran.
//!
//! A client posts each Service's signed version with the image it pinned the Service's
//! tag to. The Store keeps the statement under the Namespace, Service and digest its own
//! body names, and records the pin as what that Service of the Deployment runs. It checks
//! no signature: every Server does, against the keys it trusts.

use std::collections::BTreeMap;

use ployz_core::authority::{
    PayloadDigest, ServiceVersion, Signed, deploy::content_addressed, version::MAX_PRESENTED,
};
use ployz_core::{Namespace, RequestedServiceSpec, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::{TargetNode, now};
use crate::error;
use crate::id::DeploymentId;
use crate::storage::Tx;

/// A signed version a client posts with a Deploy, and the image it pinned for it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PostedVersion {
    #[ts(as = "Signed<ployz_core::authority::DeployApproval>")]
    pub statement: Signed<ServiceVersion>,
    /// None when the Saved State names the Service's image by digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pin: Option<TagPin>,
}

/// A Saved image reference that names a tag, and the content-addressed image a client
/// resolved it to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TagPin {
    /// The whole Saved image reference, repository included, so another repository is
    /// another tag.
    pub tag: String,
    pub image: String,
}

impl TagPin {
    /// Whether this pin is what the Saved image reference `saved` runs.
    fn resolves(&self, saved: &str) -> bool {
        self.tag == saved || self.image == saved
    }
}

/// What one Service of a Deployment runs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RanService {
    /// The digest of the Service's spec once pinned: the only one its runner sends
    /// statements for.
    pub digest: PayloadDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<TagPin>,
}

/// What a runner sends for one Service it creates: the pin to write into its spec, the
/// digest that spec must have, and every signed version kept for that digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pinned {
    pub ran: RanService,
    pub versions: Vec<Signed<ServiceVersion>>,
}

type Ran = BTreeMap<ServiceName, RanService>;

/// Keep each of `posted` and record it as what its Service of Deployment `id` runs, over
/// what `id` already records for that Service.
///
/// # Errors
/// Returns `invalid_argument` for a statement whose body doesn't parse or a pin that
/// isn't content-addressed, or a storage error.
pub(crate) fn post(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    posted: &[PostedVersion],
) -> Result<(), RpcError> {
    if posted.is_empty() {
        return Ok(());
    }
    let organization = crate::build::organization(tx, id)?;
    let mut ran = ran_of(tx, id)?;
    for PostedVersion { statement, pin } in posted {
        let body = statement.open().map_err(|refusal| {
            error::invalid(
                format!("A signed version doesn't read: {refusal}"),
                json!({}),
            )
        })?;
        if let Some(pin) = pin
            && !content_addressed(&pin.image)
        {
            return Err(error::invalid(
                format!(
                    "{}'s pinned image doesn't name its digest: pin it as name@sha256:…",
                    body.service
                ),
                json!({ "service": body.service, "image": pin.image }),
            ));
        }
        tx.execute(
            "INSERT INTO config_service_version \
             (organization_id, namespace, service, digest, signer, statement, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (organization_id, namespace, service, digest, signer) \
             DO UPDATE SET statement = excluded.statement, created_at = excluded.created_at",
            &[
                organization.as_str().into(),
                body.namespace.as_str().into(),
                body.service.as_str().into(),
                body.digest.to_string().as_str().into(),
                signer(&body, statement).as_str().into(),
                json_text(statement)?.as_str().into(),
                now().into(),
            ],
        )?;
        ran.insert(
            body.service,
            RanService {
                digest: body.digest,
                pin: pin.clone(),
            },
        );
    }
    write_ran(tx, id, &ran)
}

/// Who signed `statement`, as the key of its row: the Device, or the passkeys that
/// answered for it, since every passkey's body names the same `"passkey"`.
fn signer(body: &ServiceVersion, statement: &Signed<ServiceVersion>) -> String {
    let answered: Vec<_> = statement
        .assertions
        .iter()
        .chain(
            statement
                .proposal
                .iter()
                .flat_map(|bundle| &bundle.assertions),
        )
        .map(|assertion| &assertion.credential_id)
        .collect();
    serde_json::to_string(&(body.signer, answered)).unwrap_or_default()
}

/// Every signed version kept for `service` in `namespace` at `digest`, newest first, at
/// most as many as one request carries.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a row that doesn't read.
pub(crate) fn versions_for(
    tx: &mut dyn Tx,
    organization: &str,
    (namespace, service): (&Namespace, &ServiceName),
    digest: PayloadDigest,
) -> Result<Vec<Signed<ServiceVersion>>, RpcError> {
    tx.query(
        "SELECT statement FROM config_service_version \
         WHERE organization_id = ?1 AND namespace = ?2 AND service = ?3 AND digest = ?4 \
         ORDER BY created_at DESC, signer LIMIT ?5",
        &[
            organization.into(),
            namespace.as_str().into(),
            service.as_str().into(),
            digest.to_string().as_str().into(),
            i64::try_from(MAX_PRESENTED).unwrap_or(i64::MAX).into(),
        ],
    )?
    .iter()
    .map(|row| row.json(0, "Signed version"))
    .collect()
}

/// What each Service of `target` runs in Deployment `id`, written back so a retry of `id`
/// runs the same, with the versions kept for each digest. A Service `id` records nothing
/// for takes what the Deployment that last applied it ran, but only while that pin is
/// still what its Saved image reference names: a changed tag runs no older pin.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a record that doesn't read.
pub(super) fn pinned(
    tx: &mut dyn Tx,
    (id, environment, namespace): (&DeploymentId, &str, &Namespace),
    (nodes, target): (&[TargetNode], &[RequestedServiceSpec]),
) -> Result<BTreeMap<ServiceName, Pinned>, RpcError> {
    let recorded = ran_of(tx, id)?;
    let mut ran = recorded.clone();
    for spec in target {
        if ran.contains_key(&spec.name) {
            continue;
        }
        let Some(node) = nodes.iter().find_map(|node| match node {
            TargetNode::Service { id, runtime, .. } if *runtime == spec.name => Some(id),
            TargetNode::Service { .. }
            | TargetNode::Retire { .. }
            | TargetNode::Volume { .. }
            | TargetNode::Config { .. } => None,
        }) else {
            continue;
        };
        if let Some(last) = last_applied(tx, environment, node.as_str(), &spec.name)?
            && last
                .pin
                .as_ref()
                .is_none_or(|pin| pin.resolves(&spec.container.image))
        {
            ran.insert(spec.name.clone(), last);
        }
    }
    if ran != recorded {
        write_ran(tx, id, &ran)?;
    }
    let organization = crate::build::organization(tx, id)?;
    ran.into_iter()
        .filter(|(service, _)| target.iter().any(|spec| spec.name == *service))
        .map(|(service, ran)| {
            let versions =
                versions_for(tx, organization.as_str(), (namespace, &service), ran.digest)?;
            Ok((service, Pinned { ran, versions }))
        })
        .collect()
}

/// What `service` ran in the Deployment that last applied Service node `node`.
fn last_applied(
    tx: &mut dyn Tx,
    environment: &str,
    node: &str,
    service: &ServiceName,
) -> Result<Option<RanService>, RpcError> {
    let rows = tx.query(
        "SELECT d.ran FROM config_applied a JOIN config_deployment d ON d.id = a.deployment_id \
         WHERE a.environment_id = ?1 AND a.node_id = ?2",
        &[environment.into(), node.into()],
    )?;
    let Some(text) = rows
        .first()
        .map(|row| row.optional_text(0))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    let mut ran: Ran = serde_json::from_str(text).map_err(|_| error::corrupt("Deployment ran"))?;
    Ok(ran.remove(service))
}

fn ran_of(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Ran, RpcError> {
    let rows = tx.query(
        "SELECT ran FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    rows.first()
        .map(|row| row.optional_text(0))
        .transpose()?
        .flatten()
        .map_or(Ok(Ran::new()), |text| {
            serde_json::from_str(text).map_err(|_| error::corrupt("Deployment ran"))
        })
}

fn write_ran(tx: &mut dyn Tx, id: &DeploymentId, ran: &Ran) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_deployment SET ran = ?2 WHERE id = ?1",
        &[id.as_str().into(), json_text(ran)?.as_str().into()],
    )
    .map(drop)
}

fn json_text(value: &impl Serialize) -> Result<String, RpcError> {
    serde_json::to_string(value).map_err(|_| error::corrupt("Signed version"))
}
