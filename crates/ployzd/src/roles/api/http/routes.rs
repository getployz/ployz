//! Synchronous Route Binding attachment over accepted Corrosion rows.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use hyper::{Response, StatusCode};
use ployz_core::certificate::{
    MANAGED_LEASE_DOMAIN_SUFFIX, ManagedLeaseAcquireRequest, ManagedLeaseAcquisitionId,
};
use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTable, IngressMode,
    MachineTransport, NamespaceDocument, OperatorWriteProvenance, PloyzDnsTargetState, Principal,
    RouteBindingDocument, ServiceDocument, SqliteParameter, Statement, StoredRow,
    TransactionResult, read_named_rows,
};
use ployz_core::ids::{CorrosionUlid, NamespaceRowId, RouteBindingRowId, ServiceRowId};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::operation::{RouteHostname, RoutePort};
use ployz_core::{
    RouteAttachIntent, RouteAttachOutcome, RouteAttachRefusal, RouteAttachReply,
    RouteAttachRequest, RouteRemoveRequest,
};
use sha2::{Digest, Sha256};

use super::mutations::{decode_request, now_timestamp, typed_response};
use super::roster::corrosion_unavailable_refusal;
use super::server::{ApiService, HttpBody, refusal_response};
use super::store::{
    MutationStoreError, read_accepted_roster, read_cluster, read_named_removal_rows,
};
use crate::corrosion::{NameClaimError, NameClaimOutcome};
use crate::lease::{LeaseClient, LeaseClientError, LeaseTokenFileError, load_or_create_token};

const LEASE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_LEASE_RESPONSE_BYTES: usize = 64 * 1024;
const LEASE_ALLOCATION_ATTEMPTS: usize = 3;

#[async_trait]
pub(super) trait DeployRouteBindings: Send + Sync {
    async fn check(
        &self,
        namespace_id: &NamespaceRowId,
        service_id: &ServiceRowId,
    ) -> Result<(), AutomaticRouteBindingError>;

    async fn ensure(
        &self,
        namespace_id: &NamespaceRowId,
        service_id: &ServiceRowId,
        provenance: OperatorWriteProvenance,
    ) -> Result<(), AutomaticRouteBindingError>;
}

pub(super) struct CorrosionDeployRouteBindings {
    corrosion: crate::corrosion::CorrosionClient,
    cluster_id: ployz_core::ids::ClusterId,
    machine_id: ployz_core::ids::MachineRowId,
    worker: LeaseClient,
    token_path: PathBuf,
}

impl CorrosionDeployRouteBindings {
    pub(super) fn new(
        corrosion: crate::corrosion::CorrosionClient,
        cluster_id: ployz_core::ids::ClusterId,
        machine_id: ployz_core::ids::MachineRowId,
        worker_origin: crate::lease::LeaseWorkerOrigin,
        token_path: PathBuf,
    ) -> Result<Self, LeaseClientError> {
        Ok(Self {
            corrosion,
            cluster_id,
            machine_id,
            worker: LeaseClient::new(
                worker_origin,
                LEASE_ACQUIRE_TIMEOUT,
                MAX_LEASE_RESPONSE_BYTES,
            )?,
            token_path,
        })
    }

