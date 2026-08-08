//! Execution and presentation for bounded named-row removals.

use hyper::Method;
use ployz_core::{
    NamedRemovalOutcome, PEER_REMOVE_ROUTE, PeerRemoveRefusal, PeerRemoveReply, PeerRemoveRequest,
    ROUTE_REMOVE_ROUTE, RouteRemoveRefusal, RouteRemoveReply, RouteRemoveRequest,
    SERVICE_REMOVE_ROW_ROUTE, ServiceRemoveRowRefusal, ServiceRemoveRowReply,
    ServiceRemoveRowRequest,
};

use crate::commands::{PeerCommand, RouteRemoveCommand, ServiceCommand};
use crate::init::ssh::shell_quote;
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
                name: command.name.clone(),
                peer_id: command.peer_id,
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(reply) => Ok(render_success(
            "peer",
            &command.name,
            reply.peer_id.as_str(),
            reply.outcome,
        )),
        JsonReply::Refused(refusal) => Err(peer_refusal(refusal)),
    }
}

pub async fn execute_service(command: ServiceCommand) -> Result<String, RemovalExecutionError> {
    let ServiceCommand::Remove(command) = command;
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let reply = remote
        .request_json_with_refusal::<_, ServiceRemoveRowReply, ServiceRemoveRowRefusal>(
            Method::POST,
            SERVICE_REMOVE_ROW_ROUTE,
            Some(&ServiceRemoveRowRequest {
                namespace_id: command.namespace_id.clone(),
                name: command.name.clone(),
                service_id: command.service_id,
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(reply) => Ok(render_success(
            "service",
            &command.name,
            reply.service_id.as_str(),
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
                route_id: command.route_id,
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(reply) => Ok(render_success(
            "route",
            &handle,
            reply.route_id.as_str(),
            reply.outcome,
        )),
        JsonReply::Refused(refusal) => Err(route_refusal(refusal)),
    }
}

fn peer_refusal(refusal: PeerRemoveRefusal) -> RemovalExecutionError {
    let message = match refusal {
        PeerRemoveRefusal::NotFound { name } => format!("peer {name} does not exist"),
        PeerRemoveRefusal::Ambiguous { name, peer_ids } => {
            ambiguous_message("peer", &name, peer_ids.iter().map(|id| id.as_str()))
        }
        PeerRemoveRefusal::NameMismatch {
            peer_id,
            requested,
            found,
        } => format!(
            "peer row {} is named {found}, not {requested}; no row was removed",
            peer_id.as_str()
        ),
        PeerRemoveRefusal::IdMismatch {
            requested,
            found,
            name,
        } => format!(
            "peer row {} is absent, but {name} is row {}; retry `ployz peer rm {} --id {}`",
            requested.as_str(),
            found.as_str(),
            shell_quote(&name),
            found.as_str()
        ),
        PeerRemoveRefusal::StoredRowUnselectable { peer_id } => format!(
            "peer row {} exists but this binary cannot safely select it; no row was removed",
            peer_id.as_str()
        ),
        PeerRemoveRefusal::ConcurrentMutation { peer_id } => format!(
            "peer row {} changed before removal; inspect it and retry",
            peer_id.as_str()
        ),
    };
    RemovalExecutionError::Refused { message }
}

fn service_refusal(refusal: ServiceRemoveRowRefusal) -> RemovalExecutionError {
    let message = match refusal {
        ServiceRemoveRowRefusal::NotFound { namespace_id, name } => format!(
            "service {name} does not exist in namespace row {}",
            namespace_id.as_str()
        ),
        ServiceRemoveRowRefusal::Ambiguous {
            namespace_id,
            name,
            service_ids,
        } => service_ambiguous_message(&namespace_id, &name, &service_ids),
        ServiceRemoveRowRefusal::IdentityMismatch {
            service_id,
            requested_namespace_id,
            requested_name,
            found_namespace_id,
            found_name,
        } => format!(
            "service row {} is {}/{found_name}, not {}/{requested_name}; no row was removed",
            service_id.as_str(),
            found_namespace_id.as_str(),
            requested_namespace_id.as_str()
        ),
        ServiceRemoveRowRefusal::IdMismatch {
            requested,
            found,
            namespace_id,
            name,
        } => format!(
            "service row {} is absent, but {name} in namespace row {} is row {}; retry `ployz service rm {} --namespace-id {} --id {}`",
            requested.as_str(),
            namespace_id.as_str(),
            found.as_str(),
            shell_quote(&name),
            namespace_id.as_str(),
            found.as_str()
        ),
        ServiceRemoveRowRefusal::StoredRowUnselectable { service_id } => format!(
            "service row {} exists but this binary cannot safely select it; no row was removed",
            service_id.as_str()
        ),
        ServiceRemoveRowRefusal::ConcurrentMutation { service_id } => format!(
            "service row {} changed before removal; inspect it and retry",
            service_id.as_str()
        ),
    };
    RemovalExecutionError::Refused { message }
}

fn service_ambiguous_message(
    namespace_id: &ployz_core::ids::NamespaceRowId,
    name: &str,
    service_ids: &[ployz_core::ids::ServiceRowId],
) -> String {
    let quoted_name = shell_quote(name);
    let choices = service_ids
        .iter()
        .map(|id| {
            format!(
                "`ployz service rm {quoted_name} --namespace-id {} --id {}`",
                namespace_id.as_str(),
                id.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "service {name} in namespace row {} is ambiguous; choose an exact row: {choices}",
        namespace_id.as_str()
    )
}

fn route_refusal(refusal: RouteRemoveRefusal) -> RemovalExecutionError {
    let message = match refusal {
        RouteRemoveRefusal::NotFound { hostname } => {
            format!("route {} does not exist", hostname.as_str())
        }
        RouteRemoveRefusal::Ambiguous {
            hostname,
            route_ids,
        } => ambiguous_message(
            "route",
            hostname.as_str(),
            route_ids.iter().map(|id| id.as_str()),
        ),
        RouteRemoveRefusal::NameMismatch {
            route_id,
            requested,
            found,
        } => format!(
            "route row {} is for {}, not {}; no row was removed",
            route_id.as_str(),
            found.as_str(),
            requested.as_str()
        ),
        RouteRemoveRefusal::IdMismatch {
            requested,
            found,
            hostname,
        } => format!(
            "route row {} is absent, but {} is row {}; retry `ployz route rm {} --id {}`",
            requested.as_str(),
            hostname.as_str(),
            found.as_str(),
            hostname.as_str(),
            found.as_str()
        ),
        RouteRemoveRefusal::StoredRowUnselectable { route_id } => format!(
            "route row {} exists but this binary cannot safely select it; no row was removed",
            route_id.as_str()
        ),
        RouteRemoveRefusal::ConcurrentMutation { route_id } => format!(
            "route row {} changed before removal; inspect it and retry",
            route_id.as_str()
        ),
    };
    RemovalExecutionError::Refused { message }
}

fn ambiguous_message<'a>(
    noun: &str,
    handle: &str,
    ids: impl IntoIterator<Item = &'a str>,
) -> String {
    let quoted_handle = shell_quote(handle);
    let choices = ids
        .into_iter()
        .map(|id| format!("`ployz {noun} rm {quoted_handle} --id {id}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{noun} {handle} is ambiguous; choose an exact row: {choices}")
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
    use ployz_core::ids::PeerId;

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
    fn ambiguity_names_every_copy_ready_exact_id_retry() {
        let error = peer_refusal(PeerRemoveRefusal::Ambiguous {
            name: "operator".to_owned(),
            peer_ids: vec![
                PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("id"),
                PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("id"),
            ],
        });
        assert_eq!(
            error.to_string(),
            "peer operator is ambiguous; choose an exact row: `ployz peer rm 'operator' --id 01ARZ3NDEKTSV4RRFFQ69G5FAV`, `ployz peer rm 'operator' --id 01ARZ3NDEKTSV4RRFFQ69G5FAW`"
        );
    }

    #[test]
    fn ambiguity_shell_quotes_whitespace_and_metacharacters_in_retry_handles() {
        let handle = "dind laptop'; touch /tmp/not-run #";
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

        assert_eq!(
            ambiguous_message("peer", handle, [id]),
            format!(
                "peer {handle} is ambiguous; choose an exact row: `ployz peer rm {} --id {id}`",
                shell_quote(handle)
            )
        );
    }

    #[test]
    fn id_mismatch_names_the_copy_ready_exact_retry() {
        let requested = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("requested id");
        let found = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("found id");
        let name = "dind laptop'; touch /tmp/not-run #";

        let error = peer_refusal(PeerRemoveRefusal::IdMismatch {
            requested: requested.clone(),
            found: found.clone(),
            name: name.to_owned(),
        });

        assert_eq!(
            error.to_string(),
            format!(
                "peer row {} is absent, but {name} is row {}; retry `ployz peer rm {} --id {}`",
                requested.as_str(),
                found.as_str(),
                shell_quote(name),
                found.as_str()
            )
        );
    }
}
