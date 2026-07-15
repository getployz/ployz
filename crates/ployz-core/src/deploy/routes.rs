//! Route declarations carried by a deploy request.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRoute {
    pub target: DeployRouteTarget,
    pub endpoint_port: RoutePort,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployRouteTarget {
    AutoHostname {
        label: AutomaticHostnameLabel,
    },
    Hostname {
        hostname: crate::operation::RouteHostname,
    },
}

impl DeployRouteTarget {
    /// The route table target this declaration binds to, if it already names
    /// one. `AutoHostname` is declared intent without a hostname: it commits
    /// no `RouteBindingState` until the lease flow mints its hostname, so it
    /// yields `None` here rather than a sentinel target.
    #[must_use]
    pub fn concrete_target(&self) -> Option<RouteTarget> {
        match self {
            Self::AutoHostname { .. } => None,
            Self::Hostname { hostname } => Some(RouteTarget::new(hostname.clone())),
        }
    }
}
