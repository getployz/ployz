//! Wire types for moving changes between authored configurations.

use std::fmt;

use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use ts_rs::TS;

use super::{SavedEnvironmentIntent, SavedVariableValue};

/// The sides of one move: changes flow from `from` into `into`, judged against `base`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchChangesInput {
    /// Null when creating: nothing is shared yet.
    #[ts(as = "Option<SavedEnvironmentIntent>")]
    pub base: Option<Value>,
    #[ts(as = "SavedEnvironmentIntent")]
    pub from: Value,
    #[ts(as = "SavedEnvironmentIntent")]
    pub into: Value,
    /// The Parent, when the caller can offer its values as a variable choice.
    #[serde(default)]
    #[ts(optional, as = "Option<SavedEnvironmentIntent>")]
    pub parent: Option<Value>,
    /// Lineages `into` may use live.
    pub provided: Vec<String>,
    pub hostnames: BranchHostnames,
    pub from_kept: bool,
    /// Row keys marked Never sync, each covering the rows under it too
    /// (`<lineage>:healthcheck` covers `<lineage>:healthcheck.path`): what would move
    /// is shown as meant to differ instead.
    #[serde(default)]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub never_synced: Vec<String>,
    /// Absent compares only; present moves the picked rows.
    #[serde(default)]
    #[ts(optional)]
    pub picks: Option<Vec<BranchPick>>,
}

/// One chosen move row. Only a variable row takes a choice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchPick {
    pub key: String,
    #[serde(default)]
    #[ts(optional)]
    pub choice: Option<BranchPickChoice>,
}

/// How a picked variable lands. Only `new` carries a value, and it is never reviewed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "option", rename_all = "snake_case", deny_unknown_fields)]
pub enum BranchPickChoice {
    From,
    Parent,
    /// Without a value, a secret lands without one, and the receiver's Deploy waits for it.
    New {
        #[serde(default)]
        #[ts(optional)]
        value: Option<BranchNewValue>,
    },
    LeaveOut,
}

impl BranchPickChoice {
    pub(super) const fn option(&self) -> BranchOption {
        match self {
            Self::From => BranchOption::From,
            Self::Parent => BranchOption::Parent,
            Self::New { .. } => BranchOption::New,
            Self::LeaveOut => BranchOption::LeaveOut,
        }
    }
}

/// A caller-supplied variable value; core never encrypts or fingerprints. A secret's is sealed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchNewValue {
    pub value: SavedVariableValue,
    pub value_fingerprint: String,
}

/// Each side's generated-address suffix, appended to managed hostname prefixes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchHostnames {
    pub from: String,
    pub into: String,
}

/// The rows between two configurations, the receiver after moving, and the advanced base.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchChanges {
    pub rows: Vec<BranchRow>,
    pub next: SavedEnvironmentIntent,
    pub base: Option<SavedEnvironmentIntent>,
    /// Canonical, id-free rendering of the rows and picks; callers hash it.
    pub review: String,
}

/// One setting of one lineage. Values are redacted: secrets carry only fingerprints.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchRow {
    /// `<lineageId>:<path>`.
    #[ts(type = "string")]
    pub key: BranchRowKey,
    #[serde(flatten)]
    #[ts(flatten)]
    pub role: BranchRole,
    pub base: Value,
    pub from: Value,
    pub into: Value,
}

/// A row's lineage and setting path; it serializes as `<lineageId>:<path>`.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BranchRowKey {
    pub(super) lineage: String,
    pub(super) path: RowPath,
}

/// Where in a node a row sits.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum RowPath {
    Node,
    Data,
    Name,
    Storage,
    Mount(String),
    Variable(String),
    Setting(&'static str),
}

impl fmt::Display for BranchRowKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let lineage = &self.lineage;
        match &self.path {
            RowPath::Node => write!(f, "{lineage}:node"),
            RowPath::Data => write!(f, "{lineage}:data"),
            RowPath::Name => write!(f, "{lineage}:name"),
            RowPath::Storage => write!(f, "{lineage}:storage"),
            RowPath::Mount(volume) => write!(f, "{lineage}:mounts.{volume}"),
            RowPath::Variable(key) => write!(f, "{lineage}:variables.{key}"),
            RowPath::Setting(path) => write!(f, "{lineage}:{path}"),
        }
    }
}

impl Serialize for BranchRowKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// A row either moves (possibly over a conflicting change) or is meant to differ.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(
    tag = "role",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum BranchRole {
    Move {
        /// `into` also changed since `base`; shown into → from.
        conflict: bool,
        /// Present only on variable rows.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        choice: Option<BranchChoice>,
    },
    Differ {
        why: BranchReason,
    },
}

/// Why a row never moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BranchReason {
    Live,
    LeftOut,
    Sizing,
    CustomDomain,
    GeneratedAddress,
    GitBranch,
    Data,
    /// Marked Never sync in one of the two Environments.
    NeverSynced,
}

/// The ways a moving variable can land.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchChoice {
    pub default: BranchOption,
    pub options: Vec<BranchOption>,
    pub secret: bool,
}

/// A variable's source when it moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BranchOption {
    From,
    Parent,
    New,
    LeaveOut,
}
