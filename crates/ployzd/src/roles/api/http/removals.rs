//! Bounded, synchronous removal of one selected named row.

use hyper::{Response, StatusCode};
use ployz_core::corrosion::{CorrosionTable, OperatorWriteProvenance, Principal};
use ployz_core::{
    NamedRemovalOutcome, PeerRemoveRefusal, PeerRemoveReply, PeerRemoveRequest,
    PeerRemoveSelection, RouteRemoveRefusal, RouteRemoveReply, RouteRemoveRequest,
    RouteRemoveSelection, ServiceRemoveRefusal, ServiceRemoveReply, ServiceRemoveRequest,
    ServiceRemoveSelection, V2Route, select_peer_removal, select_route_removal,
    select_service_removal,
};

use super::mutations::{decode_request, now_timestamp, typed_response};
use super::namespace_store::{NamespaceReplaceOutcome, NamespaceStore, NamespaceStoreError};
use super::roster::corrosion_unavailable_refusal;
use super::server::{ApiService, HttpBody, refusal_response};
use super::store::{
    ConditionalNamedDelete, MutationStoreError, delete_named_if_matches,
    delete_peer_if_cluster_and_row_match, read_cluster, read_named_removal_rows,
};

pub(super) async fn handle_removal(
    service: &ApiService,
    route: V2Route,
    principal: Principal,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    match route {
        V2Route::PeerRemove => {
            let request = match decode_request::<PeerRemoveRequest>(request.into_body()).await {
                Ok(request) => request,
                Err(response) => return response,
            };
            remove_peer(service, request).await
        }
        V2Route::ServiceRemove => {
            let request = match decode_request::<ServiceRemoveRequest>(request.into_body()).await {
                Ok(request) => request,
                Err(response) => return response,
            };
            remove_service(service, principal, request).await
        }
        V2Route::RouteRemove => {
            let request = match decode_request::<RouteRemoveRequest>(request.into_body()).await {
                Ok(request) => request,
                Err(response) => return response,
            };
            remove_route(service, request).await
        }
        V2Route::Version
        | V2Route::Founding
        | V2Route::TokenCreate
        | V2Route::TokenList
        | V2Route::TokenRevoke(_)
        | V2Route::MachineEndpointSet
        | V2Route::MachineUpgrade
        | V2Route::MachineRemove
        | V2Route::Join
        | V2Route::Status
        | V2Route::Doctor
        | V2Route::Lens(_)
        | V2Route::NamespaceCreate
        | V2Route::NamespaceRemove
        | V2Route::Deploy
        | V2Route::DeployInspect
        | V2Route::DeployPrepare
        | V2Route::DeployRetire
        | V2Route::ServiceLogsProbe
        | V2Route::ServiceLogsTail(_, _)
        | V2Route::ServiceLogsFollow(_, _)
        | V2Route::RouteAttach
        | V2Route::LensWatch(_) => refusal_response(ployz_core::ApiRefusal::UnsupportedRoute),
    }
}

async fn remove_peer(service: &ApiService, request: PeerRemoveRequest) -> Response<HttpBody> {
    let cluster = match read_cluster(&service.corrosion, &service.cluster_id).await {
        Ok(cluster) => cluster,
        Err(error) => return store_failure("read cluster for peer removal", error),
    };
    let rows = match read_named_removal_rows(&service.corrosion, CorrosionTable::Peers).await {
        Ok(rows) => rows,
        Err(error) => return store_failure("read peers for removal", error),
    };
    match select_peer_removal(&cluster.document, rows, &request) {
        Ok(PeerRemoveSelection::AlreadyAbsent { peer_name }) => typed_response(
            StatusCode::OK,
            &PeerRemoveReply {
                peer_name,
                outcome: NamedRemovalOutcome::AlreadyAbsent,
            },
        ),
        Ok(PeerRemoveSelection::Delete {
            peer_name,
            stored_document,
        }) => match delete_peer_if_cluster_and_row_match(
            &service.corrosion,
            &service.cluster_id,
            cluster.stored_document,
            &peer_name,
            stored_document,
        )
        .await
        {
            Ok(ConditionalNamedDelete::Deleted) => typed_response(
                StatusCode::OK,
                &PeerRemoveReply {
                    peer_name,
                    outcome: NamedRemovalOutcome::Removed,
                },
            ),
            Ok(ConditionalNamedDelete::ConcurrentMutation) => typed_response(
                StatusCode::CONFLICT,
                &PeerRemoveRefusal::ConcurrentMutation { peer_name },
            ),
            Err(error) => store_failure("delete peer", error),
        },
        Err(refusal) => typed_response(peer_refusal_status(&refusal), &refusal),
    }
}