    async fn automatic_intent(
        &self,
        namespace_id: &NamespaceRowId,
        service_id: &ServiceRowId,
    ) -> Result<Option<AutomaticRouteIntent>, AutomaticRouteBindingError> {
        let cluster = self.allocated_cluster().await?;
        let suffix = match (
            &cluster.document.hostname_mode,
            &cluster.document.ployz_dns_target,
        ) {
            (AutomaticHostnameMode::Disabled, _) => return Ok(None),
            (AutomaticHostnameMode::Custom { suffix }, _) => suffix.clone(),
            (AutomaticHostnameMode::Ployz, PloyzDnsTargetState::Allocated { hostname, .. }) => {
                hostname.clone()
            }
            (AutomaticHostnameMode::Ployz, PloyzDnsTargetState::Pending) => {
                return Err(AutomaticRouteBindingError::AllocationUnsettled);
            }
            (AutomaticHostnameMode::Ployz, PloyzDnsTargetState::Disabled) => {
                return Err(AutomaticRouteBindingError::AllocationDisabled);
            }
        };
        let rows = read_named_removal_rows(&self.corrosion, CorrosionTable::Namespaces).await?;
        let namespace = read_named_rows::<NamespaceDocument>(&self.cluster_id, rows)
            .accepted
            .into_iter()
            .find(|row| row.id.as_str() == namespace_id.as_str())
            .ok_or_else(|| AutomaticRouteBindingError::NamespaceUnavailable {
                namespace_id: namespace_id.clone(),
            })?;
        // The namespace label remains the automatic label while v2 enforces
        // the current one-service-per-namespace invariant.
        let hostname = RouteHostname::try_new(format!(
            "{}.{}",
            namespace.value.name.as_str(),
            suffix.as_str()
        ))
        .map_err(|error| AutomaticRouteBindingError::InvalidHostname(error.to_string()))?;
        Ok(Some(AutomaticRouteIntent {
            hostname,
            namespace_id: namespace_id.clone(),
            service_id: service_id.clone(),
            endpoint_port: RoutePort::try_new(80)
                .map_err(|error| AutomaticRouteBindingError::InvalidHostname(error.to_string()))?,
        }))
    }

    async fn allocated_cluster(
        &self,
    ) -> Result<super::store::AcceptedCluster, AutomaticRouteBindingError> {
        for _ in 0..LEASE_ALLOCATION_ATTEMPTS {
            let incumbent = read_cluster(&self.corrosion, &self.cluster_id).await?;
            if !matches!(
                (
                    &incumbent.document.hostname_mode,
                    &incumbent.document.ployz_dns_target
                ),
                (AutomaticHostnameMode::Ployz, PloyzDnsTargetState::Pending)
            ) {
                return Ok(incumbent);
            }
            let addresses = self.roster_addresses().await?;
            if addresses.ipv4.is_empty() && addresses.ipv6.is_empty() {
                return Err(AutomaticRouteBindingError::NoRosterEndpoints);
            }
            let acquired = self
                .worker
                .acquire(ManagedLeaseAcquireRequest {
                    acquisition_id: acquisition_id(&self.cluster_id)?,
                    token: load_or_create_token(&self.token_path)?,
                    ipv4: addresses.ipv4,
                    ipv6: addresses.ipv6,
                })
                .await?;
            let hostname = RouteHostname::try_new(format!(
                "{}.{}",
                acquired.lease.name.as_str(),
                MANAGED_LEASE_DOMAIN_SUFFIX
            ))
            .map_err(|error| AutomaticRouteBindingError::InvalidHostname(error.to_string()))?;
            let mut replacement: ClusterDocument = incumbent.document.clone();
            replacement.ployz_dns_target = PloyzDnsTargetState::Allocated {
                hostname,
                acquired_by: self.machine_id.clone(),
            };
            if self
                .replace_cluster_if_matches(&incumbent.stored_document, &replacement)
                .await?
            {
                return read_cluster(&self.corrosion, &self.cluster_id)
                    .await
                    .map_err(Into::into);
            }
        }
        let observed = read_cluster(&self.corrosion, &self.cluster_id).await?;
        if matches!(
            observed.document.ployz_dns_target,
            PloyzDnsTargetState::Allocated { .. }
        ) {
            Ok(observed)
        } else {
            Err(AutomaticRouteBindingError::AllocationUnsettled)
        }
    }

