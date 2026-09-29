//! Branch planning: which Parent nodes a Branch copies, uses live, or leaves out.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{ConfigError, EnvironmentNodeType, SavedEnvironmentIntent};

/// A starting selection: every preset derives its picks from the focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BranchPreset {
    Only,
    Uses,
    All,
}

const PRESETS: [BranchPreset; 3] = [BranchPreset::Only, BranchPreset::Uses, BranchPreset::All];

/// The nodes a caller wants as Own Copies, by preset or by hand.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged, deny_unknown_fields)]
pub enum BranchPicks {
    Preset { preset: BranchPreset },
    Own { own: Vec<String> },
}

/// What the Branch does with a Parent node; only an Own Copy says why.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum BranchNodeRole {
    Own {
        because: BranchNodeReason,
    },
    /// Used from the Parent while it runs.
    Live,
    LeftOut,
}

/// Why a node is an Own Copy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BranchNodeReason {
    Picked,
    Used,
    ParentNotDeployed,
}

/// One Parent node's place in the Branch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchPlanNode {
    pub lineage_id: String,
    pub node_type: EnvironmentNodeType,
    #[serde(flatten)]
    #[ts(flatten)]
    pub role: BranchNodeRole,
}

/// Every Parent node's role, and the preset the picks match (`null` = picked by hand).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchPlan {
    pub nodes: Vec<BranchPlanNode>,
    pub preset: Option<BranchPreset>,
}

/// Plan a Branch of `parent`; `deployed` lists the lineages the Parent runs.
///
/// # Errors
/// Returns ConfigError when focus or picks name a lineage the Parent does not own.
pub fn plan_branch(
    parent: &SavedEnvironmentIntent,
    deployed: &[String],
    focus: &[String],
    picks: &BranchPicks,
) -> Result<BranchPlan, ConfigError> {
    let services: Vec<&str> = parent
        .services
        .iter()
        .map(|s| s.lineage_id.as_str())
        .collect();
    let volumes: BTreeSet<&str> = parent
        .volumes
        .iter()
        .map(|v| v.resource_lineage_id.as_str())
        .collect();
    let owned: BTreeSet<&str> = services
        .iter()
        .copied()
        .chain(volumes.iter().copied())
        .collect();
    let links = links(parent);
    // Lineages the Parent itself uses live stay live in the Branch.
    let parent_live: BTreeSet<&str> = links
        .iter()
        .map(|(_, used)| *used)
        .filter(|used| !owned.contains(used))
        .collect();

    let known = |ids: &[String], path: &str| {
        if ids.iter().all(|id| owned.contains(id.as_str())) {
            Ok(())
        } else {
            Err(ConfigError::at(path, "Unknown lineage in the Parent"))
        }
    };
    known(focus, "focus")?;
    let focus: BTreeSet<&str> = focus.iter().map(String::as_str).collect();
    let preset_picks = |preset: BranchPreset| -> BTreeSet<&str> {
        match preset {
            BranchPreset::Only => focus.clone(),
            BranchPreset::All => owned.clone(),
            BranchPreset::Uses => {
                let mut set = focus.clone();
                close_over(&links, |user, used| {
                    set.contains(user) && owned.contains(used) && set.insert(used)
                });
                set
            }
        }
    };
    let picked = match picks {
        BranchPicks::Preset { preset } => preset_picks(*preset),
        BranchPicks::Own { own } => {
            known(own, "picks.own")?;
            own.iter().map(String::as_str).collect()
        }
    };

    let deployed: BTreeSet<&str> = deployed.iter().map(String::as_str).collect();
    let mut roles: BTreeMap<&str, BranchNodeRole> = owned
        .iter()
        .map(|id| {
            let role = if picked.contains(id) {
                BranchNodeRole::Own {
                    because: BranchNodeReason::Picked,
                }
            } else {
                BranchNodeRole::LeftOut
            };
            (*id, role)
        })
        .chain(parent_live.iter().map(|id| (*id, BranchNodeRole::Live)))
        .collect();
    close_over(&links, |user, used| {
        let own = matches!(roles.get(user), Some(BranchNodeRole::Own { .. }));
        if !own || roles.get(used) != Some(&BranchNodeRole::LeftOut) {
            return false;
        }
        let because = if volumes.contains(used) {
            BranchNodeReason::Used
        } else if deployed.contains(used) {
            roles.insert(used, BranchNodeRole::Live);
            return true;
        } else {
            BranchNodeReason::ParentNotDeployed
        };
        roles.insert(used, BranchNodeRole::Own { because });
        true
    });

    // Owned nodes in document order (services, then volumes), then the Parent's live lineages.
    let typed = services
        .iter()
        .map(|id| (*id, EnvironmentNodeType::Service))
        .chain(
            parent
                .volumes
                .iter()
                .map(|v| (v.resource_lineage_id.as_str(), EnvironmentNodeType::Volume)),
        )
        .chain(
            parent_live
                .iter()
                .map(|id| (*id, EnvironmentNodeType::Service)),
        );
    Ok(BranchPlan {
        nodes: typed
            .map(|(lineage_id, node_type)| BranchPlanNode {
                lineage_id: lineage_id.to_owned(),
                node_type,
                role: *roles
                    .get(lineage_id)
                    .expect("every planned node has a role"),
            })
            .collect(),
        preset: PRESETS.into_iter().find(|p| preset_picks(*p) == picked),
    })
}

/// Every (user, used) lineage pair: variable references to other services, and mounts.
fn links(parent: &SavedEnvironmentIntent) -> Vec<(&str, &str)> {
    let mut links = Vec::new();
    for service in &parent.services {
        let user = service.lineage_id.as_str();
        for variable in &service.variables {
            links.extend(
                variable
                    .value
                    .referenced_lineages()
                    .filter(|used| *used != user)
                    .map(|used| (user, used)),
            );
        }
        for attachment in &service.volume_attachments {
            if let Some(volume) = parent
                .volumes
                .iter()
                .find(|v| v.resource_id == attachment.volume_resource_id)
            {
                links.push((user, volume.resource_lineage_id.as_str()));
            }
        }
    }
    links
}

/// Apply `step` to every link until a full pass changes nothing.
fn close_over<'a>(links: &[(&'a str, &'a str)], mut step: impl FnMut(&'a str, &'a str) -> bool) {
    while links
        .iter()
        .fold(false, |changed, (user, used)| step(user, used) | changed)
    {}
}
