//! Deploy policy and planning models.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::num::{NonZeroI64, NonZeroU16, NonZeroU64};

use crate::ids::{NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId, ServiceId};
use crate::ingress::AutomaticHostnameLabel;
use crate::operation::{RoutePort, RouteTarget};
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

pub mod images;
pub mod request;
pub mod revision;
pub mod routes;
pub mod runtime;
pub mod volume;

pub use images::*;
pub use request::{
    ContainerRetentionCount, DEFAULT_DEPLOY_RESERVATION_TTL_SECONDS, DependencyCondition,
    DeployImageReplacementError, DeployOrigin, DeployOriginError, DeployRequest,
    DeployReservationExpiresAt, DeployReservationId, DeployReservationNumberError,
    DeployServiceSpec, PreStartHook, ReplicaCount, ReplicaCountError, ServiceDependency,
    ServiceMode,
};
pub use revision::{
    EnvironmentRevisionKey, canonical_capabilities, namespace_revision_entry_id_for,
    namespace_revision_id_for,
};
pub use routes::*;
pub use runtime::*;
pub use volume::*;