    async fn roster_addresses(&self) -> Result<RosterAddresses, AutomaticRouteBindingError> {
        let roster = read_accepted_roster(&self.corrosion, &self.cluster_id).await?;
        let mut ipv4 = BTreeSet::new();
        let mut ipv6 = BTreeSet::new();
        for machine in roster.machines {
            let address = match machine.document.transport {
                MachineTransport::Wireguard { endpoint, .. } => endpoint.map(|value| value.ip()),
                MachineTransport::Tailscale { ip, .. } => Some(IpAddr::V4(ip)),
            };
            match address {
                Some(IpAddr::V4(address)) => {
                    ipv4.insert(address);
                }
                Some(IpAddr::V6(address)) => {
                    ipv6.insert(address);
                }
                None => {}
            }
        }
        Ok(RosterAddresses {
            ipv4: ipv4.into_iter().collect(),
            ipv6: ipv6.into_iter().collect(),
        })
    }

    async fn replace_cluster_if_matches(
        &self,
        observed: &str,
        replacement: &ClusterDocument,
    ) -> Result<bool, AutomaticRouteBindingError> {
        let replacement = serde_json::to_string(replacement)
            .map_err(|error| AutomaticRouteBindingError::Protocol(error.to_string()))?;
        let response = self
            .corrosion
            .execute(&[Statement::with_params(
                "UPDATE cluster SET document = ? WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text(replacement),
                    SqliteParameter::Text(self.cluster_id.as_str().to_owned()),
                    SqliteParameter::Text(observed.to_owned()),
                ],
            )])
            .await?;
        let [TransactionResult::Success(result)] = response.results.as_slice() else {
            return Err(AutomaticRouteBindingError::Protocol(
                "cluster allocation CAS returned an invalid result".to_owned(),
            ));
        };
        match result.rows_affected {
            0 => Ok(false),
            1 => Ok(true),
            count => Err(AutomaticRouteBindingError::Protocol(format!(
                "cluster allocation CAS affected {count} rows"
            ))),
        }
    }
}

#[async_trait]
impl DeployRouteBindings for CorrosionDeployRouteBindings {
    async fn check(
        &self,
        namespace_id: &NamespaceRowId,
        service_id: &ServiceRowId,
    ) -> Result<(), AutomaticRouteBindingError> {
        let Some(intent) = self.automatic_intent(namespace_id, service_id).await? else {
            return Ok(());
        };
        let rows = read_named_removal_rows(&self.corrosion, CorrosionTable::RouteBindings).await?;
        match select_automatic_route(&self.cluster_id, rows, &intent) {
            AutomaticRouteSelection::Absent | AutomaticRouteSelection::Same => Ok(()),
            AutomaticRouteSelection::Conflict { route_id } => {
                Err(AutomaticRouteBindingError::Collision {
                    hostname: intent.hostname,
                    route_id,
                })
            }
        }
    }

    async fn ensure(
        &self,
        namespace_id: &NamespaceRowId,
        service_id: &ServiceRowId,
        provenance: OperatorWriteProvenance,
    ) -> Result<(), AutomaticRouteBindingError> {
        let Some(intent) = self.automatic_intent(namespace_id, service_id).await? else {
            return Ok(());
        };
        let rows = read_named_removal_rows(&self.corrosion, CorrosionTable::RouteBindings).await?;
        match select_automatic_route(&self.cluster_id, rows, &intent) {
            AutomaticRouteSelection::Same => return Ok(()),
            AutomaticRouteSelection::Conflict { route_id } => {
                return Err(AutomaticRouteBindingError::Collision {
                    hostname: intent.hostname,
                    route_id,
                });
            }
            AutomaticRouteSelection::Absent => {}
        }
        let route_id = RouteBindingRowId::generate();
        let document = RouteBindingDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.cluster_id.clone(),
            provenance,
            hostname: intent.hostname.clone(),
            service_id: intent.service_id.clone(),
            namespace_id: intent.namespace_id.clone(),
            endpoint_port: intent.endpoint_port,
            origin: RouteBindingOrigin::Automatic,
            ingress_mode: IngressMode::Direct,
        };
        let claim_id = CorrosionUlid::try_new(route_id.as_str().to_owned())
            .map_err(|error| AutomaticRouteBindingError::Protocol(error.to_string()))?;
        match self.corrosion.claim_named(claim_id, &document).await? {
            NameClaimOutcome::Claimed { .. } => Ok(()),
            NameClaimOutcome::Lost { winner, report, .. } => {
                let route_id = RouteBindingRowId::try_new(winner.id.into_string())
                    .map_err(|error| AutomaticRouteBindingError::Protocol(error.to_string()))?;
                if report
                    .accepted
                    .iter()
                    .find(|row| row.id.as_str() == route_id.as_str())
                    .is_some_and(|row| automatic_matches(&intent, &row.value))
                {
                    Ok(())
                } else {
                    Err(AutomaticRouteBindingError::Collision {
                        hostname: intent.hostname,
                        route_id,
                    })
                }
            }
        }
    }
}

