//! Route naming and ownership values shared by the V2 API and gateway.

use serde::{Deserialize, Serialize};

/// Stable provenance of one attached route binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum RouteBindingOrigin {
    Declared,
    Automatic,
}
