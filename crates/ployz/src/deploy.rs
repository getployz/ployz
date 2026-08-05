//! First-deploy submission and progress attachment.

use std::io::Write as _;

use hyper::Method;
use ployz_core::deploy::ContainerRuntimeSpec;
use ployz_core::{FIRST_DEPLOY_ROUTE, FirstDeployAccepted, FirstDeployRefusal, FirstDeployRequest};

use crate::commands::{DeployCommand, OpsWatchCommand};
use crate::mesh::http::JsonReply;
use crate::ops::{OpsExecutionError, watch_to};
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: DeployCommand) -> Result<String, DeployExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.environment = command.environment;
    let reply = remote
        .request_json_with_refusal::<_, FirstDeployAccepted, FirstDeployRefusal>(
            Method::POST,
            FIRST_DEPLOY_ROUTE,
            Some(&FirstDeployRequest {
                namespace_name: command.namespace,
                service_name: command.service,
                image: command.image,
                runtime,
            }),
        )
        .await?;
    let accepted = match reply {
        JsonReply::Success(accepted) => accepted,
        JsonReply::Refused(refusal) => return Err(refusal.into()),
    };

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(
        stdout,
        "accepted operation {} on driver {}",
        accepted.operation_id, accepted.driver_machine_id
    )
    .map_err(DeployExecutionError::Output)?;
    stdout.flush().map_err(DeployExecutionError::Output)?;
    watch_to(
        &OpsWatchCommand {
            operation_id: accepted.operation_id,
            target: command.target,
        },
        &mut stdout,
    )
    .await?;
    Ok(String::new())
}

#[derive(Debug, thiserror::Error)]
pub enum DeployExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error(transparent)]
    Watch(#[from] OpsExecutionError),
    #[error("cannot write deploy progress: {0}")]
    Output(std::io::Error),
    #[error("namespace {namespace_name} does not exist; run `{create_command}`")]
    NamespaceNotFound {
        namespace_name: String,
        create_command: String,
    },
    #[error(
        "namespace {namespace_name} is ambiguous; remove duplicate namespace rows using one of these ids: {namespace_ids}"
    )]
    NamespaceAmbiguous {
        namespace_name: String,
        namespace_ids: String,
    },
    #[error(
        "namespace {namespace_id} already contains deploy state; this command only deploys the first service"
    )]
    NotFirstDeploy { namespace_id: String },
    #[error("the required ployz container bridge is unavailable on the driver")]
    BridgeUnavailable,
}

impl From<FirstDeployRefusal> for DeployExecutionError {
    fn from(refusal: FirstDeployRefusal) -> Self {
        match refusal {
            FirstDeployRefusal::NamespaceNotFound {
                namespace_name,
                create_command,
            } => Self::NamespaceNotFound {
                namespace_name: namespace_name.as_str().to_owned(),
                create_command,
            },
            FirstDeployRefusal::NamespaceAmbiguous {
                namespace_name,
                namespace_ids,
            } => Self::NamespaceAmbiguous {
                namespace_name: namespace_name.as_str().to_owned(),
                namespace_ids: namespace_ids
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            },
            FirstDeployRefusal::NotFirstDeploy { namespace_id } => Self::NotFirstDeploy {
                namespace_id: namespace_id.to_string(),
            },
            FirstDeployRefusal::BridgeUnavailable => Self::BridgeUnavailable,
        }
    }
}
