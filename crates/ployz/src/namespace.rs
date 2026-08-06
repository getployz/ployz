//! Namespace primitives and refusal presentation.

use hyper::Method;
use ployz_core::{
    CorrosionNamespaceCreateRefusal, CorrosionNamespaceCreateReply,
    CorrosionNamespaceCreateRequest, CorrosionNamespaceRemoveRefusal,
    CorrosionNamespaceRemoveReply, CorrosionNamespaceRemoveRequest, NAMESPACE_CREATE_ROUTE,
    NAMESPACE_REMOVE_ROUTE,
};

use crate::commands::{NamespaceCommand, NamespaceCreateCommand, NamespaceRemoveCommand};
use crate::mesh::http::JsonReply;
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: NamespaceCommand) -> Result<String, NamespaceExecutionError> {
    match command {
        NamespaceCommand::Create(command) => create(command).await,
        NamespaceCommand::Remove(command) => remove(command).await,
    }
}

async fn create(command: NamespaceCreateCommand) -> Result<String, NamespaceExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let reply = remote
        .request_json_with_refusal::<
            _,
            CorrosionNamespaceCreateReply,
            CorrosionNamespaceCreateRefusal,
        >(
            Method::POST,
            NAMESPACE_CREATE_ROUTE,
            Some(&CorrosionNamespaceCreateRequest {
                namespace_name: command.namespace,
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(reply) => Ok(format!(
            "created namespace {} ({})\n",
            reply.document.name.as_str(),
            reply.namespace_id
        )),
        JsonReply::Refused(CorrosionNamespaceCreateRefusal::NameAlreadyClaimed {
            namespace_name,
            winner,
        }) => Err(NamespaceExecutionError::NameAlreadyClaimed {
            namespace_name: namespace_name.as_str().to_owned(),
            winner: winner.to_string(),
        }),
    }
}

async fn remove(command: NamespaceRemoveCommand) -> Result<String, NamespaceExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let reply = remote
        .request_json_with_refusal::<
            _,
            CorrosionNamespaceRemoveReply,
            CorrosionNamespaceRemoveRefusal,
        >(
            Method::POST,
            NAMESPACE_REMOVE_ROUTE,
            Some(&CorrosionNamespaceRemoveRequest {
                namespace_name: command.namespace,
                namespace_id: command.namespace_id,
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(CorrosionNamespaceRemoveReply::Removed { namespace_id }) => {
            Ok(format!("removed namespace {namespace_id}\n"))
        }
        JsonReply::Success(CorrosionNamespaceRemoveReply::AlreadyAbsent { namespace_id }) => {
            Ok(format!("namespace {namespace_id} is already absent\n"))
        }
        JsonReply::Refused(refusal) => Err(refusal.into()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NamespaceExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error("namespace {namespace_name} is already claimed by {winner}")]
    NameAlreadyClaimed {
        namespace_name: String,
        winner: String,
    },
    #[error("namespace {namespace_name} was not found")]
    NotFound { namespace_name: String },
    #[error(
        "namespace {namespace_name} is ambiguous; retry with `ployz namespace rm {namespace_name} --id <ID>` using one of: {namespace_ids}"
    )]
    Ambiguous {
        namespace_name: String,
        namespace_ids: String,
    },
    #[error("namespace id {namespace_id} does not have name {namespace_name}")]
    IdMismatch {
        namespace_name: String,
        namespace_id: String,
    },
    #[error(
        "namespace {namespace_id} is not empty ({service_count} services, {route_binding_count} route bindings); remove those resources first"
    )]
    NotEmpty {
        namespace_id: String,
        service_count: usize,
        route_binding_count: usize,
    },
    #[error("namespace {namespace_id} changed while removal was being validated; retry")]
    Changed { namespace_id: String },
}

impl From<CorrosionNamespaceRemoveRefusal> for NamespaceExecutionError {
    fn from(refusal: CorrosionNamespaceRemoveRefusal) -> Self {
        match refusal {
            CorrosionNamespaceRemoveRefusal::NotFound { namespace_name } => Self::NotFound {
                namespace_name: namespace_name.as_str().to_owned(),
            },
            CorrosionNamespaceRemoveRefusal::Ambiguous {
                namespace_name,
                namespace_ids,
            } => Self::Ambiguous {
                namespace_name: namespace_name.as_str().to_owned(),
                namespace_ids: namespace_ids
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            },
            CorrosionNamespaceRemoveRefusal::IdMismatch {
                namespace_name,
                namespace_id,
            } => Self::IdMismatch {
                namespace_name: namespace_name.as_str().to_owned(),
                namespace_id: namespace_id.to_string(),
            },
            CorrosionNamespaceRemoveRefusal::NotEmpty {
                namespace_id,
                service_ids,
                route_binding_count,
            } => Self::NotEmpty {
                namespace_id: namespace_id.to_string(),
                service_count: service_ids.len(),
                route_binding_count,
            },
            CorrosionNamespaceRemoveRefusal::Changed { namespace_id } => Self::Changed {
                namespace_id: namespace_id.to_string(),
            },
        }
    }
}
