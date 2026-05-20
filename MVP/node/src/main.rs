use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use mvp_bus::IslandId;
use mvp_node::{
    AcmeIssueOptions, DaemonOptions, InitOptions, NodeError, NodeResult, ProductDeployOptions,
    ServingRoleOptions, admit_joiner, create_admission_request, create_invite,
    deploy_product_service, deploy_product_service_with_runtime, init_node,
    issue_product_certificate, join_from_token, load_node, now_ms, read_product_deploy_status,
    run_daemon_once, run_dns_role, run_gateway_role,
};
use mvp_projection::{
    DnsProjection, GatewayProjection, ProjectionState, write_projection_snapshots,
    write_serving_generation_manifest,
};
use serde::Serialize;

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
        "bootstrap" => bootstrap(rest),
        "status" => status(rest),
        "invite" => invite(rest),
        "join" => join(rest),
        "admission" => admission(rest),
        "admit" => admit(rest),
        "daemon" => daemon(rest),
        "daemon-status" => daemon_status(rest),
        "deploy" => deploy(rest),
        "deploy-status" => deploy_status(rest),
        "acme-issue" => acme_issue(rest),
        "runtime-http" => runtime_http(rest),
        "gateway" => gateway(rest),
        "dns" => dns(rest),
        "--help" | "-h" | "help" => Ok(help()),
        other => Err(NodeError::UnsupportedCommand {
            command: other.to_string(),
        }),
    }
}

fn deploy(args: &[String]) -> NodeResult<String> {
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

fn deploy_status(args: &[String]) -> NodeResult<String> {
    let parsed = DeployStatusArgs::parse(args)?;
    let report = read_product_deploy_status(parsed.state_dir, parsed.deploy_id)?;
    serde_json::to_string(&DeployStatusResponse::from(report))
        .map_err(|source| NodeError::EncodeNodeAgentRpc { source })
}

fn acme_issue(args: &[String]) -> NodeResult<String> {
    let parsed = AcmeIssueArgs::parse(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    let report = runtime.block_on(issue_product_certificate(parsed.into_options()))?;
    serde_json::to_string(&report).map_err(|source| NodeError::EncodeNodeAgentRpc { source })
}

fn gateway(args: &[String]) -> NodeResult<String> {
    let parsed = ServingRoleArgs::parse(args, ServingRoleCommand::Gateway)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    runtime.block_on(run_gateway_role(parsed.into_options()))?;
    Ok("gateway stopped".to_string())
}

fn dns(args: &[String]) -> NodeResult<String> {
    let parsed = ServingRoleArgs::parse(args, ServingRoleCommand::Dns)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    runtime.block_on(run_dns_role(parsed.into_options()))?;
    Ok("dns stopped".to_string())
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
        .enable_io()
        .build()
        .map_err(|source| NodeError::Runtime { source })?;
    let mut options = DaemonOptions::new(Duration::from_millis(parsed.run_for_ms.unwrap_or(1_000)));
    if let Some(control_socket) = parsed.control_socket {
        options = options.with_control_socket(control_socket);
    }
    if let Some(ifname) = parsed.linux_wireguard_ifname {
        options = options.with_linux_wireguard(ifname);
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

fn daemon_status(args: &[String]) -> NodeResult<String> {
    let control_socket = parse_control_socket_only(args)?;
    daemon_control_request(control_socket, b"status\n").map(|response| response.trim().to_string())
}

fn daemon_control_request(control_socket: PathBuf, request: &[u8]) -> NodeResult<String> {
    let mut stream =
        UnixStream::connect(&control_socket).map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket.clone(),
            operation: "connect",
            source,
        })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket.clone(),
            operation: "set status read timeout",
            source,
        })?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket.clone(),
            operation: "set status write timeout",
            source,
        })?;
    stream
        .write_all(request)
        .map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket.clone(),
            operation: "write status request",
            source,
        })?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket.clone(),
            operation: "finish status request",
            source,
        })?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket,
            operation: "read status response",
            source,
        })?;
    Ok(response)
}