fn acquisition_id(
    cluster_id: &ployz_core::ids::ClusterId,
) -> Result<ManagedLeaseAcquisitionId, AutomaticRouteBindingError> {
    let digest = Sha256::digest(cluster_id.as_str().as_bytes());
    ManagedLeaseAcquisitionId::try_new(format!("{digest:x}"))
        .map_err(|error| AutomaticRouteBindingError::Protocol(error.to_string()))
}

struct RosterAddresses {
    ipv4: Vec<Ipv4Addr>,
    ipv6: Vec<Ipv6Addr>,
}

struct AutomaticRouteIntent {
    hostname: RouteHostname,
    namespace_id: NamespaceRowId,
    service_id: ServiceRowId,
    endpoint_port: RoutePort,
}

enum AutomaticRouteSelection {
    Absent,
    Same,
    Conflict { route_id: RouteBindingRowId },
}

fn select_automatic_route(
    cluster_id: &ployz_core::ids::ClusterId,
    rows: Vec<StoredRow>,
    intent: &AutomaticRouteIntent,
) -> AutomaticRouteSelection {
    let report = read_named_rows::<RouteBindingDocument>(cluster_id, rows);
    let Some(winner) = report
        .accepted
        .into_iter()
        .find(|row| row.value.hostname == intent.hostname)
    else {
        return AutomaticRouteSelection::Absent;
    };
    let route_id = RouteBindingRowId::try_new(winner.id.into_string())
        .expect("accepted route-binding id is canonical");
    if automatic_matches(intent, &winner.value) {
        AutomaticRouteSelection::Same
    } else {
        AutomaticRouteSelection::Conflict { route_id }
    }
}

