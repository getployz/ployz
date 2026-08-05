//! Operator-peer mutations and human presentation.

use std::fmt;

use ployz_core::ids::PeerId;
use ployz_core::{PEER_REMOVE_ROUTE, PeerRemoveRefusal, PeerRemoveReply, PeerRemoveRequest};

use crate::commands::{PeerCommand, PeerRemoveCommand};
use crate::mesh::http::JsonReply;
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: PeerCommand) -> Result<String, PeerExecutionError> {
    match command {
        PeerCommand::Remove(command) => remove(command).await,
    }
}

async fn remove(command: PeerRemoveCommand) -> Result<String, PeerExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let request = PeerRemoveRequest {
        peer_name: command.peer.clone(),
        peer_id: command.peer_id.clone(),
    };
    let reply = remote
        .request_json_with_refusal::<_, PeerRemoveReply, PeerRemoveRefusal>(
            hyper::Method::POST,
            PEER_REMOVE_ROUTE,
            Some(&request),
        )
        .await?;
    let reply = match reply {
        JsonReply::Success(reply) => reply,
        JsonReply::Refused(PeerRemoveRefusal::NotFound { peer_name }) => {
            return Err(PeerExecutionError::NotFound { peer_name });
        }
        JsonReply::Refused(PeerRemoveRefusal::Ambiguous {
            peer_name,
            peer_ids,
        }) => {
            return Err(PeerRemovalAmbiguity {
                peer_name,
                peer_ids,
            }
            .into());
        }
        JsonReply::Refused(PeerRemoveRefusal::IdMismatch { peer_name, peer_id }) => {
            return Err(PeerExecutionError::IdMismatch { peer_name, peer_id });
        }
        JsonReply::Refused(refusal @ PeerRemoveRefusal::SelfRemoval { .. }) => {
            return Err(PeerExecutionError::SelfRemoval { refusal });
        }
    };
    if !reply_matches_requested_identity(&reply, command.peer_id.as_ref()) {
        return Err(PeerExecutionError::ReplyMismatch);
    }
    Ok(render_peer_removal(&command.peer, &reply))
}

#[must_use]
pub fn render_peer_removal(peer_name: &str, reply: &PeerRemoveReply) -> String {
    match reply {
        PeerRemoveReply::Removed { peer_id } => {
            format!("Removed peer {peer_name} ({}).\n", peer_id.as_str())
        }
        PeerRemoveReply::AlreadyAbsent { peer_id } => {
            format!(
                "Peer {peer_name} ({}) was already absent.\n",
                peer_id.as_str()
            )
        }
    }
}

fn reply_matches_requested_identity(
    reply: &PeerRemoveReply,
    requested_peer_id: Option<&PeerId>,
) -> bool {
    match (reply, requested_peer_id) {
        (PeerRemoveReply::Removed { .. }, None) => true,
        (PeerRemoveReply::Removed { peer_id }, Some(requested_peer_id))
        | (PeerRemoveReply::AlreadyAbsent { peer_id }, Some(requested_peer_id)) => {
            peer_id == requested_peer_id
        }
        (PeerRemoveReply::AlreadyAbsent { .. }, None) => false,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PeerExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error("peer {peer_name} is not in the accepted roster")]
    NotFound { peer_name: String },
    #[error(transparent)]
    Ambiguous(#[from] PeerRemovalAmbiguity),
    #[error(
        "peer {peer_name} does not match identity {peer_id}; omit `--id` to resolve the name again, or retry with a matching identity"
    )]
    IdMismatch { peer_name: String, peer_id: PeerId },
    #[error("{refusal}")]
    SelfRemoval { refusal: PeerRemoveRefusal },
    #[error("cluster API peer-removal reply did not match the requested identity")]
    ReplyMismatch,
}

#[derive(Debug)]
pub struct PeerRemovalAmbiguity {
    peer_name: String,
    peer_ids: Vec<PeerId>,
}

impl fmt::Display for PeerRemovalAmbiguity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "peer {} is ambiguous; retry with one of:",
            self.peer_name
        )?;
        for peer_id in &self.peer_ids {
            write!(
                formatter,
                "\n  ployz peer rm {} --id {}",
                self.peer_name,
                peer_id.as_str()
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for PeerRemovalAmbiguity {}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer_id() -> PeerId {
        PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("peer id")
    }

    #[test]
    fn removal_presentation_distinguishes_deleted_from_idempotent_retry() {
        let peer_id = peer_id();
        assert_eq!(
            render_peer_removal(
                "operator-laptop",
                &PeerRemoveReply::Removed {
                    peer_id: peer_id.clone(),
                },
            ),
            format!("Removed peer operator-laptop ({peer_id}).\n")
        );
        assert_eq!(
            render_peer_removal(
                "operator-laptop",
                &PeerRemoveReply::AlreadyAbsent { peer_id },
            ),
            "Peer operator-laptop (01ARZ3NDEKTSV4RRFFQ69G5FAY) was already absent.\n"
        );
    }

    #[test]
    fn identity_qualified_reply_must_echo_the_requested_peer() {
        let expected = peer_id();
        let other = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAZ").expect("other peer id");
        assert!(reply_matches_requested_identity(
            &PeerRemoveReply::Removed {
                peer_id: expected.clone(),
            },
            Some(&expected),
        ));
        assert!(!reply_matches_requested_identity(
            &PeerRemoveReply::Removed { peer_id: other },
            Some(&expected),
        ));
    }
}
