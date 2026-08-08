//! Route Binding primitives and refusal presentation.

use hyper::Method;
use ployz_core::{
    ROUTE_ATTACH_ROUTE, RouteAttachOutcome, RouteAttachRefusal, RouteAttachReply,
    RouteAttachRequest,
};

use crate::commands::{RouteAttachCommand, RouteCommand};
use crate::mesh::http::JsonReply;
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: RouteCommand) -> Result<String, RouteExecutionError> {
    match command {
        RouteCommand::Attach(command) => attach(command).await,
        RouteCommand::Remove(command) => crate::removal::execute_route_remove(command)
            .await
            .map_err(RouteExecutionError::Remove),
    }
}

async fn attach(command: RouteAttachCommand) -> Result<String, RouteExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let hostname = command.hostname.as_str().to_owned();
    let reply = remote
        .request_json_with_refusal::<_, RouteAttachReply, RouteAttachRefusal>(
            Method::POST,
            ROUTE_ATTACH_ROUTE,
            Some(&RouteAttachRequest {
                hostname: command.hostname,
                namespace_name: command.namespace,
                namespace_id: command.namespace_id,
                service_name: command.service,
                service_id: command.service_id,
                endpoint_port: command.endpoint_port,
                ingress_mode: command.ingress_mode,
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(RouteAttachReply {
            route_id,
            outcome: RouteAttachOutcome::Attached,
        }) => Ok(format!("attached route {hostname} ({route_id})\n")),
        JsonReply::Success(RouteAttachReply {
            route_id,
            outcome: RouteAttachOutcome::AlreadyAttached,
        }) => Ok(format!(
            "route {hostname} ({route_id}) is already attached\n"
        )),
        JsonReply::Refused(refusal) => Err(RouteExecutionError::Refused {
            message: refusal_message(refusal),
        }),
    }
}

fn refusal_message(refusal: RouteAttachRefusal) -> String {
    match refusal {
        RouteAttachRefusal::NamespaceNotFound { namespace_name } => {
            format!("namespace {} does not exist", namespace_name.as_str())
        }
        RouteAttachRefusal::NamespaceAmbiguous {
            namespace_name,
            namespace_ids,
        } => format!(
            "namespace {} is ambiguous; retry with --namespace-id using one of: {}",
            namespace_name.as_str(),
            join_ids(namespace_ids.iter().map(|id| id.as_str()))
        ),
        RouteAttachRefusal::NamespaceIdMismatch {
            namespace_name,
            requested,
            found,
        } => format!(
            "namespace row {} is absent, but {} is row {}; retry with --namespace-id {}",
            requested.as_str(),
            namespace_name.as_str(),
            found.as_str(),
            found.as_str()
        ),
        RouteAttachRefusal::NamespaceIdentityMismatch {
            namespace_id,
            requested_name,
            found_name,
        } => format!(
            "namespace row {} is named {}, not {}",
            namespace_id.as_str(),
            found_name.as_str(),
            requested_name.as_str()
        ),
        RouteAttachRefusal::NamespaceStoredRowUnselectable { namespace_id } => format!(
            "namespace row {} exists but this binary cannot safely select it",
            namespace_id.as_str()
        ),
        RouteAttachRefusal::ServiceNotFound {
            namespace_id,
            service_name,
        } => format!(
            "service {} does not exist in namespace row {}",
            service_name.as_str(),
            namespace_id.as_str()
        ),
        RouteAttachRefusal::ServiceAmbiguous {
            namespace_id,
            service_name,
            service_ids,
        } => format!(
            "service {} in namespace row {} is ambiguous; retry with --service-id using one of: {}",
            service_name.as_str(),
            namespace_id.as_str(),
            join_ids(service_ids.iter().map(|id| id.as_str()))
        ),
        RouteAttachRefusal::ServiceIdMismatch {
            namespace_id,
            service_name,
            requested,
            found,
        } => format!(
            "service row {} is absent, but {} in namespace row {} is row {}; retry with --service-id {}",
            requested.as_str(),
            service_name.as_str(),
            namespace_id.as_str(),
            found.as_str(),
            found.as_str()
        ),
        RouteAttachRefusal::ServiceIdentityMismatch {
            service_id,
            requested_namespace_id,
            requested_name,
            found_namespace_id,
            found_name,
        } => format!(
            "service row {} is {}/{}, not {}/{}",
            service_id.as_str(),
            found_namespace_id.as_str(),
            found_name.as_str(),
            requested_namespace_id.as_str(),
            requested_name.as_str()
        ),
        RouteAttachRefusal::ServiceStoredRowUnselectable { service_id } => format!(
            "service row {} exists but this binary cannot safely select it",
            service_id.as_str()
        ),
        RouteAttachRefusal::HostnameAlreadyAttached {
            hostname,
            route_id,
            remove,
        } => format!(
            "hostname {} is already attached by route {}; remove it first with `ployz route rm {} --id {}`",
            hostname.as_str(),
            route_id.as_str(),
            remove.hostname.as_str(),
            remove
                .route_id
                .as_ref()
                .map_or(route_id.as_str(), |id| id.as_str())
        ),
    }
}

fn join_ids<'a>(ids: impl IntoIterator<Item = &'a str>) -> String {
    ids.into_iter().collect::<Vec<_>>().join(", ")
}

#[derive(Debug, thiserror::Error)]
pub enum RouteExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error(transparent)]
    Remove(#[from] crate::removal::RemovalExecutionError),
    #[error("{message}")]
    Refused { message: String },
}

#[cfg(test)]
mod tests {
    use ployz_core::ids::RouteBindingRowId;
    use ployz_core::operation::RouteHostname;
    use ployz_core::{RouteAttachRefusal, RouteRemoveRequest};

    use super::*;

    #[test]
    fn hostname_collision_names_the_exact_remove_primitive() {
        let hostname = RouteHostname::try_new("web.example.com").expect("hostname");
        let route_id = RouteBindingRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("route id");

        assert_eq!(
            refusal_message(RouteAttachRefusal::HostnameAlreadyAttached {
                hostname: hostname.clone(),
                route_id: route_id.clone(),
                remove: RouteRemoveRequest {
                    hostname,
                    route_id: Some(route_id),
                },
            }),
            "hostname web.example.com is already attached by route 01ARZ3NDEKTSV4RRFFQ69G5FAV; remove it first with `ployz route rm web.example.com --id 01ARZ3NDEKTSV4RRFFQ69G5FAV`"
        );
    }
}
