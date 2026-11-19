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

    /// The normalized text: no spaces around the operator, `node.id` or `node.labels.` in
    /// lowercase, and a lowercase value.
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
pub struct Placement {
    /// Empty adds no selector restriction.
    #[serde(default)]
    pub constraints: std::collections::BTreeSet<PlacementConstraint>,
}

/// What the rule for a Cluster's first Server reads of one Server. The rule is handed every
/// Server, the ones not read too, since one of those may be the first.
#[derive(Clone, Copy, Debug)]
pub enum JoinedServer<'a> {
    Read {
        subnet: crate::MachineSubnet,
        /// When its join was approved, if the Machine record keeps it.
        joined: Option<crate::authority::Timestamp>,
        /// Its `region` label's value, if it has one.
        label: Option<&'a str>,
    },
    /// Its Machine record is incomplete where the Servers were read, so its region is unknown.
    Unread,
}

impl<'a> JoinedServer<'a> {
    /// Every Server in `frame`: each Machine it read, and each whose record it holds incomplete.
    pub fn all(frame: &'a crate::RuntimeWatchFrame) -> impl Iterator<Item = Self> + 'a {
        let read = frame.machines.iter().map(|observed| Self::Read {
            subnet: observed.machine.subnet,
            joined: observed.machine.joined_at(),
            label: observed
                .machine
                .labels
                .get(Region::LABEL)
                .map(|value| value.as_str()),
        });
        let unread = frame.incomplete_ids.machines.iter().map(|_| Self::Unread);
        read.chain(unread)
    }
}

/// Where a Server is, as its `region` Label says: `fsn`, `syd`. Lowercase, as a
/// placement constraint compares it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
pub struct Region(String);

impl Region {
    /// The Machine Label a region is read from.
    pub const LABEL: &'static str = "region";

    /// The longest a region can be: a DNS label's length.
    pub const MAX_LEN: usize = 63;

    /// What a region's name must be.
    pub const NAME_RULE: &'static str = "A region's name is 1 to 40 characters on one line.";

    /// A region's display name: `value` trimmed, when that is 1 to 40 characters on one line.
    /// The Store, the dashboard and the agent all take their rule from here.
    #[must_use]
    pub fn display_name(value: &str) -> Option<&str> {
        let value = value.trim();
        ((1..=40).contains(&value.chars().count()) && !value.chars().any(char::is_control))
            .then_some(value)
    }

    /// What [`Self::parse`] accepts, as a regular expression for a schema to quote. The CLI,
    /// the Store, the dashboard and the agent all take their rule from `parse`.
    pub const PATTERN: &'static str = "^[A-Za-z0-9_.-]{1,63}$";

    /// # Errors
    /// Rejects text that is not 1 to 63 letters, digits, hyphens, underscores and dots.
    pub fn parse(value: impl Into<String>) -> Result<Self, crate::ValueError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > Self::MAX_LEN
            || !value.bytes().all(crate::value::is_swarm_key_byte)
        {
            return Err(crate::ValueError::new(
                "region",
                value,
                "up to 63 letters, digits, hyphens, underscores and dots",
            ));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    /// The region a new Environment starts in: that of the Cluster's first Server. None when
    /// any Server has no region or went unread, since a Cluster that predates regions, or one a
    /// Server from before them is still in, cannot say which Server was first, and an Environment
    /// then runs in any region. Otherwise the first is the one with the earliest recorded join
    /// time. A Server with none was there before join approvals were kept, or founded the
    /// Cluster, so it comes first. Servers that tie, or none of which has a join time, are told
    /// apart by the lowest Machine Subnet, which allocation hands out first and a removed Server
    /// frees.
    #[must_use]
    pub fn of_first_server<'a>(
        servers: impl IntoIterator<Item = JoinedServer<'a>>,
    ) -> Option<Self> {
        let mut first = None;
        for server in servers {
            let JoinedServer::Read {
                subnet,
                joined,
                label,
            } = server
            else {
                return None;
            };
            let region = Self::parse(label?).ok()?;
            let order = (joined, subnet);
            if first.as_ref().is_none_or(|(held, _)| order < *held) {
                first = Some((order, region));
            }
        }
        first.map(|(_, region)| region)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The region `machine` is in, when its Labels name one.
    #[must_use]
    pub fn of(machine: &crate::Machine) -> Option<Self> {
        Self::parse(machine.labels.get(Self::LABEL)?.as_str()).ok()
    }

    /// The constraint only Servers in this region meet.
    #[must_use]
    pub fn constraint(&self) -> PlacementConstraint {
        PlacementConstraint(format!("node.labels.{}=={}", Self::LABEL, self.0))
    }

    /// The region `constraint` keeps a Service in, when that is what it says.
    #[must_use]
    pub fn required_by(constraint: &PlacementConstraint) -> Option<Self> {
        let value = constraint
            .as_str()
            .strip_prefix("node.labels.")?
            .strip_prefix(Self::LABEL)?
            .strip_prefix("==")?;
        Some(Self(value.to_owned()))
    }
}

impl TryFrom<String> for Region {
    type Error = crate::ValueError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}
impl From<Region> for String {
    fn from(value: Region) -> Self {
        value.0
    }
}
impl std::str::FromStr for Region {
    type Err = crate::ValueError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}
impl std::fmt::Display for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
