use std::path::PathBuf;
use std::time::Duration;

use mvp_node::{DaemonOptions, NodeError, NodeResult, run_daemon_once};

use super::runtime::{DeployRuntimeArgs, container_shell_command};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DaemonArgs {
    pub(crate) state_dir: PathBuf,
    pub(crate) run_for_ms: Option<u64>,
    pub(crate) control_socket: Option<PathBuf>,
    pub(crate) linux_wireguard_ifname: Option<String>,
    pub(crate) linux_wireguard_listen_port: Option<u16>,
    pub(crate) runtime: DeployRuntimeArgs,
}

pub(crate) fn daemon(args: &[String]) -> NodeResult<String> {
    let parsed = DaemonArgs::parse(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    let mut options = DaemonOptions::new(Duration::from_millis(parsed.run_for_ms.unwrap_or(1_000)));
    if let Some(control_socket) = parsed.control_socket {
        options = options.with_control_socket(control_socket);
    }
    if let Some(ifname) = parsed.linux_wireguard_ifname {
        options = match parsed.linux_wireguard_listen_port {
            Some(port) => options.with_linux_wireguard_listen_port(ifname, port),
            None => options.with_linux_wireguard(ifname),
        };
    }
    if let DeployRuntimeArgs::Docker {
        image,
        service_port,
        command,
    } = parsed.runtime
    {
        options = options.with_docker_runtime(image, service_port, command);
    }
    let report = runtime.block_on(run_daemon_once(parsed.state_dir, options))?;
    Ok(format!(
        "daemon node={} ticket={} imported_batches={} imported_operations={} node_agent_handlers={} wireguard_backend={} wireguard_applied_revision={}",
        report.node_id,
        report.ticket,
        report.imported_batches,
        report.imported_operations,
        report.node_agent_handlers,
        report.wireguard_backend,
        report
            .wireguard_applied_revision
            .map_or_else(|| "none".to_string(), |revision| revision.to_string())
    ))
}

impl DaemonArgs {
    pub(crate) fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut run_for_ms = None;
        let mut control_socket = None;
        let mut linux_wireguard_ifname = None;
        let mut linux_wireguard_listen_port = None;
        let mut daemon_runtime = DeployRuntimeArgs::Process;
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
                "--run-for-ms" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--run-for-ms",
                        });
                    };
                    run_for_ms =
                        Some(
                            value
                                .parse::<u64>()
                                .map_err(|_| NodeError::UnknownArgument {
                                    argument: value.clone(),
                                })?,
                        );
                }
                "--control" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--control" });
                    };
                    control_socket = Some(PathBuf::from(value));
                }
                "--linux-wireguard-ifname" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--linux-wireguard-ifname",
                        });
                    };
                    linux_wireguard_ifname = Some(value.clone());
                }
                "--linux-wireguard-listen-port" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--linux-wireguard-listen-port",
                        });
                    };
                    linux_wireguard_listen_port =
                        Some(
                            value
                                .parse::<u16>()
                                .map_err(|_| NodeError::UnknownArgument {
                                    argument: value.clone(),
                                })?,
                        );
                }
                "--runtime" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--runtime" });
                    };
                    daemon_runtime = match value.as_str() {
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
            state_dir: state_dir.ok_or(NodeError::MissingFlagValue { flag: "--state" })?,
            run_for_ms,
            control_socket,
            linux_wireguard_ifname,
            linux_wireguard_listen_port,
            runtime: match daemon_runtime {
                DeployRuntimeArgs::Process => DeployRuntimeArgs::Process,
                DeployRuntimeArgs::Docker { .. } => DeployRuntimeArgs::Docker {
                    image: docker_image.ok_or(NodeError::MissingFlagValue { flag: "--image" })?,
                    service_port: docker_service_port.unwrap_or(8080),
                    command: docker_command,
                },
            },
        })
    }
}
