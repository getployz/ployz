//! A Live Node's variable producers, rescoped so a Branch resolves them as their owner would.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{SavedVariableProducer, SavedVariableValue, ValuePart, ValuePartOwner};
use crate::Namespace;

/// The Namespace that runs a Live Node, with its frozen variable producers.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveValuesOwner {
    #[ts(type = "string")]
    pub namespace: Namespace,
    pub producers: Vec<SavedVariableProducer>,
}

/// A lineage the Branch uses live from the owner, and the keys its templates read.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveLineageUse {
    pub lineage_id: String,
    pub keys: Vec<String>,
}

/// The owner of the Live Nodes a Branch uses, and what the Branch reads from each lineage.
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
/// same lineage neither capture nor shadow them. Private addresses gain the owner's Namespace,
/// unless they already name one (the owner uses that node live itself). Secrets pass through
/// untouched.
#[must_use]
pub fn live_values(input: LiveValuesInput) -> LiveValues {
    let LiveValuesInput { owner, lineages } = input;
    let namespace = owner.namespace;
    let used: BTreeSet<&str> = lineages
        .iter()
        .map(|lineage| lineage.lineage_id.as_str())
        .collect();
    // ponytail: "::" cannot appear in a Namespace, so scoped ids never meet authored ones.
    let scope = |lineage: String| {
        if used.contains(lineage.as_str()) {
            lineage
        } else {
            format!("{namespace}::{lineage}")
        }
    };
    let missing = lineages
        .iter()
        .flat_map(|lineage| {
            lineage.keys.iter().filter_map(|key| {
                let provided = owner.producers.iter().any(|producer| {
                    producer.owner_lineage_id == lineage.lineage_id && producer.key == *key
                });
                (!provided).then(|| MissingLiveValue {
                    lineage_id: lineage.lineage_id.clone(),
                    key: key.clone(),
                })
            })
        })
        .collect();
    let producers = owner
        .producers
        .into_iter()
        .map(|producer| {
            let value = match producer.value {
                SavedVariableValue::Literal { value } if producer.key == PRIVATE_DOMAIN_KEY => {
                    SavedVariableValue::Literal {
                        // An address the owner itself uses live already names its Namespace.
                        value: match value.strip_suffix(".internal") {
                            Some(host) if !host.contains('.') => {
                                format!("{host}.{namespace}.internal")
                            }
                            _ => value,
                        },
                    }
                }
                SavedVariableValue::Template { parts } => SavedVariableValue::Template {
                    parts: parts
                        .into_iter()
                        .map(|part| match part {
                            ValuePart::Ref {
                                owner: ValuePartOwner::Service { lineage_id },
                                key,
                            } => ValuePart::Ref {
                                owner: ValuePartOwner::Service {
                                    lineage_id: scope(lineage_id),
                                },
                                key,
                            },
                            other @ (ValuePart::Text { .. } | ValuePart::Ref { .. }) => other,
                        })
                        .collect(),
                },
                other @ (SavedVariableValue::Literal { .. }
                | SavedVariableValue::Secret { .. }
                | SavedVariableValue::SecretWithoutValue) => other,
            };
            SavedVariableProducer {
                owner_lineage_id: scope(producer.owner_lineage_id),
                value,
                ..producer
            }
        })
        .collect();
    LiveValues { producers, missing }
}
