//! Deploy policy and planning models.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroI64, NonZeroU16, NonZeroU64};

use crate::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId,
    RouteBindingId, ServiceId,
};
use crate::ingress::{AutomaticHostnameLabel, RouteBindingOrigin};
use crate::intent::{RouteBindingState, ServingTargetEntry, VolumePinState};
use crate::machine::runtime::{MachineContainerObservationSnapshot, ManagedContainerIdentity};
use crate::operation::{RouteHostname, RoutePort, RouteTarget};
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

pub mod images;
pub mod planning;
pub mod request;
mod retention;
pub mod revision;
pub mod routes;
pub mod runtime;
pub mod volume;
pub mod volume_admission;

pub use images::*;
pub use planning::*;
pub use request::{
    ContainerRetentionCount, DEFAULT_DEPLOY_RESERVATION_TTL_SECONDS, DependencyCondition,
    DeployImageReplacementError, DeployOrigin, DeployOriginError, DeployRequest,
    DeployReservationExpiresAt, DeployReservationId, DeployReservationNumberError,
    DeployServiceSpec, DeployVolumeDeclarationError, PreStartHook, ReplicaCount, ReplicaCountError,
    ServiceDependency, VolumeDeclaredDeployRequest,
};
pub use retention::{DeployCleanupAction, DeployCleanupContainer, ObservedCleanupCandidate};
pub use revision::{
    canonical_capabilities, namespace_revision_entry_id_for, namespace_revision_id_for,
};
pub use routes::*;
pub use runtime::*;
pub use volume::*;
pub use volume_admission::*;
