use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use mvp_node::{
    DaemonOptions, InitOptions, NodeError, NodeResult, ProductDeployOptions, admit_joiner,
    create_admission_request, create_invite, deploy_product_service, init_node, join_from_token,
    load_node, now_ms, run_daemon_once,
};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run(args: Vec<String>) -> NodeResult<String> {
    let [command, rest @ ..] = args.as_slice() else {
        return Ok(help());
    };
    match command.as_str() {
        "init" => init(rest),
        "status" => status(rest),
        "invite" => invite(rest),
        "join" => join(rest),
        "admission" => admission(rest),
        "admit" => admit(rest),
        "daemon" => daemon(rest),
        "deploy" => deploy(rest),
        "runtime-http" => runtime_http(rest),
        "gateway" | "dns" => Err(NodeError::CommandNotWired {
            command: command.clone(),
        }),
        "--help" | "-h" | "help" => Ok(help()),
        other => Err(NodeError::UnsupportedCommand {
            command: other.to_string(),
        }),
    }
}

fn deploy(args: &[String]) -> NodeResult<String> {
    let parsed = DeployArgs::parse(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    let report = runtime.block_on(deploy_product_service(parsed.into_options()))?;
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

fn runtime_http(args: &[String]) -> NodeResult<String> {
    let parsed = RuntimeHttpArgs::parse(args)?;
    mvp_runtime::run_static_http_server(&parsed.addr, &parsed.root)
        .map_err(|source| NodeError::RuntimeBackend { source })?;
    Ok("runtime-http stopped".to_string())
}

fn admission(args: &[String]) -> NodeResult<String> {
    let state_dir = parse_state_dir_only(args)?;
    create_admission_request(state_dir)
}

fn admit(args: &[String]) -> NodeResult<String> {
    let parsed = AdmitArgs::parse(args)?;
    let report = admit_joiner(parsed.state_dir, &parsed.request, now_ms())?;
    Ok(format!(
        "admitted node={} principal={}",
        report.node_id, report.principal_id
    ))
}

fn invite(args: &[String]) -> NodeResult<String> {
    let parsed = InviteArgs::parse(args)?;
    let token = create_invite(
        parsed.state_dir,
        Duration::from_millis(parsed.ttl_ms.unwrap_or(600_000)),
    )?;
    Ok(token)
}

fn join(args: &[String]) -> NodeResult<String> {
    let parsed = JoinArgs::parse(args)?;
    let state = join_from_token(parsed.state_dir, &parsed.token, parsed.node_id, now_ms())?;
    Ok(format!(
        "joined node={} island={} state={}",
        state.node_id_str(),
        state.island_id(),
        state.paths().state_dir.display()
    ))
}

fn daemon(args: &[String]) -> NodeResult<String> {
    let parsed = DaemonArgs::parse(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    let report = runtime.block_on(run_daemon_once(
        parsed.state_dir,
        DaemonOptions::new(Duration::from_millis(parsed.run_for_ms.unwrap_or(1_000))),
    ))?;
    Ok(format!(
        "daemon node={} ticket={} imported_batches={} imported_operations={} node_agent_handlers={}",
        report.node_id,
        report.ticket,
        report.imported_batches,
        report.imported_operations,
        report.node_agent_handlers
    ))
}

fn init(args: &[String]) -> NodeResult<String> {
    let parsed = ParsedArgs::parse(args)?;
    let Some(state_dir) = parsed.state_dir else {
        return Err(NodeError::MissingFlagValue { flag: "--state" });
    };
    let mut options = InitOptions::new(state_dir);
    if let Some(island) = parsed.island {
        options = options.with_island(island);
    }
    if let Some(node_id) = parsed.node_id {
        options = options.with_node_id(node_id);
    }
    let state = init_node(options)?;
    Ok(format!(
        "initialized node={} island={} state={}",
        state.node_id_str(),
        state.island_id(),
        state.paths().state_dir.display()
    ))
}

fn status(args: &[String]) -> NodeResult<String> {
    let state_dir = parse_state_dir_only(args)?;
    let state = load_node(state_dir)?;
    Ok(format!(
        "node={} island={} principal={} facts={} projection={} gateway_snapshot={} dns_snapshot={}",
        state.node_id_str(),
        state.island_id(),
        state.principal_id(),
        state.paths().fact_store.display(),
        state.paths().projection_db.display(),
        state.paths().gateway_snapshot.display(),
        state.paths().dns_snapshot.display()
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedArgs {
    state_dir: Option<PathBuf>,
    island: Option<String>,
    node_id: Option<String>,
}

struct InviteArgs {
    state_dir: PathBuf,
    ttl_ms: Option<u64>,
}

impl InviteArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut ttl_ms = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    state_dir = Some(PathBuf::from(value));
                }
                "--ttl-ms" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--ttl-ms" });
                    };
                    ttl_ms =
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
            ttl_ms,
        })
    }
}

struct JoinArgs {
    state_dir: PathBuf,
    token: String,
    node_id: Option<String>,
}

impl JoinArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut token = None;
        let mut node_id = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    state_dir = Some(PathBuf::from(value));
                }
                "--token" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--token" });
                    };
                    token = Some(value.clone());
                }
                "--node-id" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--node-id" });
                    };
                    node_id = Some(value.clone());
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
            token: token.ok_or(NodeError::MissingFlagValue { flag: "--token" })?,
            node_id,
        })
    }
}

struct DaemonArgs {
    state_dir: PathBuf,
    run_for_ms: Option<u64>,
}

struct AdmitArgs {
    state_dir: PathBuf,
    request: String,
}

struct RuntimeHttpArgs {
    addr: String,
    root: PathBuf,
}

