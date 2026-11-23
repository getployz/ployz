//! The proof a moved Volume's Service carries to the Server that starts it, read from
//! the Config Store as a drain reads a moved copy's.

use std::sync::Arc;

use ployz_core::{
    MachineId, Namespace, Region, ResolvedServiceSpec, RpcError,
    authority::{RuleProof, ServiceVersion, Signed},
};
use ployz_store::{ConfigStore, OrganizationId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The start of a moved Volume's Service on Server `machine`.
#[derive(Clone, Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VolumeStartQuery {
    pub namespace: Namespace,
    /// The spec the source Server's Container runs.
    pub spec: ResolvedServiceSpec,
    pub machine: MachineId,
    /// `machine`'s region, for a Service kept in one: the spec starts there.
    #[serde(default)]
    #[ts(optional)]
    pub region: Option<Region>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VolumeStartProof {
    /// The spec `machine` starts, and the signed versions or verified build that admit it
    /// there while their signers stay trusted.
    Admits {
        spec: Box<ResolvedServiceSpec>,
        versions: Vec<Signed<ServiceVersion>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        rule: Option<Box<RuleProof>>,
    },
    /// No version signs this spec and no rule built its image.
    Unsigned,
    /// Only a rule built it, and the rule doesn't list `machine` or was replaced since.
    OffRule,
}

/// What admits `query`'s start, by its spec's digest in `region`.
///
/// # Errors
/// Returns `invalid_argument` for a spec with no digest, or the Store's error.
pub async fn volume_start_proof(
    store: &Arc<ConfigStore>,
    organization: OrganizationId,
    query: VolumeStartQuery,
) -> Result<VolumeStartProof, RpcError> {
    let VolumeStartQuery {
        namespace,
        mut spec,
        machine,
        region,
    } = query;
    if let Some(region) = &region {
        Region::keep(Some(region), &mut spec.placement, &mut spec.regions);
    }
    let read = crate::deploy::kept_query(&namespace, &spec).ok_or_else(|| {
        RpcError::invalid_argument("This Service's spec has no digest to look up.")
    })?;
    let view = super::store_call(store, move |store| store.versions(&organization, &read)).await?;
    let lists = |proof: &RuleProof| {
        proof.rule.open().is_ok_and(|rule| {
            !rule.ended
                && rule.generation == proof.claim.generation
                && rule.machines.contains(&machine)
        })
    };
    Ok(match (view.versions.is_empty(), view.rule) {
        (true, None) => VolumeStartProof::Unsigned,
        (true, Some(rule)) if !lists(&rule) => VolumeStartProof::OffRule,
        (_, rule) => VolumeStartProof::Admits {
            spec: Box::new(spec),
            versions: view.versions,
            rule,
        },
    })
}