fn automatic_matches(intent: &AutomaticRouteIntent, document: &RouteBindingDocument) -> bool {
    document.hostname == intent.hostname
        && document.namespace_id == intent.namespace_id
        && document.service_id == intent.service_id
        && document.endpoint_port == intent.endpoint_port
        && document.origin == RouteBindingOrigin::Automatic
        && document.ingress_mode == IngressMode::Direct
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AutomaticRouteBindingError {
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
    #[error("automatic hostname was invalid: {0}")]
    InvalidHostname(String),
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
    pub(super) fn into_deploy_failure(
        self,
        service_id: ServiceRowId,
    ) -> ployz_core::corrosion::CorrosionDeployFailure {
        use ployz_core::corrosion::CorrosionAutomaticRouteFailure;

        let failure = match self {
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
            Self::InvalidHostname(_) => CorrosionAutomaticRouteFailure::InvalidHostname,
            Self::Store(_)
            | Self::Client(_)
            | Self::Claim(_)
            | Self::Lease(_)
            | Self::Token(_)
            | Self::Protocol(_) => CorrosionAutomaticRouteFailure::Unavailable,
        };
        ployz_core::corrosion::CorrosionDeployFailure::AutomaticRoute {
            service_id,
            failure,
        }
    }
}

pub(super) async fn handle_attach(
    service: &ApiService,
    principal: Principal,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    let request = match decode_request::<RouteAttachRequest>(request.into_body()).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    match attach(service, principal, request).await {
        Ok(Ok(reply)) => typed_response(StatusCode::OK, &reply),
        Ok(Err(refusal)) => typed_response(route_attach_status(&refusal), &refusal),
        Err(error) => {
            tracing::warn!(error = %error, "route attach could not reach durable Corrosion state");
            refusal_response(corrosion_unavailable_refusal())
        }
    }
}

async fn attach(
    service: &ApiService,
    principal: Principal,
    request: RouteAttachRequest,
) -> Result<Result<RouteAttachReply, RouteAttachRefusal>, RouteAttachStoreError> {
    let namespaces = read_named_removal_rows(
        &service.corrosion,
        ployz_core::corrosion::CorrosionTable::Namespaces,
    );
    let services = read_named_removal_rows(
        &service.corrosion,
        ployz_core::corrosion::CorrosionTable::Services,
    );
    let routes = read_named_removal_rows(
        &service.corrosion,
        ployz_core::corrosion::CorrosionTable::RouteBindings,
    );
    let (namespace_rows, service_rows, route_rows) =
        tokio::try_join!(namespaces, services, routes)?;

    let namespace_id = match select_namespace(
        &service.cluster_id,
        &namespace_rows,
        &request.namespace_name,
        request.namespace_id.as_ref(),
    ) {
        Ok(namespace_id) => namespace_id,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let service_id = match select_service(
        &service.cluster_id,
        &service_rows,
        &namespace_id,
        &request.service_name,
        request.service_id.as_ref(),
    ) {
        Ok(service_id) => service_id,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let intent = RouteAttachIntent {
        hostname: request.hostname,
        namespace_id: namespace_id.clone(),
        service_id: service_id.clone(),
        endpoint_port: request.endpoint_port,
        ingress_mode: request.ingress_mode,
    };
    match select_existing_route(&service.cluster_id, route_rows, &intent) {
        ExistingRoute::Absent => {}
        ExistingRoute::Present { route_id, outcome } => {
            return Ok(Ok(RouteAttachReply { route_id, outcome }));
        }
        ExistingRoute::Conflict { route_id } => {
            return Ok(Err(hostname_conflict(&intent, route_id)));
        }
    }

    let route_id = RouteBindingRowId::try_new(ulid::Ulid::new().to_string())
        .map_err(|error| RouteAttachStoreError::Protocol(error.to_string()))?;
    let written_at = now_timestamp()
        .map_err(|()| RouteAttachStoreError::Protocol("clock was unavailable".to_owned()))?;
    let document = RouteBindingDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: service.cluster_id.clone(),
        provenance: OperatorWriteProvenance {
            written_by: principal,
            written_at,
        },
        hostname: intent.hostname.clone(),
        service_id,
        namespace_id,
        endpoint_port: intent.endpoint_port,
        origin: RouteBindingOrigin::Declared,
        ingress_mode: intent.ingress_mode,
    };
    let claim_id = CorrosionUlid::try_new(route_id.as_str().to_owned())
        .map_err(|error| RouteAttachStoreError::Protocol(error.to_string()))?;
    match service.corrosion.claim_named(claim_id, &document).await? {
        NameClaimOutcome::Claimed { id, .. } => Ok(Ok(RouteAttachReply {
            route_id: RouteBindingRowId::try_new(id.into_string())
                .map_err(|error| RouteAttachStoreError::Protocol(error.to_string()))?,
            outcome: RouteAttachOutcome::Attached,
        })),
        NameClaimOutcome::Lost { winner, report, .. } => {
            let winner_id = RouteBindingRowId::try_new(winner.id.into_string())
                .map_err(|error| RouteAttachStoreError::Protocol(error.to_string()))?;
            let winner_document = report
                .accepted
                .iter()
                .find(|row| row.id.as_str() == winner_id.as_str())
                .map(|row| &row.value);
            if winner_document.is_some_and(|winner| intent.matches(winner)) {
                Ok(Ok(RouteAttachReply {
                    route_id: winner_id,
                    outcome: RouteAttachOutcome::AlreadyAttached,
                }))
            } else {
                Ok(Err(hostname_conflict(&intent, winner_id)))
            }
        }
    }
}

fn select_namespace(
    cluster_id: &ployz_core::ids::ClusterId,
    rows: &[StoredRow],
    name: &ployz_core::corrosion::CorrosionNamespaceName,
    requested_id: Option<&NamespaceRowId>,
) -> Result<NamespaceRowId, RouteAttachRefusal> {
    let report = read_named_rows::<NamespaceDocument>(cluster_id, rows.iter().cloned());
    let accepted = report.accepted;
    if let Some(requested) = requested_id {
        if let Some(row) = accepted
            .iter()
            .find(|row| row.id.as_str() == requested.as_str())
        {
            if &row.value.name == name {
                return NamespaceRowId::try_new(row.id.as_str().to_owned()).map_err(|_| {
                    RouteAttachRefusal::NamespaceStoredRowUnselectable {
                        namespace_id: requested.clone(),
                    }
                });
            }
            return Err(RouteAttachRefusal::NamespaceIdentityMismatch {
                namespace_id: requested.clone(),
                requested_name: name.clone(),
                found_name: row.value.name.clone(),
            });
        }
        if rows.iter().any(|row| row.key == requested.as_str()) {
            return Err(RouteAttachRefusal::NamespaceStoredRowUnselectable {
                namespace_id: requested.clone(),
            });
        }
    }
    let mut candidates = accepted
        .into_iter()
        .filter(|row| &row.value.name == name)
        .map(|row| NamespaceRowId::try_new(row.id.into_string()).expect("accepted namespace id"))
        .collect::<Vec<_>>();
    candidates.sort();
    match candidates.as_slice() {
        [] => Err(RouteAttachRefusal::NamespaceNotFound {
            namespace_name: name.clone(),
        }),
        [found] if requested_id.is_some() => Err(RouteAttachRefusal::NamespaceIdMismatch {
            namespace_name: name.clone(),
            requested: requested_id.expect("checked").clone(),
            found: found.clone(),
        }),
        [found] => Ok(found.clone()),
        [_, _, ..] => Err(RouteAttachRefusal::NamespaceAmbiguous {
            namespace_name: name.clone(),
            namespace_ids: candidates,
        }),
    }
}

fn select_service(
    cluster_id: &ployz_core::ids::ClusterId,
    rows: &[StoredRow],
    namespace_id: &NamespaceRowId,
    name: &ployz_core::corrosion::CorrosionServiceName,
    requested_id: Option<&ServiceRowId>,
) -> Result<ServiceRowId, RouteAttachRefusal> {
    let report = read_named_rows::<ServiceDocument>(cluster_id, rows.iter().cloned());
    let accepted = report.accepted;
    if let Some(requested) = requested_id {
        if let Some(row) = accepted
            .iter()
            .find(|row| row.id.as_str() == requested.as_str())
        {
            if &row.value.namespace_id == namespace_id && &row.value.name == name {
                return ServiceRowId::try_new(row.id.as_str().to_owned()).map_err(|_| {
                    RouteAttachRefusal::ServiceStoredRowUnselectable {
                        service_id: requested.clone(),
                    }
                });
            }
            return Err(RouteAttachRefusal::ServiceIdentityMismatch {
                service_id: requested.clone(),
                requested_namespace_id: namespace_id.clone(),
                requested_name: name.clone(),
                found_namespace_id: row.value.namespace_id.clone(),
                found_name: row.value.name.clone(),
            });
        }
        if rows.iter().any(|row| row.key == requested.as_str()) {
            return Err(RouteAttachRefusal::ServiceStoredRowUnselectable {
                service_id: requested.clone(),
            });
        }
    }
    let mut candidates = accepted
        .into_iter()
        .filter(|row| &row.value.namespace_id == namespace_id && &row.value.name == name)
        .map(|row| ServiceRowId::try_new(row.id.into_string()).expect("accepted service id"))
        .collect::<Vec<_>>();
    candidates.sort();
    match candidates.as_slice() {
        [] => Err(RouteAttachRefusal::ServiceNotFound {
            namespace_id: namespace_id.clone(),
            service_name: name.clone(),
        }),
        [found] if requested_id.is_some() => Err(RouteAttachRefusal::ServiceIdMismatch {
            namespace_id: namespace_id.clone(),
            service_name: name.clone(),
            requested: requested_id.expect("checked").clone(),
            found: found.clone(),
        }),
        [found] => Ok(found.clone()),
        [_, _, ..] => Err(RouteAttachRefusal::ServiceAmbiguous {
            namespace_id: namespace_id.clone(),
            service_name: name.clone(),
            service_ids: candidates,
        }),
    }
}

fn hostname_conflict(
    intent: &RouteAttachIntent,
    route_id: RouteBindingRowId,
) -> RouteAttachRefusal {
    RouteAttachRefusal::HostnameAlreadyAttached {
        hostname: intent.hostname.clone(),
        route_id: route_id.clone(),
        remove: RouteRemoveRequest {
            hostname: intent.hostname.clone(),
            route_id: Some(route_id),
        },
    }
}

fn route_attach_status(refusal: &RouteAttachRefusal) -> StatusCode {
    match refusal {
        RouteAttachRefusal::NamespaceNotFound { .. }
        | RouteAttachRefusal::ServiceNotFound { .. } => StatusCode::NOT_FOUND,
        RouteAttachRefusal::NamespaceAmbiguous { .. }
        | RouteAttachRefusal::NamespaceIdMismatch { .. }
        | RouteAttachRefusal::NamespaceIdentityMismatch { .. }
        | RouteAttachRefusal::NamespaceStoredRowUnselectable { .. }
        | RouteAttachRefusal::ServiceAmbiguous { .. }
        | RouteAttachRefusal::ServiceIdMismatch { .. }
        | RouteAttachRefusal::ServiceIdentityMismatch { .. }
        | RouteAttachRefusal::ServiceStoredRowUnselectable { .. }
        | RouteAttachRefusal::HostnameAlreadyAttached { .. } => StatusCode::CONFLICT,
    }
}

#[derive(Debug, thiserror::Error)]
enum RouteAttachStoreError {
    #[error("route attach store failed: {0}")]
    Store(#[from] MutationStoreError),
    #[error("route attach claim failed: {0}")]
    Claim(#[from] NameClaimError),
    #[error("route attach protocol failed: {0}")]
    Protocol(String),
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        CorrosionDocumentVersion, IngressMode, OperatorWriteProvenance, RouteBindingDocument,
        StoredRow,
    };
    use ployz_core::ids::{ClusterId, NamespaceRowId, RouteBindingRowId, ServiceRowId};
    use ployz_core::ingress::RouteBindingOrigin;
    use ployz_core::operation::{RouteHostname, RoutePort};
    use ployz_core::{RouteAttachIntent, RouteAttachOutcome};

    use super::*;

    #[test]
    fn identical_declared_binding_is_idempotent() {
        let cluster_id = ClusterId::try_new("01J00000000000000000000010").expect("cluster");
        let route_id = RouteBindingRowId::try_new("01J00000000000000000000011").expect("route");
        let namespace_id =
            NamespaceRowId::try_new("01J00000000000000000000012").expect("namespace");
        let service_id = ServiceRowId::try_new("01J00000000000000000000013").expect("service");
        let hostname = RouteHostname::try_new("api.example.com").expect("hostname");
        let endpoint_port = RoutePort::try_new(8080).expect("port");
        let document = RouteBindingDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id.clone(),
            provenance: OperatorWriteProvenance {
                written_by: ployz_core::corrosion::Principal::Peer {
                    peer_id: ployz_core::ids::PeerId::try_new("01J00000000000000000000019")
                        .expect("peer"),
                },
                written_at: ployz_core::corrosion::CorrosionTimestamp::try_new(
                    "2026-08-08T00:00:00Z",
                )
                .expect("timestamp"),
            },
            hostname: hostname.clone(),
            service_id: service_id.clone(),
            namespace_id: namespace_id.clone(),
            endpoint_port,
            origin: RouteBindingOrigin::Declared,
            ingress_mode: IngressMode::Direct,
        };
        let rows = vec![StoredRow::new(
            route_id.as_str(),
            serde_json::to_string(&document).expect("document"),
        )];
        let intent = RouteAttachIntent {
            hostname,
            namespace_id,
            service_id,
            endpoint_port,
            ingress_mode: IngressMode::Direct,
        };

        assert_eq!(
            select_existing_route(&cluster_id, rows, &intent),
            ExistingRoute::Present {
                route_id,
                outcome: RouteAttachOutcome::AlreadyAttached,
            }
        );
    }

    #[test]
    fn collision_failure_names_the_exact_route_removal() {
        let route_id = RouteBindingRowId::try_new("01J00000000000000000000011").expect("route");
        let service_id = ServiceRowId::try_new("01J00000000000000000000013").expect("service");
        let hostname = RouteHostname::try_new("api.example.com").expect("hostname");

        let failure = AutomaticRouteBindingError::Collision {
            hostname: hostname.clone(),
            route_id: route_id.clone(),
        }
        .into_deploy_failure(service_id.clone());

        assert_eq!(
            failure,
            ployz_core::corrosion::CorrosionDeployFailure::AutomaticRoute {
                service_id,
                failure: ployz_core::corrosion::CorrosionAutomaticRouteFailure::HostnameCollision {
                    hostname: hostname.clone(),
                    route_id: route_id.clone(),
                    remove: RouteRemoveRequest {
                        hostname,
                        route_id: Some(route_id),
                    },
                },
            }
        );
    }

    #[test]
    fn worker_acquisition_identity_is_stable_hex_without_a_token() {
        let cluster_id = ClusterId::try_new("01J00000000000000000000010").expect("cluster");
        let first = acquisition_id(&cluster_id).expect("acquisition id");
        let second = acquisition_id(&cluster_id).expect("acquisition id");

        assert_eq!(first, second);
        assert_eq!(first.as_str().len(), 64);
        assert!(first.as_str().bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExistingRoute {
    Absent,
    Present {
        route_id: ployz_core::ids::RouteBindingRowId,
        outcome: ployz_core::RouteAttachOutcome,
    },
    Conflict {
        route_id: ployz_core::ids::RouteBindingRowId,
    },
}

fn select_existing_route(
    cluster_id: &ployz_core::ids::ClusterId,
    rows: Vec<ployz_core::corrosion::StoredRow>,
    intent: &ployz_core::RouteAttachIntent,
) -> ExistingRoute {
    let report = read_named_rows::<RouteBindingDocument>(cluster_id, rows);
    let Some(winner) = report
        .accepted
        .into_iter()
        .find(|row| row.value.hostname == intent.hostname)
    else {
        return ExistingRoute::Absent;
    };
    let route_id = RouteBindingRowId::try_new(winner.id.into_string())
        .expect("accepted route-binding id is canonical");
    if intent.matches(&winner.value) {
        ExistingRoute::Present {
            route_id,
            outcome: RouteAttachOutcome::AlreadyAttached,
        }
    } else {
        ExistingRoute::Conflict { route_id }
    }
}