struct DeployArgs {
    state_dir: PathBuf,
    deploy_id: String,
    target_node: String,
    service: String,
    revision: String,
    hostname: String,
}

impl DeployArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut deploy_id = None;
        let mut target_node = None;
        let mut service = None;
        let mut revision = None;
        let mut hostname = None;
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
            target_node: target_node.ok_or(NodeError::MissingFlagValue {
                flag: "--target-node",
            })?,
            service: service.unwrap_or_else(|| "web".to_string()),
            revision: revision.unwrap_or_else(|| "rev-1".to_string()),
            hostname: hostname.unwrap_or_else(|| "web.example.test".to_string()),
        })
    }

    fn into_options(self) -> ProductDeployOptions {
        ProductDeployOptions::new(self.state_dir)
            .with_deploy_id(self.deploy_id)
            .with_target_node(self.target_node)
            .with_service(self.service)
            .with_revision(self.revision)
            .with_hostname(self.hostname)
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

impl AdmitArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut request = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    state_dir = Some(PathBuf::from(value));
                }
                "--request" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--request" });
                    };
                    request = Some(value.clone());
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
            request: request.ok_or(NodeError::MissingFlagValue { flag: "--request" })?,
        })
    }
}

impl DaemonArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut run_for_ms = None;
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
        })
    }
}

impl ParsedArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut parsed = Self {
            state_dir: None,
            island: None,
            node_id: None,
        };
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    parsed.state_dir = Some(PathBuf::from(value));
                }
                "--island" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--island" });
                    };
                    parsed.island = Some(value.clone());
                }
                "--node-id" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--node-id" });
                    };
                    parsed.node_id = Some(value.clone());
                }
                other => {
                    return Err(NodeError::UnknownArgument {
                        argument: other.to_string(),
                    });
                }
            }
        }
        Ok(parsed)
    }
}

fn parse_state_dir_only(args: &[String]) -> NodeResult<PathBuf> {
    let mut state_dir = None;
    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--state" => {
                let Some(value) = remaining.next() else {
                    return Err(NodeError::MissingFlagValue { flag: "--state" });
                };
                state_dir = Some(PathBuf::from(value));
            }
            other => {
                return Err(NodeError::UnknownArgument {
                    argument: other.to_string(),
                });
            }
        }
    }
    state_dir.ok_or(NodeError::MissingFlagValue { flag: "--state" })
}

fn help() -> String {
    [
        "mvp-node <command> [options]",
        "",
        "Commands:",
        "  init --state <dir> [--island <id>] [--node-id <id>]",
        "  status --state <dir>",
        "  daemon --state <dir> [--run-for-ms <ms>]",
        "  invite --state <dir> [--ttl-ms <ms>]",
        "  join --state <dir> --token <json> [--node-id <id>]",
        "  admission --state <dir>",
        "  admit --state <dir> --request <json>",
        "  deploy --state <dir> --target-node <id> [--deploy-id <id>] [--service <name>] [--revision <rev>] [--hostname <name>]",
        "  gateway|dns  (planned product-vertical commands)",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{ParsedArgs, run};

    #[test]
    fn init_and_status_round_trip_through_cli_surface() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("node-a");

        let init = run(vec![
            "init".to_string(),
            "--state".to_string(),
            state.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("init");
        let status = run(vec![
            "status".to_string(),
            "--state".to_string(),
            state.display().to_string(),
        ])
        .expect("status");

        assert!(init.contains("initialized node=node-a island=prod"));
        assert!(status.contains("node=node-a island=prod principal=node:node-a"));
    }

    #[test]
    fn invite_join_admission_and_admit_round_trip_through_cli_surface() {
        let temp = tempfile::tempdir().expect("tempdir");
        let node_a = temp.path().join("node-a");
        let node_b = temp.path().join("node-b");
        run(vec![
            "init".to_string(),
            "--state".to_string(),
            node_a.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("init node a");

        let invite = run(vec![
            "invite".to_string(),
            "--state".to_string(),
            node_a.display().to_string(),
        ])
        .expect("invite");
        let joined = run(vec![
            "join".to_string(),
            "--state".to_string(),
            node_b.display().to_string(),
            "--token".to_string(),
            invite,
            "--node-id".to_string(),
            "node-b".to_string(),
        ])
        .expect("join node b");
        let admission = run(vec![
            "admission".to_string(),
            "--state".to_string(),
            node_b.display().to_string(),
        ])
        .expect("admission");
        let admitted = run(vec![
            "admit".to_string(),
            "--state".to_string(),
            node_a.display().to_string(),
            "--request".to_string(),
            admission,
        ])
        .expect("admit node b");

        assert!(joined.contains("joined node=node-b island=prod"));
        assert!(admitted.contains("admitted node=node-b principal=node:node-b"));
    }

    #[test]
    fn parses_state_island_and_node_id_flags() {
        let parsed = ParsedArgs::parse(&[
            "--state".to_string(),
            "/tmp/node".to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("parse args");

        assert_eq!(
            parsed.state_dir.expect("state").display().to_string(),
            "/tmp/node"
        );
        assert_eq!(parsed.island.as_deref(), Some("prod"));
        assert_eq!(parsed.node_id.as_deref(), Some("node-a"));
    }

    #[test]
    fn status_rejects_init_only_flags() {
        let error = run(vec![
            "status".to_string(),
            "--state".to_string(),
            "/tmp/node".to_string(),
            "--island".to_string(),
            "prod".to_string(),
        ])
        .expect_err("status rejects island");

        assert!(matches!(
            error,
            mvp_node::NodeError::UnknownArgument { argument } if argument == "--island"
        ));
    }
}
