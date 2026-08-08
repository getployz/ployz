use ployz_core::RouteRemoveRequest;
use ployz_core::corrosion::{
    CorrosionAutomaticRouteFailure, CorrosionDeployFailure, CorrosionDeployWarning,
};
use ployz_core::ids::{NamespaceRowId, RouteBindingRowId, ServiceRowId};
use ployz_core::ingress::AutomaticHostnameLabelError;
use ployz_core::operation::{RouteHostname, RouteHostnameError};

use super::super::store::MutationStoreError;
use crate::corrosion::NameClaimError;
use crate::lease::{LeaseClientError, LeaseTokenFileError};

#[derive(Debug, thiserror::Error)]
pub(crate) enum AutomaticRouteBindingError {
    #[error("automatic hostname collides with route {route_id}: {hostname:?}")]
    Collision {
        hostname: RouteHostname,
        route_id: RouteBindingRowId,
    },
    #[error("automatic hostname namespace {namespace_id} is unavailable")]
    NamespaceUnavailable { namespace_id: NamespaceRowId },
    #[error("Ployz DNS target allocation has no roster endpoint address")]
    NoRosterEndpoints,
    #[error("Ployz DNS target allocation remains pending")]
    AllocationUnsettled,
    #[error("Ployz automatic hostnames require an enabled DNS target allocation")]
    AllocationDisabled,
    #[error("automatic hostname label was invalid: {0}")]
    InvalidLabel(#[from] AutomaticHostnameLabelError),
    #[error("automatic hostname was invalid: {0}")]
    InvalidHostname(#[from] RouteHostnameError),
    #[error("automatic route Corrosion store failed: {0}")]
    Store(#[from] MutationStoreError),
    #[error("automatic route Corrosion client failed: {0}")]
    Client(#[from] crate::corrosion::CorrosionClientError),
    #[error("automatic route claim failed: {0}")]
    Claim(#[from] NameClaimError),
    #[error("managed lease worker failed: {0}")]
    Lease(#[from] LeaseClientError),
    #[error("managed lease token failed: {0}")]
    Token(#[from] LeaseTokenFileError),
    #[error("automatic route protocol failed: {0}")]
    Protocol(String),
}

impl AutomaticRouteBindingError {
    pub(crate) fn into_deploy_failure(self, service_id: ServiceRowId) -> CorrosionDeployFailure {
        CorrosionDeployFailure::AutomaticRoute {
            service_id,
            failure: self.into_route_failure(),
        }
    }

    pub(crate) fn into_deploy_warning(self, service_id: ServiceRowId) -> CorrosionDeployWarning {
        CorrosionDeployWarning::AutomaticRouteActivation {
            service_id,
            failure: self.into_route_failure(),
        }
    }

    fn into_route_failure(self) -> CorrosionAutomaticRouteFailure {
        match self {
            Self::Collision { hostname, route_id } => {
                CorrosionAutomaticRouteFailure::HostnameCollision {
                    hostname: hostname.clone(),
                    route_id: route_id.clone(),
                    remove: RouteRemoveRequest {
                        hostname,
                        route_id: Some(route_id),
                    },
                }
            }
            Self::NamespaceUnavailable { namespace_id } => {
                CorrosionAutomaticRouteFailure::NamespaceUnavailable { namespace_id }
            }
            Self::NoRosterEndpoints => CorrosionAutomaticRouteFailure::NoRosterEndpoints,
            Self::AllocationUnsettled => CorrosionAutomaticRouteFailure::AllocationUnsettled,
            Self::AllocationDisabled => CorrosionAutomaticRouteFailure::AllocationDisabled,
            Self::InvalidLabel(_) | Self::InvalidHostname(_) => {
                CorrosionAutomaticRouteFailure::InvalidHostname
            }
            Self::Store(_)
            | Self::Client(_)
            | Self::Claim(_)
            | Self::Lease(_)
            | Self::Token(_)
            | Self::Protocol(_) => CorrosionAutomaticRouteFailure::Unavailable,
        }
    }
}
