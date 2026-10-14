use serde::{Deserialize, Serialize};

use crate::{ContainerId, DeploymentLogId, QualifiedService};

/// Docker label carrying the deployment that created a Container.
pub const DEPLOYMENT_ID_LABEL: &str = "ployz.deployment.id";

/// Which Containers of one Service take traffic, stored at [`ServingPointer::key`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServingPointer {
    pub deploy: DeploymentLogId,
    pub containers: Vec<ContainerId>,
    /// Deploy that served before this pointer, routed while a pointed row is still missing.
    pub previous: Option<DeploymentLogId>,
    /// Containers this pointer superseded. While a pointed row is missing they
    /// serve with `previous`, which matters for old copies without a deploy label.
    pub retiring: Vec<ContainerId>,
}

impl ServingPointer {
    /// Cluster table key `serving/<namespace>/<service>`.
    #[must_use]
    pub fn key(service: &QualifiedService) -> String {
        format!("serving/{service}")
    }
}