async fn remove_service(
    service: &ApiService,
    principal: Principal,
    request: ServiceRemoveRequest,
) -> Response<HttpBody> {
    let store = NamespaceStore::new(service.corrosion.clone(), service.cluster_id.clone());
    let row = match store.raw_by_id(&request.namespace_name).await {
        Ok(row) => row,
        Err(error) => return namespace_store_failure("read namespace for service removal", error),
    };
    let written_at = match now_timestamp() {
        Ok(written_at) => written_at,
        Err(()) => return refusal_response(corrosion_unavailable_refusal()),
    };
    match select_service_removal(
        &service.cluster_id,
        row,
        &request,
        OperatorWriteProvenance {
            written_by: principal,
            written_at,
        },
    ) {
        Ok(ServiceRemoveSelection::AlreadyAbsent {
            namespace_name,
            service_name,
        }) => typed_response(
            StatusCode::OK,
            &ServiceRemoveReply {
                namespace_name,
                service_name,
                outcome: NamedRemovalOutcome::AlreadyAbsent,
            },
        ),
        Ok(ServiceRemoveSelection::Replace {
            namespace_name,
            stored_document,
            replacement_document,
        }) => match store
            .replace_if_matches(&namespace_name, &stored_document, &replacement_document)
            .await
        {
            Ok(NamespaceReplaceOutcome::Replaced) => typed_response(
                StatusCode::OK,
                &ServiceRemoveReply {
                    namespace_name,
                    service_name: request.service_name,
                    outcome: NamedRemovalOutcome::Removed,
                },
            ),
            Ok(NamespaceReplaceOutcome::Changed) => typed_response(
                StatusCode::CONFLICT,
                &ServiceRemoveRefusal::ConcurrentMutation {
                    namespace_name,
                    service_name: request.service_name,
                },
            ),
            Err(error) => namespace_store_failure("replace namespace for service removal", error),
        },
        Err(refusal) => typed_response(service_refusal_status(&refusal), &refusal),
    }
}

async fn remove_route(service: &ApiService, request: RouteRemoveRequest) -> Response<HttpBody> {
    let rows =
        match read_named_removal_rows(&service.corrosion, CorrosionTable::RouteBindings).await {
            Ok(rows) => rows,
            Err(error) => return store_failure("read route bindings for removal", error),
        };
    match select_route_removal(&service.cluster_id, rows, &request) {
        Ok(RouteRemoveSelection::AlreadyAbsent { hostname }) => typed_response(
            StatusCode::OK,
            &RouteRemoveReply {
                hostname,
                outcome: NamedRemovalOutcome::AlreadyAbsent,
            },
        ),
        Ok(RouteRemoveSelection::Delete {
            hostname,
            stored_document,
        }) => match delete_named_if_matches(
            &service.corrosion,
            CorrosionTable::RouteBindings,
            hostname.as_str(),
            stored_document,
        )
        .await
        {
            Ok(ConditionalNamedDelete::Deleted) => typed_response(
                StatusCode::OK,
                &RouteRemoveReply {
                    hostname,
                    outcome: NamedRemovalOutcome::Removed,
                },
            ),
            Ok(ConditionalNamedDelete::ConcurrentMutation) => typed_response(
                StatusCode::CONFLICT,
                &RouteRemoveRefusal::ConcurrentMutation { hostname },
            ),
            Err(error) => store_failure("delete route binding", error),
        },
        Err(refusal) => typed_response(route_refusal_status(&refusal), &refusal),
    }
}

fn peer_refusal_status(refusal: &PeerRemoveRefusal) -> StatusCode {
    match refusal {
        PeerRemoveRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        PeerRemoveRefusal::StoredRowUnselectable { .. }
        | PeerRemoveRefusal::ConcurrentMutation { .. } => StatusCode::CONFLICT,
    }
}

fn service_refusal_status(refusal: &ServiceRemoveRefusal) -> StatusCode {
    match refusal {
        ServiceRemoveRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        ServiceRemoveRefusal::NamespaceStoredRowUnselectable { .. }
        | ServiceRemoveRefusal::ConcurrentMutation { .. } => StatusCode::CONFLICT,
    }
}

fn route_refusal_status(refusal: &RouteRemoveRefusal) -> StatusCode {
    match refusal {
        RouteRemoveRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        RouteRemoveRefusal::StoredRowUnselectable { .. }
        | RouteRemoveRefusal::ConcurrentMutation { .. } => StatusCode::CONFLICT,
    }
}

fn store_failure(action: &'static str, error: MutationStoreError) -> Response<HttpBody> {
    tracing::warn!(%action, error = %error, "named removal could not reach durable Corrosion state");
    refusal_response(corrosion_unavailable_refusal())
}

fn namespace_store_failure(action: &'static str, error: NamespaceStoreError) -> Response<HttpBody> {
    tracing::warn!(%action, error = %error, "named removal could not reach durable Corrosion state");
    refusal_response(corrosion_unavailable_refusal())
}
