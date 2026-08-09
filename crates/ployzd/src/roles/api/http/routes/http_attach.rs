//! Synchronous declared Route Binding attachment over accepted Corrosion rows.

use hyper::{Response, StatusCode};
use ployz_core::corrosion::{
    CorrosionDocumentVersion, CorrosionTable, IngressMode, NamespaceDocument,
    OperatorWriteProvenance, Principal, RouteBindingDocument, ServiceDocument, StoredRow,
    read_named_rows,
};
use ployz_core::ids::{NamespaceRowId, RouteBindingRowId, ServiceRowId};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::{
    RouteAttachIntent, RouteAttachOutcome, RouteAttachRefusal, RouteAttachReply,
    RouteAttachRequest, RouteRemoveRequest,
};

use super::super::mutations::{decode_request, now_timestamp, typed_response};
use super::super::roster::corrosion_unavailable_refusal;
use super::super::server::{ApiService, HttpBody, refusal_response};
use super::super::store::{MutationStoreError, insert_document, read_named_removal_rows};
use super::adjudication::route_for_hostname;

pub(crate) async fn handle_attach(
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
    if request.ingress_mode != IngressMode::Direct {
        return Ok(Err(RouteAttachRefusal::UnsupportedIngressMode {
            requested: request.ingress_mode,
        }));
    }

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

    let route_id = RouteBindingRowId::generate();
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
    insert_document(
        &service.corrosion,
        CorrosionTable::RouteBindings,
        route_id.as_str(),
        &document,
    )
    .await?;
    Ok(Ok(RouteAttachReply {
        route_id,
        outcome: RouteAttachOutcome::Attached,
    }))
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
        RouteAttachRefusal::UnsupportedIngressMode { .. } => StatusCode::BAD_REQUEST,
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
    #[error("route attach protocol failed: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExistingRoute {
    Absent,
    Present {
        route_id: RouteBindingRowId,
        outcome: RouteAttachOutcome,
    },
    Conflict {
        route_id: RouteBindingRowId,
    },
}

fn select_existing_route(
    cluster_id: &ployz_core::ids::ClusterId,
    rows: Vec<StoredRow>,
    intent: &RouteAttachIntent,
) -> ExistingRoute {
    let Some(winner) = route_for_hostname(cluster_id, rows, &intent.hostname) else {
        return ExistingRoute::Absent;
    };
    if intent.matches(&winner.document) {
        ExistingRoute::Present {
            route_id: winner.id,
            outcome: RouteAttachOutcome::AlreadyAttached,
        }
    } else {
        ExistingRoute::Conflict {
            route_id: winner.id,
        }
    }
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
    fn unsupported_ingress_mode_is_a_bad_request() {
        assert_eq!(
            route_attach_status(&RouteAttachRefusal::UnsupportedIngressMode {
                requested: IngressMode::CloudflareTunnel,
            }),
            StatusCode::BAD_REQUEST
        );
    }
}
