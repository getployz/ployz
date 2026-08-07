//! First-deploy submission and progress attachment.

use std::io::Write as _;

use hyper::Method;
use ployz_core::deploy::ContainerRuntimeSpec;
use ployz_core::{DEPLOY_ROUTE, DeployAccepted, DeployRefusal, DeployRequest};

use crate::commands::{DeployCommand, OpsWatchCommand};
use crate::mesh::http::JsonReply;
use crate::ops::{OpsExecutionError, watch_to};
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: DeployCommand) -> Result<String, DeployExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let reply = remote
        .request_json_with_refusal::<_, DeployAccepted, DeployRefusal>(
            Method::POST,
            DEPLOY_ROUTE,
            Some(&deploy_request(&command)),
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

fn deploy_request(command: &DeployCommand) -> DeployRequest {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.environment = command.environment.clone();
    DeployRequest {
        namespace_name: command.namespace.clone(),
        service_name: command.service.clone(),
        image: command.image.clone(),
        runtime,
        health_gate: command.health_gate,
    }
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
        "namespace {namespace_id} is held by service {incumbent_service_name}; deploy that service or run `ployz service remove {incumbent_service_name}` first"
    )]
    DifferentService {
        namespace_id: String,
        incumbent_service_name: String,
    },
    #[error(
        "namespace {namespace_id} holds multiple services ({service_ids}); remove the extras before deploying"
    )]
    MultipleServices {
        namespace_id: String,
        service_ids: String,
    },
    #[error(
        "namespace {namespace_id} has route bindings but no service; remove the routes before deploying"
    )]
    RoutesWithoutServices { namespace_id: String },
    #[error("the incumbent service is pinned to machine {machine_id}; deploy through that machine")]
    IncumbentOnAnotherMachine { machine_id: String },
    #[error("the required ployz container bridge is unavailable on the driver")]
    BridgeUnavailable,
}

impl From<DeployRefusal> for DeployExecutionError {
    fn from(refusal: DeployRefusal) -> Self {
        match refusal {
            DeployRefusal::NamespaceNotFound {
                namespace_name,
                create_command,
            } => Self::NamespaceNotFound {
                namespace_name: namespace_name.as_str().to_owned(),
                create_command,
            },
            DeployRefusal::NamespaceAmbiguous {
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
            DeployRefusal::DifferentService {
                namespace_id,
                incumbent_service_name,
            } => Self::DifferentService {
                namespace_id: namespace_id.to_string(),
                incumbent_service_name: incumbent_service_name.as_str().to_owned(),
            },
            DeployRefusal::MultipleServices {
                namespace_id,
                service_ids,
            } => Self::MultipleServices {
                namespace_id: namespace_id.to_string(),
                service_ids: service_ids
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            },
            DeployRefusal::RoutesWithoutServices { namespace_id } => Self::RoutesWithoutServices {
                namespace_id: namespace_id.to_string(),
            },
            DeployRefusal::IncumbentOnAnotherMachine { machine_id } => {
                Self::IncumbentOnAnotherMachine {
                    machine_id: machine_id.to_string(),
                }
            }
            DeployRefusal::BridgeUnavailable => Self::BridgeUnavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ployz_core::HealthGatePolicy;
    use ployz_core::corrosion::{CorrosionNamespaceName, CorrosionServiceName};
    use ployz_core::deploy::{ImageReference, ServiceEnvironment};

    use super::*;

    fn command(health_gate: HealthGatePolicy) -> DeployCommand {
        DeployCommand {
            namespace: CorrosionNamespaceName::try_new("production").expect("namespace name"),
            service: CorrosionServiceName::try_new("web").expect("service name"),
            image: ImageReference::try_new("registry.example/web:latest").expect("image"),
            environment: ServiceEnvironment::from(BTreeMap::new()),
            health_gate,
            target: None,
        }
    }

    #[test]
    fn deploy_request_carries_the_commanded_health_gate_policy() {
        for (policy, expected) in [
            (HealthGatePolicy::Enforce, "enforce"),
            (HealthGatePolicy::Skip, "skip"),
        ] {
            let body = serde_json::to_value(deploy_request(&command(policy)))
                .expect("deploy request serializes");
            assert_eq!(body.get("health_gate"), Some(&serde_json::json!(expected)));
            assert_eq!(
                body.get("namespace_name"),
                Some(&serde_json::json!("production"))
            );
            assert_eq!(body.get("service_name"), Some(&serde_json::json!("web")));
        }
    }
}
