//! Automatic route checks and activation around deploy promotion.

use std::path::PathBuf;

use async_trait::async_trait;
use ployz_core::corrosion::{
    AutomaticHostnameMode, CorrosionDocumentVersion, CorrosionServiceName, CorrosionTable,
    IngressMode, NamespaceDocument, OperatorWriteProvenance, PloyzDnsTargetState,
    RouteBindingDocument, read_named_rows,
};
use ployz_core::ids::{
    ClusterId, CorrosionUlid, MachineRowId, NamespaceRowId, RouteBindingRowId, ServiceRowId,
};
use ployz_core::ingress::{AutomaticHostnameLabel, RouteBindingOrigin};
use ployz_core::operation::{RouteHostname, RoutePort};

use super::super::store::read_named_removal_rows;
use super::adjudication::route_for_hostname;
use super::allocation::ManagedTargetAllocator;
use super::error::AutomaticRouteBindingError;
use crate::corrosion::NameClaimOutcome;
use crate::lease::{LeaseClientError, LeaseWorkerOrigin};

#[async_trait]
pub(crate) trait DeployRouteBindings: Send + Sync {
    async fn check(
        &self,
        namespace_id: &NamespaceRowId,
        service_id: &ServiceRowId,
        service_name: &CorrosionServiceName,
        provenance: OperatorWriteProvenance,
    ) -> Result<(), AutomaticRouteBindingError>;

    async fn ensure(
        &self,
        namespace_id: &NamespaceRowId,
        service_id: &ServiceRowId,
        service_name: &CorrosionServiceName,
        provenance: OperatorWriteProvenance,
    ) -> Result<(), AutomaticRouteBindingError>;
}

pub(crate) struct CorrosionDeployRouteBindings {
    corrosion: crate::corrosion::CorrosionClient,
    cluster_id: ClusterId,
    allocator: ManagedTargetAllocator,
}

impl CorrosionDeployRouteBindings {
    pub(crate) fn new(
        corrosion: crate::corrosion::CorrosionClient,
        cluster_id: ClusterId,
        machine_id: MachineRowId,
        worker_origin: LeaseWorkerOrigin,
        token_path: PathBuf,
    ) -> Result<Self, LeaseClientError> {
        let allocator = ManagedTargetAllocator::new(
            corrosion.clone(),
            cluster_id.clone(),
            machine_id,
            worker_origin,
            token_path,
        )?;
        Ok(Self {
            corrosion,
            cluster_id,
            allocator,
        })
    }

    async fn automatic_intent(
        &self,
        namespace_id: &NamespaceRowId,
        service_id: &ServiceRowId,
        service_name: &CorrosionServiceName,
        provenance: &OperatorWriteProvenance,
    ) -> Result<Option<AutomaticRouteIntent>, AutomaticRouteBindingError> {
        let cluster = self.allocator.settle_cluster(provenance).await?;
        let suffix = match (
            cluster.document.hostname_mode,
            cluster.document.ployz_dns_target,
        ) {
            (AutomaticHostnameMode::Disabled, _) => return Ok(None),
            (AutomaticHostnameMode::Custom { suffix }, _) => suffix,
            (AutomaticHostnameMode::Ployz, PloyzDnsTargetState::Allocated { hostname, .. }) => {
                hostname
            }
            (AutomaticHostnameMode::Ployz, PloyzDnsTargetState::Pending) => {
                return Err(AutomaticRouteBindingError::AllocationUnsettled);
            }
            (AutomaticHostnameMode::Ployz, PloyzDnsTargetState::Disabled) => {
                return Err(AutomaticRouteBindingError::AllocationDisabled);
            }
        };

        // Namespace acceptance remains a route precondition, but the caller's
        // service name is the automatic label.
        let rows = read_named_removal_rows(&self.corrosion, CorrosionTable::Namespaces).await?;
        if !read_named_rows::<NamespaceDocument>(&self.cluster_id, rows)
            .accepted
            .iter()
            .any(|row| row.id.as_str() == namespace_id.as_str())
        {
            return Err(AutomaticRouteBindingError::NamespaceUnavailable {
                namespace_id: namespace_id.clone(),
            });
        }
        let hostname = automatic_hostname(service_name, &suffix)?;
        Ok(Some(AutomaticRouteIntent {
            hostname,
            namespace_id: namespace_id.clone(),
            service_id: service_id.clone(),
            endpoint_port: RoutePort::try_new(80).expect("port 80 is a valid route port"),
        }))
    }

    async fn selection(
        &self,
        intent: &AutomaticRouteIntent,
    ) -> Result<AutomaticRouteSelection, AutomaticRouteBindingError> {
        let rows = read_named_removal_rows(&self.corrosion, CorrosionTable::RouteBindings).await?;
        let Some(winner) = route_for_hostname(&self.cluster_id, rows, &intent.hostname) else {
            return Ok(AutomaticRouteSelection::Absent);
        };
        if automatic_matches(intent, &winner.document) {
            Ok(AutomaticRouteSelection::Same)
        } else {
            Ok(AutomaticRouteSelection::Conflict {
                route_id: winner.id,
            })
        }
    }
}

#[async_trait]
impl DeployRouteBindings for CorrosionDeployRouteBindings {
    async fn check(
        &self,
        namespace_id: &NamespaceRowId,
        service_id: &ServiceRowId,
        service_name: &CorrosionServiceName,
        provenance: OperatorWriteProvenance,
    ) -> Result<(), AutomaticRouteBindingError> {
        let Some(intent) = self
            .automatic_intent(namespace_id, service_id, service_name, &provenance)
            .await?
        else {
            return Ok(());
        };
        match self.selection(&intent).await? {
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
        service_name: &CorrosionServiceName,
        provenance: OperatorWriteProvenance,
    ) -> Result<(), AutomaticRouteBindingError> {
        let Some(intent) = self
            .automatic_intent(namespace_id, service_id, service_name, &provenance)
            .await?
        else {
            return Ok(());
        };
        match self.selection(&intent).await? {
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

fn automatic_matches(intent: &AutomaticRouteIntent, document: &RouteBindingDocument) -> bool {
    document.hostname == intent.hostname
        && document.namespace_id == intent.namespace_id
        && document.service_id == intent.service_id
        && document.endpoint_port == intent.endpoint_port
        && document.origin == RouteBindingOrigin::Automatic
        && document.ingress_mode == IngressMode::Direct
}

fn automatic_hostname(
    service_name: &CorrosionServiceName,
    suffix: &RouteHostname,
) -> Result<RouteHostname, AutomaticRouteBindingError> {
    let label = AutomaticHostnameLabel::try_new(service_name.as_str())?;
    RouteHostname::try_new(format!("{}.{}", label.as_str(), suffix.as_str())).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::CorrosionServiceName;
    use ployz_core::operation::RouteHostname;

    use super::automatic_hostname;

    #[test]
    fn service_name_is_the_automatic_hostname_label() {
        let service_name = CorrosionServiceName::try_new("api").expect("service name");
        let suffix = RouteHostname::try_new("apps.example.test").expect("suffix");
        let hostname = automatic_hostname(&service_name, &suffix).expect("hostname");

        assert_eq!(hostname.as_str(), "api.apps.example.test");
    }
}
