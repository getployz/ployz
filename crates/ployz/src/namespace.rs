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
        JsonReply::Success(reply) => Ok(format!("created namespace {}\n", reply.namespace_name)),
        JsonReply::Refused(CorrosionNamespaceCreateRefusal::AlreadyExists { namespace_name }) => {
            Err(NamespaceExecutionError::AlreadyExists {
                namespace_name: namespace_name.to_string(),
            })
        }
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
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(CorrosionNamespaceRemoveReply::Removed { namespace_name }) => {
            Ok(format!("removed namespace {namespace_name}\n"))
        }
        JsonReply::Success(CorrosionNamespaceRemoveReply::AlreadyAbsent { namespace_name }) => {
            Ok(format!("namespace {namespace_name} is already absent\n"))
        }
        JsonReply::Refused(refusal) => Err(refusal.into()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NamespaceExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error("namespace {namespace_name} already exists")]
    AlreadyExists { namespace_name: String },
    #[error("namespace {namespace_name} was not found")]
    NotFound { namespace_name: String },
    #[error(
        "namespace {namespace_name} is not empty ({service_count} services, {route_binding_count} route bindings); remove those resources first"
    )]
    NotEmpty {
        namespace_name: String,
        service_count: usize,
        route_binding_count: usize,
    },
    #[error("namespace {namespace_name} changed while removal was being validated; retry")]
    Changed { namespace_name: String },
}

impl From<CorrosionNamespaceRemoveRefusal> for NamespaceExecutionError {
    fn from(refusal: CorrosionNamespaceRemoveRefusal) -> Self {
        match refusal {
            CorrosionNamespaceRemoveRefusal::NotFound { namespace_name } => Self::NotFound {
                namespace_name: namespace_name.as_str().to_owned(),
            },
            CorrosionNamespaceRemoveRefusal::NotEmpty {
                namespace_name,
                service_names,
                route_binding_count,
            } => Self::NotEmpty {
                namespace_name: namespace_name.to_string(),
                service_count: service_names.len(),
                route_binding_count,
            },
            CorrosionNamespaceRemoveRefusal::Changed { namespace_name } => Self::Changed {
                namespace_name: namespace_name.to_string(),
            },
        }
    }
}
