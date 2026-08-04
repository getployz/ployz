//! Machine-local host port ownership policy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HostPortAssurance {
    Keeper,
    External,
}

impl HostPortAssurance {
    #[must_use]
    pub const fn keeper() -> Self {
        Self::Keeper
    }
}
