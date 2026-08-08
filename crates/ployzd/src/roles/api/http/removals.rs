//! Bounded, synchronous removal of one selected named row.

use hyper::{Response, StatusCode};
use ployz_core::corrosion::CorrosionTable;
use ployz_core::{
    NamedRemovalOutcome, PeerRemoveRefusal, PeerRemoveReply, PeerRemoveRequest,
    PeerRemoveSelection, RouteRemoveRefusal, RouteRemoveReply, RouteRemoveRequest,
    RouteRemoveSelection, ServiceRemoveRowRefusal, ServiceRemoveRowReply, ServiceRemoveRowRequest,
    ServiceRemoveRowSelection, V2Route, select_peer_removal, select_route_removal,
    select_service_removal,
};

use super::mutations::{decode_request, typed_response};
use super::roster::corrosion_unavailable_refusal;
use super::server::{ApiService, HttpBody, refusal_response};
use super::store::{
    ConditionalNamedDelete, MutationStoreError, delete_named_if_matches,
    delete_peer_if_cluster_and_row_match, read_cluster, read_named_removal_rows,
};

pub(super) async fn handle_removal(
    service: &ApiService,
    route: V2Route,
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
            let request = match decode_request::<ServiceRemoveRowRequest>(request.into_body()).await
            {
                Ok(request) => request,
                Err(response) => return response,
            };
            remove_service(service, request).await
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
        | V2Route::Operation(_)
        | V2Route::OperationWatch(_)
        | V2Route::ServiceLogsTail(_)
        | V2Route::ServiceLogsFollow(_)
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
        Ok(PeerRemoveSelection::AlreadyAbsent { peer_id }) => typed_response(
            StatusCode::OK,
            &PeerRemoveReply {
                peer_id,
                outcome: NamedRemovalOutcome::AlreadyAbsent,
            },
        ),
        Ok(PeerRemoveSelection::Delete {
            peer_id,
            stored_document,
        }) => match delete_peer_if_cluster_and_row_match(
            &service.corrosion,
            &service.cluster_id,
            cluster.stored_document,
            &peer_id,
            stored_document,
        )
        .await
        {
            Ok(ConditionalNamedDelete::Deleted) => typed_response(
                StatusCode::OK,
                &PeerRemoveReply {
                    peer_id,
                    outcome: NamedRemovalOutcome::Removed,
                },
            ),
            Ok(ConditionalNamedDelete::ConcurrentMutation) => typed_response(
                StatusCode::CONFLICT,
                &PeerRemoveRefusal::ConcurrentMutation { peer_id },
            ),
            Err(error) => store_failure("delete peer", error),
        },
        Err(refusal) => typed_response(peer_refusal_status(&refusal), &refusal),
    }
}

async fn remove_service(
    service: &ApiService,
    request: ServiceRemoveRowRequest,
) -> Response<HttpBody> {
    let rows = match read_named_removal_rows(&service.corrosion, CorrosionTable::Services).await {
        Ok(rows) => rows,
        Err(error) => return store_failure("read services for removal", error),
    };
    match select_service_removal(&service.cluster_id, rows, &request) {
        Ok(ServiceRemoveRowSelection::AlreadyAbsent { service_id }) => typed_response(
            StatusCode::OK,
            &ServiceRemoveRowReply {
                service_id,
                outcome: NamedRemovalOutcome::AlreadyAbsent,
            },
        ),
        Ok(ServiceRemoveRowSelection::Delete {
            service_id,
            stored_document,
        }) => match delete_named_if_matches(
            &service.corrosion,
            CorrosionTable::Services,
            service_id.as_str(),
            stored_document,
        )
        .await
        {
            Ok(ConditionalNamedDelete::Deleted) => typed_response(
                StatusCode::OK,
                &ServiceRemoveRowReply {
                    service_id,
                    outcome: NamedRemovalOutcome::Removed,
                },
            ),
            Ok(ConditionalNamedDelete::ConcurrentMutation) => typed_response(
                StatusCode::CONFLICT,
                &ServiceRemoveRowRefusal::ConcurrentMutation { service_id },
            ),
            Err(error) => store_failure("delete service", error),
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
        Ok(RouteRemoveSelection::AlreadyAbsent { route_id }) => typed_response(
            StatusCode::OK,
            &RouteRemoveReply {
                route_id,
                outcome: NamedRemovalOutcome::AlreadyAbsent,
            },
        ),
        Ok(RouteRemoveSelection::Delete {
            route_id,
            stored_document,
        }) => match delete_named_if_matches(
            &service.corrosion,
            CorrosionTable::RouteBindings,
            route_id.as_str(),
            stored_document,
        )
        .await
        {
            Ok(ConditionalNamedDelete::Deleted) => typed_response(
                StatusCode::OK,
                &RouteRemoveReply {
                    route_id,
                    outcome: NamedRemovalOutcome::Removed,
                },
            ),
            Ok(ConditionalNamedDelete::ConcurrentMutation) => typed_response(
                StatusCode::CONFLICT,
                &RouteRemoveRefusal::ConcurrentMutation { route_id },
            ),
            Err(error) => store_failure("delete route binding", error),
        },
        Err(refusal) => typed_response(route_refusal_status(&refusal), &refusal),
    }
}

fn peer_refusal_status(refusal: &PeerRemoveRefusal) -> StatusCode {
    match refusal {
        PeerRemoveRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        PeerRemoveRefusal::Ambiguous { .. }
        | PeerRemoveRefusal::NameMismatch { .. }
        | PeerRemoveRefusal::IdMismatch { .. }
        | PeerRemoveRefusal::StoredRowUnselectable { .. }
        | PeerRemoveRefusal::ConcurrentMutation { .. } => StatusCode::CONFLICT,
    }
}

fn service_refusal_status(refusal: &ServiceRemoveRowRefusal) -> StatusCode {
    match refusal {
        ServiceRemoveRowRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        ServiceRemoveRowRefusal::Ambiguous { .. }
        | ServiceRemoveRowRefusal::IdentityMismatch { .. }
        | ServiceRemoveRowRefusal::IdMismatch { .. }
        | ServiceRemoveRowRefusal::StoredRowUnselectable { .. }
        | ServiceRemoveRowRefusal::ConcurrentMutation { .. } => StatusCode::CONFLICT,
    }
}

fn route_refusal_status(refusal: &RouteRemoveRefusal) -> StatusCode {
    match refusal {
        RouteRemoveRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        RouteRemoveRefusal::Ambiguous { .. }
        | RouteRemoveRefusal::NameMismatch { .. }
        | RouteRemoveRefusal::IdMismatch { .. }
        | RouteRemoveRefusal::StoredRowUnselectable { .. }
        | RouteRemoveRefusal::ConcurrentMutation { .. } => StatusCode::CONFLICT,
    }
}

fn store_failure(action: &'static str, error: MutationStoreError) -> Response<HttpBody> {
    tracing::warn!(%action, error = %error, "named removal could not reach durable Corrosion state");
    refusal_response(corrosion_unavailable_refusal())
}
