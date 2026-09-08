//! Project authored and runtime evidence into unsaved, pending, and drift reviews.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::*;

type Comparisons = BTreeMap<String, (Value, ReviewSource)>;

/// Use saved evidence, or an introduction only when no applied baseline exists.
#[must_use]
pub fn resolve_working_comparison(
    saved: Option<Value>,
    applied: Option<Value>,
    introduction: Option<Value>,
) -> Value {
    if let Some(value) = saved {
        json!({"role":"saved","value":value})
    } else if applied.is_some() {
        Value::Null
    } else {
        introduction.map_or(
            Value::Null,
            |value| json!({"role":"node_introduction","value":value}),
        )
    }
}

fn projections(state: &ReviewStateProjection) -> BTreeMap<String, &ReviewNodeProjection> {
    state
        .nodes
        .iter()
        .map(|node| (node.node.key(), node))
        .collect()
}

fn presence(config: Option<&Value>) -> ReviewPresence {
    if config.is_some() {
        ReviewPresence::Present
    } else {
        ReviewPresence::Absent
    }
}

fn slice(
    baseline: &ReviewStateProjection,
    baseline_role: ReviewRole,
    target: &ReviewStateProjection,
    target_role: ReviewRole,
    comparisons: Option<&Comparisons>,
    discard_target: Option<ReviewDiscardTarget>,
) -> Result<ReviewChangeSlice, ConfigError> {
    let before = projections(baseline);
    let after = projections(target);
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    let drift = target_role == ReviewRole::RuntimeObservation;
    let mut groups = Vec::new();
    for key in keys {
        let previous = before.get(key).copied();
        let next = after.get(key).copied();
        let node = &next.or(previous).expect("union contains node").node;
        let prior_config = previous.and_then(|n| n.config.as_ref());
        let next_config = next.and_then(|n| n.config.as_ref());
        let default_comparison = prior_config.map(|v| {
            (
                v.clone(),
                ReviewSource {
                    role: baseline_role,
                    token: baseline.token.clone(),
                },
            )
        });
        let comparison = match comparisons {
            Some(map) => map.get(key),
            None => default_comparison.as_ref(),
        };
        let mut settings = Vec::new();
        if let (Some(current), Some((config, source))) = (next_config, comparison) {
            for row in
                compare_resource_settings(node.node_type, current.clone(), Some(config.clone()))?
            {
                if row.path == "node" || row.derived_from.is_some() {
                    continue;
                }
                let owner = ReviewSettingOwner {
                    node: node.clone(),
                    setting: row.path.clone(),
                };
                let resettable = !drift && row.can_restore;
                let discard_plan = discard_target
                    .as_ref()
                    .filter(|_| resettable)
                    .map(|target| ReviewSettingDiscardPlan {
                        kind: ReviewSettingDiscardKind::RestoreSetting,
                        target: target.clone(),
                        owner: owner.clone(),
                        config: config.clone(),
                    });
                settings.push(ReviewSettingChange {
                    id: format!("{key}:{}", row.path),
                    owner,
                    label: None,
                    kind: if drift {
                        ReviewSettingKind::Drift
                    } else {
                        match row.kind {
                            ChangeKind::Add => ReviewSettingKind::Add,
                            ChangeKind::Update => ReviewSettingKind::Update,
                            ChangeKind::Remove => ReviewSettingKind::Remove,
                        }
                    },
                    baseline_value: row.before,
                    target_value: row.after,
                    baseline_source: Some(source.clone()),
                    discard_plan,
                });
            }
        }
        let kind = match (prior_config.is_some(), next_config.is_some()) {
            (false, true) => ReviewLifecycleKind::Create,
            (true, false) => ReviewLifecycleKind::Delete,
            _ if !settings.is_empty() => ReviewLifecycleKind::Update,
            _ => continue,
        };
        groups.push(ReviewNodeChange {
            id: key.clone(),
            node: node.clone(),
            presence: ReviewNodePresence {
                baseline: presence(prior_config),
                target: presence(next_config),
            },
            lifecycle: ReviewLifecycleChange {
                id: format!("{key}:lifecycle"),
                owner: ReviewLifecycleOwner { node: node.clone() },
                kind,
            },
            settings,
            discard_plan: discard_target.as_ref().map(|target| ReviewNodeDiscardPlan {
                kind: if prior_config.is_some() {
                    ReviewNodeDiscardKind::Restore
                } else {
                    ReviewNodeDiscardKind::Delete
                },
                node: node.clone(),
                target: target.clone(),
            }),
        });
    }
    let mut result = ReviewChangeSlice {
        provenance: ReviewProvenance {
            baseline: ReviewSource {
                role: baseline_role,
                token: baseline.token.clone(),
            },
            target: ReviewSource {
                role: target_role,
                token: target.token.clone(),
            },
        },
        groups,
        lifecycle_count: 0,
        setting_count: 0,
        total_count: 0,
        discard_plans: ReviewDiscardPlans {
            nodes: Vec::new(),
            settings: Vec::new(),
        },
    };
    summarize(&mut result);
    Ok(result)
}

