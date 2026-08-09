//! Synchronous declared Route Binding attachment over accepted Corrosion rows.

use hyper::{Response, StatusCode};
use ployz_core::corrosion::{
    CorrosionDocumentVersion, CorrosionTable, IngressMode, NamespaceDocument,
    OperatorWriteProvenance, Principal, RouteBindingDocument, ServiceDocument, StoredRow,
    read_named_rows, read_rows, service_key,
};
use ployz_core::ids::CorrosionNamespaceName;
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
    let routes = read_named_removal_rows(
        &service.corrosion,
        ployz_core::corrosion::CorrosionTable::RouteBindings,
    );
    let services = read_named_removal_rows(
        &service.corrosion,
        ployz_core::corrosion::CorrosionTable::Services,
    );
    let (namespace_rows, service_rows, route_rows) =
        tokio::try_join!(namespaces, services, routes)?;

    let namespace_id = match select_namespace(
        &service.cluster_id,
        &namespace_rows,
        &request.namespace_name,
    ) {
        Ok(namespace_id) => namespace_id,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let service_name = match select_service(
        &service.cluster_id,
        &service_rows,
        &namespace_id,
        &request.service_name,
    ) {
        Ok(service_name) => service_name,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let intent = RouteAttachIntent {
        hostname: request.hostname,
        namespace_id: namespace_id.clone(),
        service_name: service_name.clone(),
        endpoint_port: request.endpoint_port,
        ingress_mode: request.ingress_mode,
    };
    match select_existing_route(&service.cluster_id, route_rows, &intent) {
        ExistingRoute::Absent => {}
        ExistingRoute::Present { outcome } => {
            return Ok(Ok(RouteAttachReply { outcome }));
        }
        ExistingRoute::Conflict => {
            return Ok(Err(hostname_conflict(&intent)));
        }
    }

    let route_id = intent.hostname.clone();
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
        namespace_id,
        service_name,
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
        outcome: RouteAttachOutcome::Attached,
    }))
}

fn select_namespace(
    cluster_id: &ployz_core::ids::ClusterName,
    rows: &[StoredRow],
    name: &ployz_core::corrosion::CorrosionNamespaceName,
) -> Result<CorrosionNamespaceName, RouteAttachRefusal> {
    let report = read_named_rows::<NamespaceDocument>(cluster_id, rows.iter().cloned());
    if report
        .accepted
        .iter()
        .any(|row| row.source.key == name.as_str())
    {
        return Ok(name.clone());
    }
    if rows.iter().any(|row| row.key == name.as_str()) {
        return Err(RouteAttachRefusal::NamespaceStoredRowUnselectable {
            namespace_name: name.clone(),
        });
    }
    Err(RouteAttachRefusal::NamespaceNotFound {
        namespace_name: name.clone(),
    })
}

fn select_service(
    cluster_id: &ployz_core::ids::ClusterName,
    rows: &[StoredRow],
    namespace_id: &CorrosionNamespaceName,
    name: &ployz_core::corrosion::CorrosionServiceName,
) -> Result<ployz_core::corrosion::CorrosionServiceName, RouteAttachRefusal> {
    let report = read_rows::<ServiceDocument>(cluster_id, rows.iter().cloned());
    let expected_key = service_key(namespace_id, name);
    let Some(service) = report
        .accepted
        .into_iter()
        .find(|row| row.source.key == expected_key)
    else {
        if rows.iter().any(|row| row.key == expected_key) {
            return Err(RouteAttachRefusal::ServiceStoredRowUnselectable {
                namespace_name: namespace_id.clone(),
                service_name: name.clone(),
            });
        }
        return Err(RouteAttachRefusal::ServiceNotFound {
            namespace_name: namespace_id.clone(),
            service_name: name.clone(),
        });
    };
    debug_assert_eq!(service.value.namespace_id, *namespace_id);
    debug_assert_eq!(&service.value.name, name);
    Ok(name.clone())
}

fn hostname_conflict(intent: &RouteAttachIntent) -> RouteAttachRefusal {
    RouteAttachRefusal::HostnameAlreadyAttached {
        hostname: intent.hostname.clone(),
        remove: RouteRemoveRequest {
            hostname: intent.hostname.clone(),
        },
    }
}

fn route_attach_status(refusal: &RouteAttachRefusal) -> StatusCode {
    match refusal {
        RouteAttachRefusal::UnsupportedIngressMode { .. } => StatusCode::BAD_REQUEST,
        RouteAttachRefusal::NamespaceNotFound { .. }
        | RouteAttachRefusal::ServiceNotFound { .. } => StatusCode::NOT_FOUND,
        RouteAttachRefusal::NamespaceStoredRowUnselectable { .. }
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
    Present { outcome: RouteAttachOutcome },
    Conflict,
}

fn select_existing_route(
    cluster_id: &ployz_core::ids::ClusterName,
    rows: Vec<StoredRow>,
    intent: &RouteAttachIntent,
) -> ExistingRoute {
    let Some(winner) = route_for_hostname(cluster_id, rows, &intent.hostname) else {
        return ExistingRoute::Absent;
    };
    if intent.matches(&winner) {
        ExistingRoute::Present {
            outcome: RouteAttachOutcome::AlreadyAttached,
        }
    } else {
        ExistingRoute::Conflict
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        CorrosionDocumentVersion, IngressMode, OperatorWriteProvenance, RouteBindingDocument,
        StoredRow,
    };
    use ployz_core::ids::{ClusterName, CorrosionNamespaceName, RouteHostname};
    use ployz_core::ingress::RouteBindingOrigin;
    use ployz_core::operation::RoutePort;
    use ployz_core::{RouteAttachIntent, RouteAttachOutcome};

    use super::*;

    #[test]
    fn identical_declared_binding_is_idempotent() {
        let cluster_id = ClusterName::try_new("main").expect("cluster");
        let namespace_id = CorrosionNamespaceName::try_new("prod").expect("namespace");
        let service_name =
            ployz_core::corrosion::CorrosionServiceName::try_new("api").expect("service");
        let hostname = RouteHostname::try_new("api.example.com").expect("hostname");
        let endpoint_port = RoutePort::try_new(8080).expect("port");
        let document = RouteBindingDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id.clone(),
            provenance: OperatorWriteProvenance {
                written_by: ployz_core::corrosion::Principal::Peer {
                    peer_id: ployz_core::ids::PeerName::try_new("operator").expect("peer"),
                },
                written_at: ployz_core::corrosion::CorrosionTimestamp::try_new(
                    "2026-08-08T00:00:00Z",
                )
                .expect("timestamp"),
            },
            hostname: hostname.clone(),
            namespace_id: namespace_id.clone(),
            service_name: service_name.clone(),
            endpoint_port,
            origin: RouteBindingOrigin::Declared,
            ingress_mode: IngressMode::Direct,
        };
        let rows = vec![StoredRow::new(
            hostname.as_str(),
            serde_json::to_string(&document).expect("document"),
        )];
        let intent = RouteAttachIntent {
            hostname,
            namespace_id,
            service_name,
            endpoint_port,
            ingress_mode: IngressMode::Direct,
        };

        assert_eq!(
            select_existing_route(&cluster_id, rows, &intent),
            ExistingRoute::Present {
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
