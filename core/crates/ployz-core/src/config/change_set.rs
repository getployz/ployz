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
    head: &ReviewStateProjection,
    working: &ReviewStateProjection,
    introductions: &BTreeMap<String, &ReviewNodeProjection>,
) -> Result<Vec<ReviewNodeChange>, ConfigError> {
    let before = projections(head);
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
        let (comparison, role) = match previous {
            Some(config) => (Some(config), Some(ReviewComparisonRole::Head)),
            None => match introductions.get(key).and_then(|node| node.config.as_ref()) {
                Some(config) => (Some(config), Some(ReviewComparisonRole::Introduction)),
                None => (None, None),
            },
        };
        let settings: Vec<_> = match (next, comparison) {
            (Some(current), Some(baseline)) => {
                compare_resource_settings(node.node_type, current.clone(), Some(baseline.clone()))?
                    .into_iter()
                    .filter(|row| row.path != "node")
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
            comparison: role,
            settings,
        });
    }
    Ok(groups)
}

pub fn project_environment_changes(input: ChangeSetInput) -> Result<ReviewChangeSet, ConfigError> {
    let head = input.submitted.as_ref().unwrap_or(&input.applied);
    let introductions = projections(&input.node_introductions);
    let groups = compare(head, &input.working, &introductions)?;
    let total_count = groups
        .iter()
        .map(|group| {
            group.settings.len() + usize::from(group.lifecycle != ReviewLifecycleKind::Update)
        })
        .sum();
    Ok(ReviewChangeSet {
        groups,
        total_count,
        head_token: head.token.clone(),
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
            assert_eq!(
                review.head_token,
                format!("{:?}", submitted.or(Some(applied)))
            );
        }
    }

    #[test]
    fn new_node_compares_against_its_introduction_until_head_has_it() {
        let review = project_environment_changes(ChangeSetInput {
            working: state(Some(7)),
            applied: state(None),
            submitted: None,
            node_introductions: state(Some(1)),
        })
        .unwrap();
        let group = review.groups.first().expect("new node has a change group");
        assert_eq!(group.lifecycle, ReviewLifecycleKind::Create);
        assert_eq!(group.comparison, Some(ReviewComparisonRole::Introduction));
        assert_eq!(
            group
                .settings
                .iter()
                .map(|row| (row.before.clone(), row.after.clone()))
                .collect::<Vec<_>>(),
            vec![(json!(1), json!(7))]
        );
        assert_eq!(review.total_count, 2);
    }
}
