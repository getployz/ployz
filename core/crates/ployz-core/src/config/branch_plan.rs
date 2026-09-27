//! Branch planning: which Parent nodes a Branch copies, uses live, or leaves out.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    ConfigError, EnvironmentNodeType, SavedEnvironmentIntent, SavedVariableValue, ValuePart,
    ValuePartOwner,
};
use crate::ProjectName;

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

/// What the Branch does with a Parent node.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BranchNodeRole {
    Own,
    Live,
    LeftOut,
}

/// Why a node is an Own Copy or Live.
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
    pub role: BranchNodeRole,
    pub because: Option<BranchNodeReason>,
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
    // Owned nodes in document order: services, then volumes.
    let mut nodes: Vec<(String, EnvironmentNodeType)> = parent
        .services
        .iter()
        .map(|s| (s.lineage_id.clone(), EnvironmentNodeType::Service))
        .chain(
            parent
                .volumes
                .iter()
                .map(|v| (v.resource_lineage_id.clone(), EnvironmentNodeType::Volume)),
        )
        .collect();
    let owned: BTreeSet<String> = nodes.iter().map(|(id, _)| id.clone()).collect();

    let mut links: Vec<(String, String)> = Vec::new();
    let mut parent_live = BTreeSet::new();
    for service in &parent.services {
        for variable in &service.variables {
            let SavedVariableValue::Template { parts } = &variable.value else {
                continue;
            };
            for part in parts {
                if let ValuePart::Ref {
                    owner: ValuePartOwner::Service { lineage_id },
                    ..
                } = part
                    && *lineage_id != service.lineage_id
                {
                    if !owned.contains(lineage_id) {
                        parent_live.insert(lineage_id.clone());
                    }
                    links.push((service.lineage_id.clone(), lineage_id.clone()));
                }
            }
        }
        for attachment in &service.volume_attachments {
            if let Some(volume) = parent
                .volumes
                .iter()
                .find(|v| v.resource_id == attachment.volume_resource_id)
            {
                links.push((
                    service.lineage_id.clone(),
                    volume.resource_lineage_id.clone(),
                ));
            }
        }
    }

    let known = |ids: &[String], path: &str| {
        if ids.iter().all(|id| owned.contains(id)) {
            Ok(())
        } else {
            Err(ConfigError::at(path, "Unknown lineage in the Parent"))
        }
    };
    known(focus, "focus")?;
    let preset_picks = |preset: BranchPreset| -> BTreeSet<String> {
        match preset {
            BranchPreset::Only => focus.iter().cloned().collect(),
            BranchPreset::All => owned.clone(),
            BranchPreset::Uses => {
                let mut set: BTreeSet<String> = focus.iter().cloned().collect();
                let mut changed = true;
                while changed {
                    changed = false;
                    for (user, used) in &links {
                        if set.contains(user) && owned.contains(used) {
                            changed |= set.insert(used.clone());
                        }
                    }
                }
                set
            }
        }
    };
    let picked = match picks {
        BranchPicks::Preset { preset } => preset_picks(*preset),
        BranchPicks::Own { own } => {
            known(own, "picks.own")?;
            own.iter().cloned().collect()
        }
    };

    let deployed: BTreeSet<&str> = deployed.iter().map(String::as_str).collect();
    let volume = |id: &str| {
        nodes
            .iter()
            .any(|(n, t)| n == id && *t == EnvironmentNodeType::Volume)
    };
    let mut roles: BTreeMap<String, (BranchNodeRole, Option<BranchNodeReason>)> = owned
        .iter()
        .map(|id| {
            let role = if picked.contains(id) {
                (BranchNodeRole::Own, Some(BranchNodeReason::Picked))
            } else {
                (BranchNodeRole::LeftOut, None)
            };
            (id.clone(), role)
        })
        .collect();
    for id in &parent_live {
        roles.insert(
            id.clone(),
            (BranchNodeRole::Live, Some(BranchNodeReason::Used)),
        );
    }
    let mut changed = true;
    while changed {
        changed = false;
        for (user, used) in &links {
            let role = |id: &String| roles.get(id).map(|(role, _)| *role);
            if role(user) == Some(BranchNodeRole::Own)
                && role(used) == Some(BranchNodeRole::LeftOut)
            {
                roles.insert(
                    used.clone(),
                    if volume(used) {
                        (BranchNodeRole::Own, Some(BranchNodeReason::Used))
                    } else if deployed.contains(used.as_str()) {
                        (BranchNodeRole::Live, Some(BranchNodeReason::Used))
                    } else {
                        (
                            BranchNodeRole::Own,
                            Some(BranchNodeReason::ParentNotDeployed),
                        )
                    },
                );
                changed = true;
            }
        }
    }

    nodes.extend(
        parent_live
            .into_iter()
            .map(|id| (id, EnvironmentNodeType::Service)),
    );
    Ok(BranchPlan {
        nodes: nodes
            .into_iter()
            .map(|(lineage_id, node_type)| {
                let (role, because) = roles
                    .get(&lineage_id)
                    .copied()
                    .expect("every planned node has a role");
                BranchPlanNode {
                    lineage_id,
                    node_type,
                    role,
                    because,
                }
            })
            .collect(),
        preset: PRESETS.into_iter().find(|p| preset_picks(*p) == picked),
    })
}

/// Admit a Branch's Project namespace under the runtime's Project name rule.
///
/// # Errors
/// Returns ConfigError saying why the name would fail at deploy.
pub fn check_branch_name(name: &str) -> Result<ProjectName, ConfigError> {
    let why = if name.is_empty() {
        "Project name is empty"
    } else if name.len() > 63 {
        "Project name is longer than 63 characters"
    } else {
        match ProjectName::parse(name) {
            Ok(name) if name.is_reserved() => "Project name is reserved for the system Project",
            Ok(name) => return Ok(name),
            Err(_) => "Project name must be a lowercase DNS label",
        }
    };
    Err(ConfigError::at("projectName", why))
}
