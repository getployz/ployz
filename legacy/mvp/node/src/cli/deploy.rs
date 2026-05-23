use std::path::PathBuf;

use mvp_node::{
    NodeError, NodeResult, ProductDeployOptions, deploy_product_service,
    deploy_product_service_with_runtime, read_product_deploy_status,
};
use serde::Serialize;

use super::control::daemon_control_request;
use super::runtime::{DeployRuntimeArgs, container_shell_command, docker_runtime_backend};

#[derive(Clone)]
pub(crate) struct DeployArgs {
    pub(crate) state_dir: Option<PathBuf>,
    control_socket: Option<PathBuf>,
    deploy_id: String,
    target_node: String,
    service: String,
    revision: String,
    hostname: String,
    runtime: DeployRuntimeArgs,
}

pub(crate) struct DeployStatusArgs {
    state_dir: PathBuf,
    deploy_id: String,
}

#[derive(Serialize)]
struct DeployStatusResponse {
    deploy_id: String,
    statuses: Vec<DeployStatusEntry>,
}

#[derive(Serialize)]
struct DeployStatusEntry {
    sequence: u64,
    phase: mvp_deploy::DeployStatusPhase,
    serving_epoch: Option<u64>,
    message: Option<String>,
}

impl From<mvp_node::ProductDeployStatusReport> for DeployStatusResponse {
    fn from(value: mvp_node::ProductDeployStatusReport) -> Self {
        Self {
            deploy_id: value.deploy_id.to_string(),
            statuses: value
                .statuses
                .into_iter()
                .map(|status| DeployStatusEntry {
                    sequence: status.sequence,
                    phase: status.phase,
                    serving_epoch: status.serving_epoch,
                    message: status.message,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct DaemonDeployRequest<'a> {
    command: &'static str,
    deploy_id: &'a str,
    target_node: &'a str,
    service: &'a str,
    revision: &'a str,
    hostname: &'a str,
}

impl<'a> From<&'a DeployArgs> for DaemonDeployRequest<'a> {
    fn from(value: &'a DeployArgs) -> Self {
        Self {
            command: "deploy",
            deploy_id: &value.deploy_id,
            target_node: &value.target_node,
            service: &value.service,
            revision: &value.revision,
            hostname: &value.hostname,
        }
    }
}

pub(crate) fn deploy(args: &[String]) -> NodeResult<String> {
    let parsed = DeployArgs::parse(args)?;
    if let Some(control_socket) = parsed.control_socket.clone() {
        return daemon_deploy(control_socket, &parsed);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    let options = parsed.clone().into_options()?;
    let runtime_backend = parsed.runtime_backend()?;
    let report = match runtime_backend {
        Some(runtime_backend) => runtime.block_on(deploy_product_service_with_runtime(
            options,
            Some(runtime_backend),
        ))?,
        None => runtime.block_on(deploy_product_service(options))?,
    };
    let active = report
        .active_backends
        .iter()
        .map(|backend| format!("{}@{}", backend.node_id, backend.address))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        "deployed id={} active_backends={} old_backends={} visible_nodes={} host_network_backends={}",
        report.deploy_id,
        active,
        report.old_backends_to_drain.len(),
        report.visible_nodes,
        report.host_network_backends
    ))
}

fn daemon_deploy(control_socket: PathBuf, parsed: &DeployArgs) -> NodeResult<String> {
    let request = serde_json::to_vec(&DaemonDeployRequest::from(parsed))
        .map_err(|source| NodeError::EncodeNodeAgentRpc { source })?;
    let response = daemon_control_request(control_socket, &request)?;
    let value = serde_json::from_str::<serde_json::Value>(response.trim()).map_err(|source| {
        NodeError::NodeAgentRpc {
            message: format!("daemon deploy returned invalid JSON: {source}"),
        }
    })?;
    match value.get("status").and_then(serde_json::Value::as_str) {
        Some("deployed") => {}
        Some("failed") => {
            return Err(NodeError::NodeAgentRpc {
                message: value
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("daemon deploy failed")
                    .to_string(),
            });
        }
        Some(status) => {
            return Err(NodeError::NodeAgentRpc {
                message: format!("daemon deploy returned unexpected status '{status}'"),
            });
        }
        None => {
            return Err(NodeError::NodeAgentRpc {
                message: "daemon deploy response missing status".to_string(),
            });
        }
    }
    Ok(response.trim().to_string())
}

pub(crate) fn deploy_status(args: &[String]) -> NodeResult<String> {
    let parsed = DeployStatusArgs::parse(args)?;
    let report = read_product_deploy_status(parsed.state_dir, parsed.deploy_id)?;
    serde_json::to_string(&DeployStatusResponse::from(report))
        .map_err(|source| NodeError::EncodeNodeAgentRpc { source })
}

impl DeployArgs {
    pub(crate) fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut deploy_id = None;
        let mut target_node = None;
        let mut service = None;
        let mut revision = None;
        let mut hostname = None;
        let mut control_socket = None;
        let mut deploy_runtime = DeployRuntimeArgs::Process;
        let mut docker_image = None;
        let mut docker_service_port = None;
        let mut docker_command = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    state_dir = Some(PathBuf::from(value));
                }
                "--control" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--control" });
                    };
                    control_socket = Some(PathBuf::from(value));
                }
                "--deploy-id" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--deploy-id",
                        });
                    };
                    deploy_id = Some(value.clone());
                }
                "--target-node" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--target-node",
                        });
                    };
                    target_node = Some(value.clone());
                }
                "--service" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--service" });
                    };
                    service = Some(value.clone());
                }
                "--revision" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--revision" });
                    };
                    revision = Some(value.clone());
                }
                "--hostname" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--hostname" });
                    };
                    hostname = Some(value.clone());
                }
                "--runtime" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--runtime" });
                    };
                    deploy_runtime = match value.as_str() {
                        "process" => DeployRuntimeArgs::Process,
                        "docker" => DeployRuntimeArgs::Docker {
                            image: String::new(),
                            service_port: 8080,
                            command: None,
                        },
                        _ => {
                            return Err(NodeError::UnknownArgument {
                                argument: value.clone(),
                            });
                        }
                    };
                }
                "--image" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--image" });
                    };
                    docker_image = Some(value.clone());
                }
                "--service-port" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--service-port",
                        });
                    };
                    docker_service_port =
                        Some(
                            value
                                .parse::<u16>()
                                .map_err(|_| NodeError::UnknownArgument {
                                    argument: value.clone(),
                                })?,
                        );
                }
                "--container-command" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--container-command",
                        });
                    };
                    docker_command = Some(container_shell_command(value));
                }
                other => {
                    return Err(NodeError::UnknownArgument {
                        argument: other.to_string(),
                    });
                }
            }
        }
        Ok(Self {
            state_dir,
            control_socket,
            deploy_id: deploy_id.unwrap_or_else(|| "deploy-main".to_string()),
            target_node: target_node.ok_or(NodeError::MissingFlagValue {
                flag: "--target-node",
            })?,
            service: service.unwrap_or_else(|| "web".to_string()),
            revision: revision.unwrap_or_else(|| "rev-1".to_string()),
            hostname: hostname.unwrap_or_else(|| "web.example.test".to_string()),
            runtime: match deploy_runtime {
                DeployRuntimeArgs::Process => DeployRuntimeArgs::Process,
                DeployRuntimeArgs::Docker { .. } => DeployRuntimeArgs::Docker {
                    image: docker_image.ok_or(NodeError::MissingFlagValue { flag: "--image" })?,
                    service_port: docker_service_port.unwrap_or(8080),
                    command: docker_command,
                },
            },
        })
    }

    fn into_options(self) -> NodeResult<ProductDeployOptions> {
        Ok(ProductDeployOptions::new(
            self.state_dir
                .ok_or(NodeError::MissingFlagValue { flag: "--state" })?,
        )
        .with_deploy_id(self.deploy_id)
        .with_target_node(self.target_node)
        .with_service(self.service)
        .with_revision(self.revision)
        .with_hostname(self.hostname))
    }

    fn runtime_backend(
        &self,
    ) -> NodeResult<Option<std::sync::Arc<dyn mvp_runtime::RuntimeBackend>>> {
        match &self.runtime {
            DeployRuntimeArgs::Process => Ok(None),
            DeployRuntimeArgs::Docker {
                image,
                service_port,
                command,
            } => docker_runtime_backend(
                self.state_dir.as_ref(),
                image,
                *service_port,
                command.as_deref(),
            ),
        }
    }
}

impl DeployStatusArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut deploy_id = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    state_dir = Some(PathBuf::from(value));
                }
                "--deploy-id" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--deploy-id",
                        });
                    };
                    deploy_id = Some(value.clone());
                }
                other => {
                    return Err(NodeError::UnknownArgument {
                        argument: other.to_string(),
                    });
                }
            }
        }
        Ok(Self {
            state_dir: state_dir.ok_or(NodeError::MissingFlagValue { flag: "--state" })?,
            deploy_id: deploy_id.unwrap_or_else(|| "deploy-main".to_string()),
        })
    }
}
