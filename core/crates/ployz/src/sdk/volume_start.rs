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
    /// `machine`'s region label. A Service kept in a region starts in this one.
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

/// The specs `machine` may start for `spec`, in the order their approvals are looked up.
/// A Service kept in a region starts in `region`, written either way lowering writes one
/// region: as `spec` writes it, then the other way, a count of one there or a constraint.
/// A per-Service region edit signs a count where the Environment's region signed a
/// constraint, so the Deploy that allowed the move signed the other one. A Service that
/// runs anywhere keeps its spec.
fn starting_specs(spec: ResolvedServiceSpec, region: Option<&Region>) -> Vec<ResolvedServiceSpec> {
    let Some(region) = region.filter(|_| Region::kept_in(&spec.placement, &spec.regions).is_some())
    else {
        return vec![spec];
    };
    let mut same = spec.clone();
    Region::keep(Some(region), &mut same.placement, &mut same.regions);
    let mut other = spec;
    if other.regions.is_empty() {
        other
            .regions
            .insert(region.clone(), std::num::NonZeroU32::MIN);
    } else {
        other.regions.clear();
    }
    Region::keep(Some(region), &mut other.placement, &mut other.regions);
    vec![same, other]
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
        spec,
        machine,
        region,
    } = query;
    let mut found = None;
    for spec in starting_specs(spec, region.as_ref()) {
        let read = crate::deploy::kept_query(&namespace, &spec).ok_or_else(|| {
            RpcError::invalid_argument("This Service's spec has no digest to look up.")
        })?;
        let organization = organization.clone();
        let view =
            super::store_call(store, move |store| store.versions(&organization, &read)).await?;
        let signed = !view.versions.is_empty() || view.rule.is_some();
        found = Some((spec, view));
        if signed {
            break;
        }
    }
    let Some((spec, view)) = found else {
        return Ok(VolumeStartProof::Unsigned);
    };
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

#[cfg(test)]
mod tests {
    use ployz_core::config::{RegionReplicas, Replicas, lower_placement};

    use super::*;

    fn region(name: &str) -> Region {
        Region::parse(name).unwrap()
    }

    /// `web` placed as lowering places `replicas` in an Environment kept in `fsn`.
    fn lowered(replicas: &Replicas) -> ResolvedServiceSpec {
        let mut spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "f".repeat(32),
            "name": "web",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "postgres:18", "pull_policy": "missing" },
        }))
        .unwrap();
        let mut source = ployz_core::RawVolumeSource::Provisioned {
            name: ployz_core::DockerVolumeName::parse("data").unwrap(),
            maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
                std::num::NonZeroU64::new(ployz_core::STORAGE_GIB).unwrap(),
            ),
            labels: Default::default(),
            birth: None,
        }
        .admit()
        .unwrap();
        source.scope_to_namespace(&namespace());
        let reference = ployz_core::ServiceVolumeReference::parse("data").unwrap();
        let volumes = vec![ployz_core::ServiceVolume {
            reference: reference.clone(),
            source,
        }];
        let mounts = vec![ployz_core::ServiceMount {
            volume: reference,
            target: ployz_core::ContainerPath::parse("/var/lib/postgresql").unwrap(),
            read_only: false,
            no_copy: false,
            subpath: None,
        }];
        let graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();
        spec.set_volume_graph(graph.try_into().unwrap()).unwrap();
        (spec.placement, spec.regions) = lower_placement(replicas, Some(&region("fsn")));
        spec
    }

    fn namespace() -> Namespace {
        Namespace::parse("shop-production").unwrap()
    }

    fn digest(spec: &ResolvedServiceSpec) -> ployz_core::authority::PayloadDigest {
        crate::deploy::kept_query(&namespace(), spec)
            .unwrap()
            .digest
    }

    /// A Service that inherited the Environment's region, moved by a Scale edit of its own
    /// region, starts on the new region's Server under the edit's signature.
    #[test]
    fn a_per_service_region_edit_signs_a_spec_the_move_looks_up() {
        let running = lowered(&Replicas::Count(1));
        let syd = RegionReplicas::new([(region("syd"), std::num::NonZeroU8::MIN)]).unwrap();
        let signed = lowered(&Replicas::PerRegion(syd));

        let looked_up: Vec<_> = starting_specs(running, Some(&region("syd")))
            .iter()
            .map(digest)
            .collect();

        assert!(looked_up.contains(&digest(&signed)), "{looked_up:?}");
    }

    /// And back: a Service with its own region, whose Scale edit returns it to the
    /// Environment's region, finds that Deploy's signature too.
    #[test]
    fn a_return_to_the_environments_region_signs_a_spec_the_move_looks_up() {
        let fsn = RegionReplicas::new([(region("fsn"), std::num::NonZeroU8::MIN)]).unwrap();
        let running = lowered(&Replicas::PerRegion(fsn));
        let mut signed = lowered(&Replicas::Count(1));
        Region::keep(
            Some(&region("syd")),
            &mut signed.placement,
            &mut signed.regions,
        );

        let looked_up: Vec<_> = starting_specs(running, Some(&region("syd")))
            .iter()
            .map(digest)
            .collect();

        assert!(looked_up.contains(&digest(&signed)), "{looked_up:?}");
    }
}
