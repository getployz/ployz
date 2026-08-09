//! First-deploy submission and progress attachment.

use std::io::Write as _;

use hyper::Method;
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
        "accepted deploy {}/{} on controller {}",
        accepted.namespace_name, accepted.deploy_name, accepted.controller_machine_name
    )
    .map_err(DeployExecutionError::Output)?;
    stdout.flush().map_err(DeployExecutionError::Output)?;
    watch_to(
        &OpsWatchCommand {
            namespace_name: accepted.namespace_name,
            deploy_name: accepted.deploy_name,
            target: command.target,
        },
        &mut stdout,
    )
    .await?;
    Ok(String::new())
}

fn deploy_request(command: &DeployCommand) -> DeployRequest {
    DeployRequest {
        namespace_name: command.namespace.clone(),
        deploy_name: command.deploy.clone(),
        services: command.services.clone(),
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
        "deploy name {deploy_name} has already been used in namespace {namespace_name}; choose a new deploy name"
    )]
    DeployNameAlreadyUsed {
        namespace_name: String,
        deploy_name: String,
    },
    #[error(
        "named-volume redeploy is unsupported; remove the service row and local runtime explicitly before deploying again"
    )]
    NamedVolumeRedeployUnsupported,
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
            DeployRefusal::DeployNameAlreadyUsed {
                namespace_name,
                deploy_name,
            } => Self::DeployNameAlreadyUsed {
                namespace_name: namespace_name.to_string(),
                deploy_name: deploy_name.to_string(),
            },
            DeployRefusal::NamedVolumeRedeployUnsupported => Self::NamedVolumeRedeployUnsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::HealthGatePolicy;
    use ployz_core::corrosion::{CorrosionNamespaceName, CorrosionServiceName};
    use ployz_core::deploy::{ContainerRuntimeSpec, ImageReference};

    use super::*;

    fn command(health_gate: HealthGatePolicy) -> DeployCommand {
        DeployCommand {
            namespace: CorrosionNamespaceName::try_new("production").expect("namespace name"),
            deploy: ployz_core::ids::DeployName::try_new("release-1").expect("deploy"),
            services: vec![ployz_core::DeployServiceRequest {
                service_name: CorrosionServiceName::try_new("web").expect("service name"),
                image: ImageReference::try_new("registry.example/web:latest").expect("image"),
                credential: None,
                runtime: ContainerRuntimeSpec::image_defaults(),
                health_gate,
                placement: None,
                machines: None,
            }],
            target: None,
        }
    }

    fn first_service(value: &serde_json::Value) -> &serde_json::Value {
        value
            .get("services")
            .and_then(serde_json::Value::as_array)
            .and_then(|services| services.first())
            .expect("one serialized service")
    }

    #[test]
    fn deploy_request_carries_the_commanded_health_gate_policy() {
        for (policy, expected) in [
            (HealthGatePolicy::Enforce, "enforce"),
            (HealthGatePolicy::Skip, "skip"),
        ] {
            let body = serde_json::to_value(deploy_request(&command(policy)))
                .expect("deploy request serializes");
            assert_eq!(
                first_service(&body).get("health_gate"),
                Some(&serde_json::json!(expected))
            );
            assert_eq!(
                body.get("namespace_name"),
                Some(&serde_json::json!("production"))
            );
            assert_eq!(
                first_service(&body).get("service_name"),
                Some(&serde_json::json!("web"))
            );
        }
    }

    #[test]
    fn deploy_request_forwards_placement_and_pins_only_when_commanded() {
        let inherit = serde_json::to_value(deploy_request(&command(HealthGatePolicy::Enforce)))
            .expect("deploy request serializes");
        assert_eq!(first_service(&inherit).get("placement"), None);
        assert_eq!(first_service(&inherit).get("machines"), None);

        let mut placed = command(HealthGatePolicy::Enforce);
        let service = placed.services.first_mut().expect("one service");
        service.placement = Some(ployz_core::RequestedPlacement::Replicated {
            replicas: Some(
                ployz_core::corrosion::ServiceReplicaCount::try_new(3).expect("replica count"),
            ),
        });
        service.machines = Some(ployz_core::RequestedPins::Any);
        let body =
            serde_json::to_value(deploy_request(&placed)).expect("deploy request serializes");
        assert_eq!(
            first_service(&body).get("placement"),
            Some(&serde_json::json!({ "mode": "replicated", "replicas": 3 }))
        );
        assert_eq!(
            first_service(&body).get("machines"),
            Some(&serde_json::json!({ "kind": "any" }))
        );
    }

    #[test]
    fn named_volume_redeploy_refusal_is_explicit_about_manual_cleanup() {
        let refusal = DeployRefusal::NamedVolumeRedeployUnsupported;

        assert_eq!(
            DeployExecutionError::from(refusal).to_string(),
            "named-volume redeploy is unsupported; remove the service row and local runtime explicitly before deploying again"
        );
    }
}