fn parse_control_socket_only(args: &[String]) -> NodeResult<PathBuf> {
    let mut control_socket = None;
    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--control" => {
                let Some(value) = remaining.next() else {
                    return Err(NodeError::MissingFlagValue { flag: "--control" });
                };
                control_socket = Some(PathBuf::from(value));
            }
            other => {
                return Err(NodeError::UnknownArgument {
                    argument: other.to_string(),
                });
            }
        }
    }
    control_socket.ok_or(NodeError::MissingFlagValue { flag: "--control" })
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

fn bootstrap(args: &[String]) -> NodeResult<String> {
    let parsed = ParsedArgs::parse(args)?;
    let Some(state_dir) = parsed.state_dir else {
        return Err(NodeError::MissingFlagValue { flag: "--state" });
    };
    let state = match load_node(&state_dir) {
        Ok(state) => {
            if let Some(island) = parsed.island.as_deref()
                && state.island_id() != island
            {
                return Err(NodeError::BootstrapConflict {
                    field: "island",
                    requested: island.to_string(),
                    existing: state.island_id().to_string(),
                });
            }
            if let Some(node_id) = parsed.node_id.as_deref()
                && state.node_id_str() != node_id
            {
                return Err(NodeError::BootstrapConflict {
                    field: "node_id",
                    requested: node_id.to_string(),
                    existing: state.node_id_str().to_string(),
                });
            }
            state
        }
        Err(NodeError::NotInitialized { .. }) => {
            let mut options = InitOptions::new(&state_dir);
            if let Some(island) = parsed.island {
                options = options.with_island(island);
            }
            if let Some(node_id) = parsed.node_id {
                options = options.with_node_id(node_id);
            }
            init_node(options)?
        }
        Err(error) => return Err(error),
    };
    ensure_bootstrap_dirs(state.paths())?;
    ensure_bootstrap_snapshots(&state)?;
    let response = BootstrapResponse::from_state(&state);
    serde_json::to_string(&response).map_err(|source| NodeError::EncodeNodeAgentRpc { source })
}

fn ensure_bootstrap_dirs(paths: &mvp_node::NodePaths) -> NodeResult<()> {
    for path in [
        paths.runtime_dir.clone(),
        paths.wireguard_dir.clone(),
        paths.state_dir.join("control"),
        paths.state_dir.join("acme"),
    ] {
        fs::create_dir_all(&path).map_err(|source| NodeError::CreateStateDir { path, source })?;
    }
    Ok(())
}

