//! Template resolution over supplied producer facts. No storage or provider execution.
use super::{ValuePart, ValuePartOwner};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Plaintext inputs belong only to an authorized server-side resolution call.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResolverValue {
    Literal { value: String },
    Secret { value: String },
    Template { parts: Vec<ValuePart> },
}

/// A supplied variable value keyed by its owner and stable reference scope.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VariableProducer {
    pub owner_id: String,
    pub owner: ValuePartOwner,
    pub key: String,
    pub value: ResolverValue,
}

/// A template and the producer facts available to resolve it.
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveVariablesInput {
    pub parts: Vec<ValuePart>,
    pub self_owner_id: String,
    pub producers: Vec<VariableProducer>,
}

/// A missing producer reference encountered during template resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TemplateWarning {
    #[ts(type = "'missing'")]
    pub kind: String,
    pub owner_id: Option<String>,
    pub key: String,
}

/// Resolved text with secret provenance, or the cycle that prevents resolution.
#[derive(Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResolveVariablesResult {
    Resolved {
        value: String,
        secret: bool,
        warnings: Vec<TemplateWarning>,
    },
    Cycle {
        path: Vec<String>,
    },
}

/// Resolve supplied templates transitively, retaining secret provenance and reporting missing producers or cycles.
#[must_use]
pub fn resolve_variables(input: &ResolveVariablesInput) -> ResolveVariablesResult {
    let mut warnings = Vec::new();
    let mut memo = BTreeMap::new();
    let mut stack = Vec::new();
    match resolve_parts(
        &input.parts,
        &input.self_owner_id,
        &input.producers,
        &mut warnings,
        &mut memo,
        &mut stack,
    ) {
        Ok((value, secret)) => ResolveVariablesResult::Resolved {
            value,
            secret,
            warnings,
        },
        Err(path) => ResolveVariablesResult::Cycle { path },
    }
}

fn resolve_parts(
    parts: &[ValuePart],
    self_owner_id: &str,
    producers: &[VariableProducer],
    warnings: &mut Vec<TemplateWarning>,
    memo: &mut BTreeMap<(String, String), (String, bool)>,
    stack: &mut Vec<(String, String)>,
) -> Result<(String, bool), Vec<String>> {
    let mut value = String::new();
    let mut secret = false;
    for part in parts {
        let ValuePart::Ref { owner, key } = part else {
            if let ValuePart::Text { value: text } = part {
                value.push_str(text);
            }
            continue;
        };
        // ponytail: linear lookup; index by owner/key if large environments make it measurable.
        let found = producers.iter().rev().find(|producer| {
            producer.key == *key
                && match owner {
                    ValuePartOwner::Self_ => producer.owner_id == self_owner_id,
                    ValuePartOwner::Service { .. } | ValuePartOwner::VariableGroup { .. } => {
                        producer.owner == *owner
                    }
                }
        });
        let Some(found) = found else {
            warnings.push(TemplateWarning {
                kind: "missing".into(),
                owner_id: matches!(owner, ValuePartOwner::Self_).then(|| self_owner_id.into()),
                key: key.clone(),
            });
            continue;
        };
        let id = (found.owner_id.clone(), key.clone());
        if stack.contains(&id) {
            return Err(stack
                .iter()
                .chain(std::iter::once(&id))
                .map(|(owner, key)| format!("{owner}::{key}"))
                .collect());
        }
        let resolved = if let Some(cached) = memo.get(&id) {
            cached.clone()
        } else {
            let resolved = match &found.value {
                ResolverValue::Literal { value } => (value.clone(), false),
                ResolverValue::Secret { value } => (value.clone(), true),
                ResolverValue::Template { parts } => {
                    stack.push(id.clone());
                    let result =
                        resolve_parts(parts, &found.owner_id, producers, warnings, memo, stack)?;
                    stack.pop();
                    result
                }
            };
            memo.insert(id, resolved.clone());
            resolved
        };
        value.push_str(&resolved.0);
        secret |= resolved.1;
    }
    Ok((value, secret))
}

/// Evidence of equality, a change, or a comparison that could not be performed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EnvironmentEvidence {
    Same,
    Changed { kind: super::ChangeKind },
    NotChecked { reason: EnvironmentUncheckedReason },
}

/// Why a requested environment value lacks usable comparison evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentUncheckedReason {
    ProviderUnresolved,
    InspectionUnavailable,
}

/// A variable key and its redacted live-comparison evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnvironmentReview {
    pub key: String,
    pub evidence: EnvironmentEvidence,
}

/// Values are compared privately; only key/status leaves this function.
#[must_use]
pub fn review_environment(
    requested: &BTreeMap<String, String>,
    prior_declared: &BTreeMap<String, String>,
    inspected: Option<&BTreeMap<String, String>>,
) -> Vec<EnvironmentReview> {
    requested
        .keys()
        .chain(prior_declared.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|key| {
            let after = requested.get(key);
            let evidence = if after.is_some_and(|value| value.starts_with("secret://")) {
                EnvironmentEvidence::NotChecked {
                    reason: EnvironmentUncheckedReason::ProviderUnresolved,
                }
            } else if let Some(inspected) = inspected {
                let before = inspected.get(key);
                if before == after {
                    EnvironmentEvidence::Same
                } else {
                    EnvironmentEvidence::Changed {
                        kind: if after.is_none() {
                            super::ChangeKind::Remove
                        } else if before.is_none() {
                            super::ChangeKind::Add
                        } else {
                            super::ChangeKind::Update
                        },
                    }
                }
            } else {
                EnvironmentEvidence::NotChecked {
                    reason: EnvironmentUncheckedReason::InspectionUnavailable,
                }
            };
            EnvironmentReview {
                key: key.clone(),
                evidence,
            }
        })
        .collect()
}