fn summarize(slice: &mut ReviewChangeSlice) {
    slice.lifecycle_count = slice
        .groups
        .iter()
        .filter(|g| {
            matches!(
                g.lifecycle.kind,
                ReviewLifecycleKind::Create | ReviewLifecycleKind::Delete
            )
        })
        .count();
    slice.setting_count = slice.groups.iter().map(|g| g.settings.len()).sum();
    slice.total_count = slice.lifecycle_count + slice.setting_count;
    slice.discard_plans.nodes = slice
        .groups
        .iter()
        .filter_map(|g| g.discard_plan.clone())
        .collect();
    slice.discard_plans.settings = slice
        .groups
        .iter()
        .flat_map(|g| g.settings.iter().filter_map(|s| s.discard_plan.clone()))
        .collect();
}

fn runtime_group<'a>(
    groups: &'a mut Vec<ReviewNodeChange>,
    node: &ReviewNodeIdentity,
) -> &'a mut ReviewNodeChange {
    let key = node.key();
    let index = groups.iter().position(|g| g.id == key).unwrap_or_else(|| {
        groups.push(ReviewNodeChange {
            id: key.clone(),
            node: node.clone(),
            presence: ReviewNodePresence {
                baseline: ReviewPresence::Present,
                target: ReviewPresence::Present,
            },
            lifecycle: ReviewLifecycleChange {
                id: format!("{key}:lifecycle"),
                owner: ReviewLifecycleOwner { node: node.clone() },
                kind: ReviewLifecycleKind::Update,
            },
            settings: Vec::new(),
            discard_plan: None,
        });
        groups.len() - 1
    });
    groups
        .get_mut(index)
        .expect("existing or newly inserted group")
}

fn validate_projection(state: &mut ReviewStateProjection) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();
    for node in &mut state.nodes {
        if !seen.insert(node.node.key()) {
            return Err(ConfigError::at(
                "nodes",
                "Projection identities must be unique",
            ));
        }
        if let Some(config) = node.config.take() {
            node.config = Some(parse_resource_config(node.node.node_type, config)?);
        }
    }
    Ok(())
}

