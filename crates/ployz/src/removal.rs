//! Execution and presentation for bounded named-row removals.

use hyper::Method;
use ployz_core::{
    NamedRemovalOutcome, PEER_REMOVE_ROUTE, PeerRemoveRefusal, PeerRemoveReply, PeerRemoveRequest,
    ROUTE_REMOVE_ROUTE, RouteRemoveRefusal, RouteRemoveReply, RouteRemoveRequest,
    SERVICE_REMOVE_ROUTE, ServiceRemoveRefusal, ServiceRemoveReply, ServiceRemoveRequest,
};

use crate::commands::{PeerCommand, RouteRemoveCommand, ServiceCommand};
use crate::mesh::http::JsonReply;
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute_peer(command: PeerCommand) -> Result<String, RemovalExecutionError> {
    let PeerCommand::Remove(command) = command;
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let reply = remote
        .request_json_with_refusal::<_, PeerRemoveReply, PeerRemoveRefusal>(
            Method::POST,
            PEER_REMOVE_ROUTE,
            Some(&PeerRemoveRequest {
                peer_name: command.name.clone(),
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(reply) => Ok(render_success(
            "peer",
            command.name.as_str(),
            reply.peer_name.as_str(),
            reply.outcome,
        )),
        JsonReply::Refused(refusal) => Err(peer_refusal(refusal)),
    }
}

pub async fn execute_service(command: ServiceCommand) -> Result<String, RemovalExecutionError> {
    let ServiceCommand::Remove(command) = command;
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let reply = remote
        .request_json_with_refusal::<_, ServiceRemoveReply, ServiceRemoveRefusal>(
            Method::POST,
            SERVICE_REMOVE_ROUTE,
            Some(&ServiceRemoveRequest {
                namespace_name: command.namespace_name.clone(),
                service_name: command.service_name.clone(),
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(reply) => Ok(render_success(
            "service",
            command.service_name.as_str(),
            &format!("{}/{}", reply.namespace_name, reply.service_name),
            reply.outcome,
        )),
        JsonReply::Refused(refusal) => Err(service_refusal(refusal)),
    }
}

pub async fn execute_route_remove(
    command: RouteRemoveCommand,
) -> Result<String, RemovalExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let handle = command.hostname.as_str().to_owned();
    let reply = remote
        .request_json_with_refusal::<_, RouteRemoveReply, RouteRemoveRefusal>(
            Method::POST,
            ROUTE_REMOVE_ROUTE,
            Some(&RouteRemoveRequest {
                hostname: command.hostname,
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(reply) => Ok(render_success(
            "route",
            &handle,
            reply.hostname.as_str(),
            reply.outcome,
        )),
        JsonReply::Refused(refusal) => Err(route_refusal(refusal)),
    }
}

fn peer_refusal(refusal: PeerRemoveRefusal) -> RemovalExecutionError {
    let message = match refusal {
        PeerRemoveRefusal::NotFound { peer_name } => format!("peer {peer_name} does not exist"),
        PeerRemoveRefusal::StoredRowUnselectable { peer_name } => format!(
            "peer {} exists but its row is invalid; no row was removed",
            peer_name.as_str()
        ),
        PeerRemoveRefusal::ConcurrentMutation { peer_name } => format!(
            "peer {} changed before removal; inspect it and retry",
            peer_name.as_str()
        ),
    };
    RemovalExecutionError::Refused { message }
}

fn service_refusal(refusal: ServiceRemoveRefusal) -> RemovalExecutionError {
    let message = match refusal {
        ServiceRemoveRefusal::NotFound {
            namespace_name,
            service_name,
        } => format!("service {}/{} does not exist", namespace_name, service_name),
        ServiceRemoveRefusal::NamespaceStoredRowUnselectable { namespace_name } => format!(
            "namespace {} exists but its document is invalid; service removal was not attempted",
            namespace_name.as_str()
        ),
        ServiceRemoveRefusal::ConcurrentMutation {
            namespace_name,
            service_name,
        } => format!(
            "service {}/{} changed before removal; inspect it and retry",
            namespace_name, service_name
        ),
    };
    RemovalExecutionError::Refused { message }
}

fn route_refusal(refusal: RouteRemoveRefusal) -> RemovalExecutionError {
    let message = match refusal {
        RouteRemoveRefusal::NotFound { hostname } => {
            format!("route {} does not exist", hostname.as_str())
        }
        RouteRemoveRefusal::StoredRowUnselectable { hostname } => format!(
            "route {} exists but its row is invalid; no row was removed",
            hostname.as_str()
        ),
        RouteRemoveRefusal::ConcurrentMutation { hostname } => format!(
            "route {} changed before removal; inspect it and retry",
            hostname.as_str()
        ),
    };
    RemovalExecutionError::Refused { message }
}

#[must_use]
pub fn render_success(kind: &str, handle: &str, id: &str, outcome: NamedRemovalOutcome) -> String {
    match outcome {
        NamedRemovalOutcome::Removed => format!("removed {kind} {handle} ({id})\n"),
        NamedRemovalOutcome::AlreadyAbsent => {
            format!("{kind} {handle} ({id}) was already absent\n")
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RemovalExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error("{message}")]
    Refused { message: String },
}

#[cfg(test)]
mod tests {
    use ployz_core::ids::PeerName;

    use super::*;

    #[test]
    fn success_copy_distinguishes_deletion_from_idempotent_retry() {
        assert_eq!(
            render_success("service", "web", "ROW", NamedRemovalOutcome::Removed),
            "removed service web (ROW)\n"
        );
        assert_eq!(
            render_success("service", "web", "ROW", NamedRemovalOutcome::AlreadyAbsent),
            "service web (ROW) was already absent\n"
        );
    }

    #[test]
    fn peer_refusal_uses_the_canonical_name() {
        let error = peer_refusal(PeerRemoveRefusal::NotFound {
            peer_name: PeerName::try_new("operator").expect("name"),
        });
        assert_eq!(error.to_string(), "peer operator does not exist");
    }
}
