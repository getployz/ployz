//! A Live Node's variable producers, rescoped so a Branch resolves them as their owner would.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::*;
use crate::ProjectName;

/// The Project that runs a Live Node, with its frozen variable producers.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveValuesOwner {
    pub namespace: String,
    pub producers: Vec<SavedVariableProducer>,
}

/// A lineage the Branch uses live from the owner, and the keys its templates read.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveLineageUse {
    pub lineage_id: String,
    pub keys: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveValuesInput {
    pub owner: LiveValuesOwner,
    pub lineages: Vec<LiveLineageUse>,
}

/// A key the Branch reads from a Live lineage that its owner does not provide.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MissingLiveValue {
    pub lineage_id: String,
    pub key: String,
}

/// Producers to append to the Branch attempt's frozen producers, and the values the owner lacks.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LiveValues {
    pub producers: Vec<SavedVariableProducer>,
    pub missing: Vec<MissingLiveValue>,
}

const PRIVATE_DOMAIN_KEY: &str = "PLOYZ_PRIVATE_DOMAIN";

/// Rescope the owner's producers: used lineages keep their id so the Branch finds them; every
/// other owner lineage moves under the owner's namespace so the Branch's Own Copies of the
/// same lineage neither capture nor shadow them. Private addresses gain the owner's Project.
/// Secrets pass through untouched.
///
/// # Errors
/// Returns ConfigError when the owner namespace is not a usable Project name.
pub fn live_values(input: LiveValuesInput) -> Result<LiveValues, ConfigError> {
    let namespace = ProjectName::parse(&input.owner.namespace)
        .ok()
        .filter(|name| !name.is_reserved())
        .ok_or_else(|| ConfigError::at("owner.namespace", "Invalid Project name"))?;
    let used: BTreeSet<&str> = input
        .lineages
        .iter()
        .map(|lineage| lineage.lineage_id.as_str())
        .collect();
    // ponytail: "::" cannot appear in a Project name, so scoped ids never meet authored ones.
    let scope = |lineage: &str| {
        if used.contains(lineage) {
            lineage.to_owned()
        } else {
            format!("{namespace}::{lineage}")
        }
    };
    let missing = input
        .lineages
        .iter()
        .flat_map(|lineage| {
            lineage.keys.iter().filter_map(|key| {
                let provided = input.owner.producers.iter().any(|producer| {
                    producer.owner_lineage_id == lineage.lineage_id && producer.key == *key
                });
                (!provided).then(|| MissingLiveValue {
                    lineage_id: lineage.lineage_id.clone(),
                    key: key.clone(),
                })
            })
        })
        .collect();
    let producers = input
        .owner
        .producers
        .iter()
        .map(|producer| {
            let value = match &producer.value {
                SavedVariableValue::Literal { value } if producer.key == PRIVATE_DOMAIN_KEY => {
                    SavedVariableValue::Literal {
                        value: value.strip_suffix(".internal").map_or_else(
                            || value.clone(),
                            |host| format!("{host}.{namespace}.internal"),
                        ),
                    }
                }
                SavedVariableValue::Template { parts } => SavedVariableValue::Template {
                    parts: parts
                        .iter()
                        .map(|part| match part {
                            ValuePart::Ref {
                                owner: ValuePartOwner::Service { lineage_id },
                                key,
                            } => ValuePart::Ref {
                                owner: ValuePartOwner::Service {
                                    lineage_id: scope(lineage_id),
                                },
                                key: key.clone(),
                            },
                            other => other.clone(),
                        })
                        .collect(),
                },
                other => other.clone(),
            };
            SavedVariableProducer {
                owner_lineage_id: scope(&producer.owner_lineage_id),
                value,
                ..producer.clone()
            }
        })
        .collect();
    Ok(LiveValues { producers, missing })
}