fn ensure_bootstrap_snapshots(state: &mvp_node::LoadedNodeState) -> NodeResult<()> {
    if state.paths().gateway_snapshot.exists() && state.paths().dns_snapshot.exists() {
        write_serving_generation_manifest(
            &state.paths().gateway_snapshot,
            &state.paths().dns_snapshot,
        )
        .map_err(|source| NodeError::Projection { source })?;
        return Ok(());
    }
    let mut projection = ProjectionState::for_island(IslandId::new(state.island_id().to_string()));
    projection.gateway = Some(GatewayProjection {
        gateway_commit_id: "bootstrap-gateway-empty".to_string(),
        route_commit_id: "bootstrap-route-empty".to_string(),
        routes: Vec::new(),
    });
    projection.dns = Some(DnsProjection {
        dns_commit_id: "bootstrap-dns-empty".to_string(),
        records: Vec::new(),
    });
    write_projection_snapshots(
        &projection,
        &state.paths().gateway_snapshot,
        &state.paths().dns_snapshot,
    )
    .map_err(|source| NodeError::Projection { source })?;
    Ok(())
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

#[derive(Serialize)]
struct BootstrapResponse {
    status: &'static str,
    node_id: String,
    island: String,
    principal: String,
    paths: BootstrapPaths,
    identity: BootstrapIdentity,
    role_defaults: BootstrapRoleDefaults,
}

#[derive(Serialize)]
struct BootstrapPaths {
    state_dir: PathBuf,
    fact_store: PathBuf,
    projection_db: PathBuf,
    gateway_snapshot: PathBuf,
    dns_snapshot: PathBuf,
    runtime_dir: PathBuf,
    wireguard_dir: PathBuf,
    wireguard_private_key: PathBuf,
    acme_accounts: PathBuf,
}

#[derive(Serialize)]
struct BootstrapIdentity {
    p2panda_network_id_hex: String,
    p2panda_topic_hex: String,
    wireguard_public_key: String,
    wireguard_overlay_ip: String,
    container_subnet: String,
}

#[derive(Serialize)]
struct BootstrapRoleDefaults {
    daemon_control_socket: PathBuf,
    gateway_control_socket: PathBuf,
    dns_control_socket: PathBuf,
}

impl BootstrapResponse {
    fn from_state(state: &mvp_node::LoadedNodeState) -> Self {
        let paths = state.paths();
        Self {
            status: "bootstrapped",
            node_id: state.node_id_str().to_string(),
            island: state.island_id().to_string(),
            principal: state.principal_id().to_string(),
            paths: BootstrapPaths {
                state_dir: paths.state_dir.clone(),
                fact_store: paths.fact_store.clone(),
                projection_db: paths.projection_db.clone(),
                gateway_snapshot: paths.gateway_snapshot.clone(),
                dns_snapshot: paths.dns_snapshot.clone(),
                runtime_dir: paths.runtime_dir.clone(),
                wireguard_dir: paths.wireguard_dir.clone(),
                wireguard_private_key: paths.wireguard_private_key.clone(),
                acme_accounts: paths.state_dir.join("acme-accounts.json"),
            },
            identity: BootstrapIdentity {
                p2panda_network_id_hex: state.p2panda_network_id_hex().to_string(),
                p2panda_topic_hex: state.p2panda_topic_hex().to_string(),
                wireguard_public_key: state.wireguard_public_key().to_string(),
                wireguard_overlay_ip: state.wireguard_overlay_ip().to_string(),
                container_subnet: state.container_subnet().to_string(),
            },
            role_defaults: BootstrapRoleDefaults {
                daemon_control_socket: role_socket(paths.state_dir.as_path(), "daemon"),
                gateway_control_socket: role_socket(paths.state_dir.as_path(), "gateway"),
                dns_control_socket: role_socket(paths.state_dir.as_path(), "dns"),
            },
        }
    }
}

fn role_socket(state_dir: &Path, role: &str) -> PathBuf {
    state_dir.join("control").join(format!("{role}.sock"))
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
    control_socket: Option<PathBuf>,
    linux_wireguard_ifname: Option<String>,
}

struct AdmitArgs {
    state_dir: PathBuf,
    request: String,
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

#[derive(Clone)]
struct DeployArgs {
    state_dir: Option<PathBuf>,
    control_socket: Option<PathBuf>,
    deploy_id: String,
    target_node: String,
    service: String,
    revision: String,
    hostname: String,
    runtime: DeployRuntimeArgs,
}

#[derive(Clone)]
enum DeployRuntimeArgs {
    Process,
    Docker {
        image: String,
        service_port: u16,
        command: Option<Vec<String>>,
    },
}

struct DeployStatusArgs {
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

impl DeployArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
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
                    docker_command = Some(
                        value
                            .split_whitespace()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>(),
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

    fn runtime_backend(&self) -> NodeResult<Option<Arc<dyn mvp_runtime::RuntimeBackend>>> {
        match &self.runtime {
            DeployRuntimeArgs::Process => Ok(None),
            DeployRuntimeArgs::Docker {
                image,
                service_port,
                command,
            } => docker_runtime_backend(self, image, *service_port, command.as_deref()),
        }
    }
}

#[cfg(feature = "docker-runtime")]
fn docker_runtime_backend(
    args: &DeployArgs,
    image: &str,
    service_port: u16,
    command: Option<&[String]>,
) -> NodeResult<Option<Arc<dyn mvp_runtime::RuntimeBackend>>> {
    let state_dir = args
        .state_dir
        .as_ref()
        .ok_or(NodeError::MissingFlagValue { flag: "--state" })?;
    let state = load_node(state_dir)?;
    let mut config = mvp_runtime::DockerRuntimeConfig::new(
        state.node_id(),
        state.paths().runtime_dir.clone(),
        image,
    )
    .with_service_port(service_port)
    .with_dns_server(state.container_subnet().docker_gateway_ip().to_string());
    if let Some(command) = command {
        config = config.with_command(command.iter().cloned());
    }
    let network =
        mvp_runtime::DockerBridgeNetwork::connect(mvp_runtime::DockerBridgeNetworkConfig::new(
            format!("ployz-mvp-{}", state.node_id_str()),
            state.container_subnet(),
        ))
        .map_err(|source| NodeError::RuntimeBackend { source })?;
    let runtime =
        mvp_runtime::DockerRuntime::connect_with_container_network(config, Arc::new(network))
            .map_err(|source| NodeError::RuntimeBackend { source })?;
    Ok(Some(Arc::new(runtime)))
}

#[cfg(not(feature = "docker-runtime"))]
fn docker_runtime_backend(
    _args: &DeployArgs,
    _image: &str,
    _service_port: u16,
    _command: Option<&[String]>,
) -> NodeResult<Option<Arc<dyn mvp_runtime::RuntimeBackend>>> {
    Err(NodeError::CommandNotWired {
        command: "deploy --runtime docker requires the docker-runtime feature".to_string(),
    })
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
        let mut control_socket = None;
        let mut linux_wireguard_ifname = None;
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
        "  bootstrap --state <dir> [--island <id>] [--node-id <id>]",
        "  status --state <dir>",
        "  daemon --state <dir> [--run-for-ms <ms>] [--linux-wireguard-ifname <ifname>]",
        "  invite --state <dir> [--ttl-ms <ms>]",
        "  join --state <dir> --token <json> [--node-id <id>]",
        "  admission --state <dir>",
        "  admit --state <dir> --request <json>",
        "  deploy (--state <dir> | --control <socket>) --target-node <id> [--deploy-id <id>] [--service <name>] [--revision <rev>] [--hostname <name>] [--runtime process|docker --image <ref> [--service-port <port>] [--container-command <cmd>]",
        "  deploy-status --state <dir> [--deploy-id <id>]",
        "  acme-issue --state <dir> --hostname <host> --gateway <url> [--gateway-control <socket>] [--issuer-holder <id>] [--account-path <path>]",
        "  gateway --state <dir> --listen <addr> --control <socket> [--tls-listen <addr>] [--stale-after-ms <ms>]",
        "  dns --state <dir> --listen <addr> --control <socket> [--stale-after-ms <ms>]",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{ParsedArgs, run};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

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
    fn bootstrap_is_idempotent_and_reports_product_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("node-a");

        let first = run(vec![
            "bootstrap".to_string(),
            "--state".to_string(),
            state.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("bootstrap");
        let second = run(vec![
            "bootstrap".to_string(),
            "--state".to_string(),
            state.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("bootstrap again");
        let first: serde_json::Value = serde_json::from_str(&first).expect("first json");
        let second: serde_json::Value = serde_json::from_str(&second).expect("second json");

        assert_eq!(first["status"], "bootstrapped");
        assert_eq!(first["node_id"], "node-a");
        assert_eq!(first["island"], "prod");
        assert_eq!(first["identity"], second["identity"]);
        assert!(state.join("runtime").is_dir());
        assert!(state.join("control").is_dir());
        assert!(state.join("wireguard").join("private.key").exists());
        assert_eq!(
            first["role_defaults"]["gateway_control_socket"],
            serde_json::Value::String(
                state
                    .join("control")
                    .join("gateway.sock")
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn bootstrap_refuses_conflicting_existing_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("node-a");
        run(vec![
            "bootstrap".to_string(),
            "--state".to_string(),
            state.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("bootstrap");

        let error = run(vec![
            "bootstrap".to_string(),
            "--state".to_string(),
            state.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-b".to_string(),
        ])
        .expect_err("conflicting node id fails");

        assert!(error.to_string().contains("existing node has 'node-a'"));
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

    #[test]
    fn deploy_control_rejects_non_deploy_daemon_response() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket = temp.path().join("daemon.sock");
        let listener = UnixListener::bind(socket.as_path()).expect("bind listener");
        let server = thread::spawn(move || {
            let (mut stream, _addr) = listener.accept().expect("accept");
            let mut request = Vec::new();
            stream.read_to_end(&mut request).expect("read request");
            assert!(!request.is_empty());
            stream
                .write_all(br#"{"status":"ready","node":"founder"}"#)
                .expect("write response");
        });

        let error = run(vec![
            "deploy".to_string(),
            "--control".to_string(),
            socket.display().to_string(),
            "--target-node".to_string(),
            "peer-a".to_string(),
        ])
        .expect_err("ready is not deployed");
        server.join().expect("server thread");

        assert!(matches!(
            error,
            mvp_node::NodeError::NodeAgentRpc { message }
                if message.contains("unexpected status 'ready'")
        ));
    }
}
