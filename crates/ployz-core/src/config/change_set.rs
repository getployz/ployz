//! Compare Working State with submitted intent, or confirmed Applied State.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn projections(state: &ReviewStateProjection) -> BTreeMap<String, &ReviewNodeProjection> {
    state
        .nodes
        .iter()
        .map(|node| (node.node.key(), node))
        .collect()
}

fn compare(
    baseline: &ReviewStateProjection,
    working: &ReviewStateProjection,
    introductions: &BTreeMap<String, &ReviewNodeProjection>,
) -> Result<Vec<ReviewNodeChange>, ConfigError> {
    let before = projections(baseline);
    let after = projections(working);
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    let mut groups = Vec::new();
    for key in keys {
        let previous = before.get(key).and_then(|node| node.config.as_ref());
        let next = after.get(key).and_then(|node| node.config.as_ref());
        let node = &after
            .get(key)
            .or_else(|| before.get(key))
            .expect("union contains node")
            .node;
        let comparison =
            previous.or_else(|| introductions.get(key).and_then(|node| node.config.as_ref()));
        let settings: Vec<_> = match (next, comparison) {
            (Some(current), Some(baseline)) => {
                compare_resource_settings(node.node_type, current.clone(), Some(baseline.clone()))?
                    .into_iter()
                    .filter(|row| row.path != "node" && row.derived_from.is_none())
                    .collect()
            }
            _ => Vec::new(),
        };
        let lifecycle = match (previous.is_some(), next.is_some()) {
            (false, true) => ReviewLifecycleKind::Create,
            (true, false) => ReviewLifecycleKind::Delete,
            (true, true) if !settings.is_empty() => ReviewLifecycleKind::Update,
            _ => continue,
        };
        groups.push(ReviewNodeChange {
            node: node.clone(),
            lifecycle,
            settings,
        });
    }
    Ok(groups)
}

pub fn project_environment_changes(input: ChangeSetInput) -> Result<ReviewChangeSet, ConfigError> {
    let baseline = input.submitted.as_ref().unwrap_or(&input.applied);
    let saved = projections(&input.saved);
    let applied = projections(&input.applied);
    let introductions = projections(&input.node_introductions)
        .into_iter()
        .filter(|(key, _)| {
            saved
                .get(key)
                .and_then(|node| node.config.as_ref())
                .is_none()
                && applied
                    .get(key)
                    .and_then(|node| node.config.as_ref())
                    .is_none()
        })
        .collect();
    let groups = compare(baseline, &input.working, &introductions)?;
    let total_count = groups
        .iter()
        .map(|group| {
            group.settings.len() + usize::from(group.lifecycle != ReviewLifecycleKind::Update)
        })
        .sum();
    let can_save = !compare(&input.saved, &input.working, &BTreeMap::new())?.is_empty();
    Ok(ReviewChangeSet {
        groups,
        total_count,
        can_save,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn state(replicas: Option<u32>) -> ReviewStateProjection {
        ReviewStateProjection {
            token: format!("{replicas:?}"),
            nodes: vec![ReviewNodeProjection {
                node: ReviewNodeIdentity {
                    node_type: EnvironmentNodeType::Service,
                    id: "api".into(),
                },
                config: replicas.map(|replicas| {
                    serde_json::to_value(
                        parse_service_config(json!({
                            "version": 2, "name": "API", "privateDns": "api", "replicas": replicas,
                            "source": {"version": 1, "type": "empty", "rootDir": "/"},
                            "healthcheck": {"type": "none"}, "restartPolicy": "unless-stopped"
                        }))
                        .unwrap(),
                    )
                    .unwrap()
                }),
            }],
        }
    }

    #[test]
    fn deployment_lifecycle_has_one_actionable_diff() {
        for (applied, submitted, working, expected) in [
            (1, Some(5), 5, vec![]),
            (1, Some(5), 7, vec![(json!(5), json!(7))]),
            (1, None, 5, vec![(json!(1), json!(5))]),
            (1, None, 7, vec![(json!(1), json!(7))]),
            (5, None, 7, vec![(json!(5), json!(7))]),
            (5, None, 5, vec![]),
        ] {
            let review = project_environment_changes(ChangeSetInput {
                working: state(Some(working)),
                saved: state(Some(5)),
                applied: state(Some(applied)),
                submitted: submitted.map(|n| state(Some(n))),
                node_introductions: state(None),
            })
            .unwrap();
            let rows: Vec<(Value, Value)> = review
                .groups
                .iter()
                .flat_map(|group| &group.settings)
                .map(|row| (row.before.clone(), row.after.clone()))
                .collect();
            assert_eq!(rows, expected);
            assert_eq!(review.total_count, expected.len());
            assert_eq!(review.can_save, working != 5);
        }
    }
}
