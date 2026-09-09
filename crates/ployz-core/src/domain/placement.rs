//! Canonical Machine placement predicates and matching.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Validated, canonical Swarm placement constraint for a Machine ID or Label.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
pub struct PlacementConstraint(String);

/// A malformed constraint or a selector outside the supported Swarm subset.
#[derive(Debug, thiserror::Error)]
#[error(
    "invalid placement constraint {0:?}: expected node.id or node.labels.KEY with == or != and a non-empty Swarm value"
)]
pub struct PlacementConstraintError(String);

impl PlacementConstraint {
    /// Parse and normalize operator spacing and case-insensitive selector/value text.
    ///
    /// # Errors
    /// Rejects malformed input and unsupported selectors or operators.
    pub fn parse(expression: impl Into<String>) -> Result<Self, PlacementConstraintError> {
        let expression = expression.into();
        let parse = || {
            let (key, operator, value) = ["==", "!="].into_iter().find_map(|operator| {
                expression
                    .split_once(operator)
                    .map(|(key, value)| (key.trim(), operator, value.trim()))
            })?;
            // Match Swarm's ASCII key/value grammar; punctuation is literal, never regex.
            if !key.bytes().all(crate::value::is_swarm_key_byte)
                || value.is_empty()
                || !value.bytes().all(crate::value::is_swarm_value_byte)
            {
                return None;
            }
            let key = if key.eq_ignore_ascii_case("node.id") {
                "node.id".to_owned()
            } else {
                let prefix = "node.labels.";
                if key.len() <= prefix.len()
                    || !key.get(..prefix.len())?.eq_ignore_ascii_case(prefix)
                {
                    return None;
                }
                format!("{prefix}{}", &key[prefix.len()..])
            };
            Some(Self(format!(
                "{key}{operator}{}",
                value.to_ascii_lowercase()
            )))
        };
        parse().ok_or(PlacementConstraintError(expression))
    }

    /// Borrow the canonical expression.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Match values case-insensitively; Label keys remain case-sensitive.
    #[must_use]
    pub fn matches(&self, machine: &crate::Machine) -> bool {
        let (key, value, equal) = if let Some((key, value)) = self.0.split_once("==") {
            (key, value, true)
        } else {
            let (key, value) = self
                .0
                .split_once("!=")
                .expect("validated constraint operator");
            (key, value, false)
        };
        let actual = key.strip_prefix("node.labels.").map_or_else(
            || machine.id.as_str(),
            |label| {
                machine
                    .labels
                    .get(label)
                    .map_or("", crate::MachineLabelValue::as_str)
            },
        );
        actual.eq_ignore_ascii_case(value) == equal
    }
}

impl TryFrom<String> for PlacementConstraint {
    type Error = PlacementConstraintError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}
impl From<PlacementConstraint> for String {
    fn from(value: PlacementConstraint) -> Self {
        value.0
    }
}
impl std::str::FromStr for PlacementConstraint {
    type Err = PlacementConstraintError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}
impl std::fmt::Display for PlacementConstraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A conjunction of Machine placement predicates, retained for future Machines.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    /// Empty adds no selector restriction.
    #[serde(default)]
    pub constraints: std::collections::BTreeSet<PlacementConstraint>,
}
