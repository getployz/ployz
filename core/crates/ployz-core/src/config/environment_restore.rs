//! The Environment Node families.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Environment Node families with distinct configuration semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentNodeType {
    Service,
    Volume,
    Config,
}

impl EnvironmentNodeType {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Volume => "volume",
            Self::Config => "config",
        }
    }
}
