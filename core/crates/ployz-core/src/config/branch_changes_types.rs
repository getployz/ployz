//! Wire types for moving changes between authored configurations.

use std::fmt;

use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use ts_rs::TS;

use super::SavedEnvironmentIntent;

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
    /// Lineages `into` may use live.
    pub provided: Vec<String>,
    pub hostnames: BranchHostnames,
    /// Row keys marked Never sync, each covering the rows under it too
    /// (`<lineage>:healthcheck` covers `<lineage>:healthcheck.path`): what would move
    /// is shown as meant to differ instead.
    #[serde(default)]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub never_synced: Vec<String>,
    /// A Parent's deployed changes following into its Branch: a secret the Branch has
    /// but never set its own (it holds what `base` holds) follows too.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub follow: bool,
    /// A secret the receiver lacks arrives with its sealed value; without this (a
    /// Sync) it arrives without one, and the receiver's Deploy waits for it. Following
    /// always carries it.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub carry_secrets: bool,
    /// Row keys to move. Absent compares only; present moves the picked rows.
    #[serde(default)]
    #[ts(optional)]
    pub picks: Option<Vec<String>>,
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

impl BranchRow {
    /// Whether it moves a secret.
    pub fn secret(&self) -> bool {
        matches!(self.key.path, RowPath::Variable(_)) && self.from["kind"] == "secret"
    }
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
