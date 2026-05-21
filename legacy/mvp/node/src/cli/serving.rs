use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use mvp_node::{
    AcmeIssueOptions, NodeError, NodeResult, ServingRoleOptions, issue_product_certificate,
    run_dns_role, run_gateway_role,
};

pub(crate) fn acme_issue(args: &[String]) -> NodeResult<String> {
    let parsed = AcmeIssueArgs::parse(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    let report = runtime.block_on(issue_product_certificate(parsed.into_options()))?;
    serde_json::to_string(&report).map_err(|source| NodeError::EncodeNodeAgentRpc { source })
}

pub(crate) fn gateway(args: &[String]) -> NodeResult<String> {
    let parsed = ServingRoleArgs::parse(args, ServingRoleCommand::Gateway)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    runtime.block_on(run_gateway_role(parsed.into_options()))?;
    Ok("gateway stopped".to_string())
}

pub(crate) fn dns(args: &[String]) -> NodeResult<String> {
    let parsed = ServingRoleArgs::parse(args, ServingRoleCommand::Dns)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    runtime.block_on(run_dns_role(parsed.into_options()))?;
    Ok("dns stopped".to_string())
}

pub(crate) fn runtime_http(args: &[String]) -> NodeResult<String> {
    let parsed = RuntimeHttpArgs::parse(args)?;
    mvp_runtime::run_static_http_server(&parsed.addr, &parsed.root)
        .map_err(|source| NodeError::RuntimeBackend { source })?;
    Ok("runtime-http stopped".to_string())
}

struct RuntimeHttpArgs {
    addr: String,
    root: PathBuf,
}

struct ServingRoleArgs {
    state_dir: PathBuf,
    listen: SocketAddr,
    tls_listen: Option<SocketAddr>,
    control_socket: PathBuf,
    stale_after_ms: Option<u64>,
}

struct AcmeIssueArgs {
    state_dir: PathBuf,
    hostname: String,
    gateway_url: String,
    gateway_control_socket: Option<PathBuf>,
    issuer_holder: Option<String>,
    account_path: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum ServingRoleCommand {
    Gateway,
    Dns,
}

impl ServingRoleArgs {
    fn parse(args: &[String], command: ServingRoleCommand) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut listen = None;
        let mut tls_listen = None;
        let mut control_socket = None;
        let mut stale_after_ms = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    state_dir = Some(PathBuf::from(value));
                }
                "--listen" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--listen" });
                    };
                    listen = Some(value.parse::<SocketAddr>().map_err(|_| {
                        NodeError::UnknownArgument {
                            argument: value.clone(),
                        }
                    })?);
                }
                "--tls-listen" if matches!(command, ServingRoleCommand::Gateway) => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--tls-listen",
                        });
                    };
                    tls_listen = Some(value.parse::<SocketAddr>().map_err(|_| {
                        NodeError::UnknownArgument {
                            argument: value.clone(),
                        }
                    })?);
                }
                "--control" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--control" });
                    };
                    control_socket = Some(PathBuf::from(value));
                }
                "--stale-after-ms" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--stale-after-ms",
                        });
                    };
                    stale_after_ms =
                        Some(
                            value
                                .parse::<u64>()
                                .map_err(|_| NodeError::UnknownArgument {
                                    argument: value.clone(),
                                })?,
                        );
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
            listen: listen.ok_or(NodeError::MissingFlagValue { flag: "--listen" })?,
            tls_listen,
            control_socket: control_socket
                .ok_or(NodeError::MissingFlagValue { flag: "--control" })?,
            stale_after_ms,
        })
    }

    fn into_options(self) -> ServingRoleOptions {
        let mut options = ServingRoleOptions::new(self.state_dir, self.listen, self.control_socket);
        if let Some(tls_listen) = self.tls_listen {
            options = options.with_tls_listen(tls_listen);
        }
        match self.stale_after_ms {
            Some(stale_after_ms) => options.with_stale_after(Duration::from_millis(stale_after_ms)),
            None => options,
        }
    }
}

impl AcmeIssueArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut hostname = None;
        let mut gateway_url = None;
        let mut gateway_control_socket = None;
        let mut issuer_holder = None;
        let mut account_path = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    state_dir = Some(PathBuf::from(value));
                }
                "--hostname" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--hostname" });
                    };
                    hostname = Some(value.clone());
                }
                "--gateway" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--gateway" });
                    };
                    gateway_url = Some(value.clone());
                }
                "--gateway-control" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--gateway-control",
                        });
                    };
                    gateway_control_socket = Some(PathBuf::from(value));
                }
                "--issuer-holder" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--issuer-holder",
                        });
                    };
                    issuer_holder = Some(value.clone());
                }
                "--account-path" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--account-path",
                        });
                    };
                    account_path = Some(PathBuf::from(value));
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
            hostname: hostname.ok_or(NodeError::MissingFlagValue { flag: "--hostname" })?,
            gateway_url: gateway_url.ok_or(NodeError::MissingFlagValue { flag: "--gateway" })?,
            gateway_control_socket,
            issuer_holder,
            account_path,
        })
    }

    fn into_options(self) -> AcmeIssueOptions {
        let mut options = AcmeIssueOptions::new(self.state_dir, self.hostname, self.gateway_url);
        if let Some(gateway_control_socket) = self.gateway_control_socket {
            options = options.with_gateway_control_socket(gateway_control_socket);
        }
        if let Some(issuer_holder) = self.issuer_holder {
            options = options.with_issuer_holder(issuer_holder);
        }
        if let Some(account_path) = self.account_path {
            options = options.with_account_path(account_path);
        }
        options
    }
}

impl RuntimeHttpArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut addr = None;
        let mut root = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--addr" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--addr" });
                    };
                    addr = Some(value.clone());
                }
                "--root" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--root" });
                    };
                    root = Some(PathBuf::from(value));
                }
                other => {
                    return Err(NodeError::UnknownArgument {
                        argument: other.to_string(),
                    });
                }
            }
        }
        Ok(Self {
            addr: addr.ok_or(NodeError::MissingFlagValue { flag: "--addr" })?,
            root: root.ok_or(NodeError::MissingFlagValue { flag: "--root" })?,
        })
    }
}