/// Project each evidence role into separate owner-aware review and discard results.
///
/// # Errors
/// Returns ConfigError when a supplied node configuration is invalid for its owner type.
pub fn project_environment_changes(
    mut input: ChangeSetInput,
) -> Result<ReviewChangeSet, ConfigError> {
    let mut saved = input.saved.state();
    for state in [
        &mut input.working,
        &mut saved,
        &mut input.applied,
        &mut input.node_introductions,
    ] {
        validate_projection(state)?;
    }
    if let Some(state) = &mut input.runtime_observed {
        validate_projection(state)?;
    }
    let unavailable = ReviewStateProjection {
        token: "runtime:unavailable".into(),
        nodes: input.applied.nodes.clone(),
    };
    let mut drift = slice(
        &input.applied,
        ReviewRole::Applied,
        input.runtime_observed.as_ref().unwrap_or(&unavailable),
        ReviewRole::RuntimeObservation,
        None,
        None,
    )?;
    if let (Some(_), Some(observations)) = (&input.runtime_observed, input.runtime_observations) {
        for observed in observations.settings {
            let group = runtime_group(&mut drift.groups, &observed.node);
            let id = format!("{}:{}", observed.node.key(), observed.setting);
            if group.settings.iter().any(|s| s.id == id) {
                continue;
            }
            group.settings.push(ReviewSettingChange {
                id,
                owner: ReviewSettingOwner {
                    node: observed.node,
                    setting: observed.setting,
                },
                label: Some(observed.label),
                kind: ReviewSettingKind::Drift,
                baseline_value: json!(observed.applied_value),
                target_value: json!(observed.observed_value),
                baseline_source: Some(ReviewSource {
                    role: ReviewRole::Applied,
                    token: input.applied.token.clone(),
                }),
                discard_plan: None,
            });
        }
        for observed in observations.presence.into_iter().flatten() {
            if observed.applied == observed.observed {
                continue;
            }
            let group = runtime_group(&mut drift.groups, &observed.node);
            group.presence = ReviewNodePresence {
                baseline: observed.applied,
                target: observed.observed,
            };
            group.lifecycle.kind = if observed.applied == ReviewPresence::Present {
                ReviewLifecycleKind::Delete
            } else {
                ReviewLifecycleKind::Create
            };
        }
        drift.groups.sort_by(|a, b| a.id.cmp(&b.id));
        for group in &mut drift.groups {
            group.settings.sort_by(|a, b| a.id.cmp(&b.id));
        }
        summarize(&mut drift);
        drift.provenance.target.token = observations.token;
    }
    let saved_nodes = projections(&saved);
    let applied_nodes = projections(&input.applied);
    let introductions = projections(&input.node_introductions);
    let mut comparisons = Comparisons::new();
    for working in &input.working.nodes {
        let key = working.node.key();
        let saved_config = saved_nodes.get(&key).and_then(|n| n.config.as_ref());
        let applied_config = applied_nodes.get(&key).and_then(|n| n.config.as_ref());
        let introduction = introductions.get(&key).and_then(|n| n.config.as_ref());
        let comparison = resolve_working_comparison(
            saved_config.cloned(),
            applied_config.cloned(),
            introduction.cloned(),
        );
        if comparison.is_null() {
            continue;
        }
        let from_saved = comparison.get("role").and_then(Value::as_str) == Some("saved");
        comparisons.insert(
            key,
            (
                comparison.get("value").cloned().unwrap_or_default(),
                ReviewSource {
                    role: if from_saved {
                        ReviewRole::Saved
                    } else {
                        ReviewRole::NodeIntroduction
                    },
                    token: if from_saved {
                        saved.token.clone()
                    } else {
                        input.node_introductions.token.clone()
                    },
                },
            ),
        );
    }
    let saved_target = match input.saved {
        ReviewSavedProjection::NoSavedState { .. } => None,
        ReviewSavedProjection::SavedRevision {
            saved_state_snapshot_id,
            ..
        } => Some(ReviewDiscardTarget::Saved {
            basis: ReviewSavedBasis::SavedRevision {
                saved_state_snapshot_id,
            },
        }),
    };
    let unsaved = slice(
        &saved,
        ReviewRole::Saved,
        &input.working,
        ReviewRole::Working,
        Some(&comparisons),
        Some(ReviewDiscardTarget::Working),
    )?;
    let pending = slice(
        &input.applied,
        ReviewRole::Applied,
        &saved,
        ReviewRole::Saved,
        None,
        saved_target,
    )?;
    let discard_all_plan = aggregate_discard_plan(&unsaved, &pending)?;
    Ok(ReviewChangeSet {
        unsaved,
        pending,
        drift,
        discard_all_plan,
    })
}

fn aggregate_discard_plan(
    unsaved: &ReviewChangeSlice,
    pending: &ReviewChangeSlice,
) -> Result<ReviewAggregateDiscardPlan, ConfigError> {
    let mut plans: BTreeMap<_, _> = unsaved
        .discard_plans
        .nodes
        .iter()
        .map(|p| (p.node.key(), p))
        .collect();
    plans.extend(
        pending
            .discard_plans
            .nodes
            .iter()
            .map(|p| (p.node.key(), p)),
    );
    let mut plans: Vec<_> = plans.into_values().collect();
    plans.sort_by_key(|p| {
        (
            p.node.node_type == EnvironmentNodeType::Service,
            p.node.key(),
        )
    });
    let mut saved_command: Option<ReviewSavedDiscardCommand> = None;
    let mut nodes = Vec::new();
    for plan in plans {
        nodes.push(ReviewAggregateNodePlan {
            node: plan.node.clone(),
            working: ReviewWorkingNodePlan {
                kind: plan.kind,
                target: ReviewWorkingTarget::Working,
                node: plan.node.clone(),
            },
        });
        if let ReviewDiscardTarget::Saved { basis } = &plan.target {
            let command = saved_command.get_or_insert_with(|| ReviewSavedDiscardCommand {
                kind: ReviewSavedCommandKind::Discard,
                basis: basis.clone(),
                operations: Vec::new(),
            });
            if &command.basis != basis {
                return Err(ConfigError::at(
                    "basis",
                    "Discard plans must share one publication basis",
                ));
            }
            command.operations.push(ReviewSavedNodeOperation {
                kind: ReviewSavedOperationKind::Node,
                node_type: plan.node.node_type,
                node_id: plan.node.id.clone(),
            });
        }
    }
    Ok(ReviewAggregateDiscardPlan {
        nodes,
        saved_command,
    })
}
