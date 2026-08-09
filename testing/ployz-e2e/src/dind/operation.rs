//! Shared operator, registry, deploy, and gateway fixtures for DinD scenarios.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use bollard::Docker;
use ployz::commands::SshTarget;
use ployz::init::ssh::{SshPeerKey, default_config_home};
use ployz::mesh::context::{OperatorContextStore, SSH_CONTEXT_HANDOFF_PREFIX, SshContextHandoff};
use ployz_core::corrosion::{MachineDocument, MachineTransport, SqliteValue};
use ployz_core::ids::{MachineRowId, OperationRowId};
use ployz_core::join::JoinBlob;
use ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT;
use ployz_core::{LensCollection, LensSnapshot, lens_route};

use super::{
    DindMachine, RELEASE_MANIFEST, artifact_dir, corrosion_access, corrosion_query,
    exec_in_container, exec_ok, install_local_release_channel, require,
};

const CLUSTER_NAME: &str = "dind-operation-deploy";
pub const FOUNDER_NAME: &str = "machine-one";
const WAIT_BUDGET: Duration = Duration::from_secs(60);
const WAIT_DELAY: Duration = Duration::from_millis(250);
const CLI_BUDGET: Duration = Duration::from_secs(180);
pub const REGISTRY_PORT: u16 = 5_000;
const OPERATION_ROW_ID_LABEL: &str = "plz.operation_row_id";
const DNS_QUERY_PROGRAM: &str = r#"
import ipaddress
import socket
import struct
import sys

resolver, name = sys.argv[1:]
labels = name.rstrip(".").split(".")
question = b"".join(bytes([len(label)]) + label.encode("ascii") for label in labels) + b"\0"
query = struct.pack("!HHHHHH", 0x8150, 0x0100, 1, 0, 0, 0) + question + struct.pack("!HH", 1, 1)
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.settimeout(3)
sock.sendto(query, (resolver, 53))
response = sock.recv(4096)
transaction_id, flags, questions, answers, _, _ = struct.unpack("!HHHHHH", response[:12])
if transaction_id != 0x8150 or not flags & 0x8000 or flags & 0x000f or questions != 1:
    raise SystemExit("unsuccessful DNS response")
offset = 12
while response[offset] != 0:
    offset += response[offset] + 1
offset += 5
addresses = []
for _ in range(answers):
    if response[offset] & 0xc0 == 0xc0:
        offset += 2
    else:
        while response[offset] != 0:
            offset += response[offset] + 1
        offset += 1
    kind, dns_class, _, length = struct.unpack("!HHIH", response[offset:offset + 10])
    offset += 10
    data = response[offset:offset + length]
    offset += length
    if kind == 1 and dns_class == 1 and length == 4:
        addresses.append(str(ipaddress.ip_address(data)))
for address in sorted(set(addresses), key=ipaddress.ip_address):
    print(address)
"#;

pub struct OperatorFixture {
    _temporary_home: tempfile::TempDir,
    home: PathBuf,
    cli: PathBuf,
    pub founder_target: SshTarget,
    pub founder_machine_id: MachineRowId,
    pub joiners: Vec<JoinedMachine>,
}

impl OperatorFixture {
    /// The operator context store home the shipped CLI reads under `$HOME`.
    #[must_use]
    pub fn config_home(&self) -> PathBuf {
        default_config_home(&self.home)
    }
}

/// One joined machine's operator-facing coordinates.
pub struct JoinedMachine {
    pub name: String,
    pub machine_id: MachineRowId,
    pub target: SshTarget,
    pub api_address: String,
    pub dns_address: Ipv4Addr,
}

pub async fn found_and_join(
    docker: &Docker,
    founder: &DindMachine,
    joiners: &[&DindMachine],
) -> Result<OperatorFixture, String> {
    found_and_join_with_service_urls(docker, founder, joiners, "disabled").await
}

pub async fn found_and_join_with_service_urls(
    docker: &Docker,
    founder: &DindMachine,
    joiners: &[&DindMachine],
    service_urls: &str,
) -> Result<OperatorFixture, String> {
    install_local_release_channel(docker, founder).await?;
    let temporary_home = tempfile::tempdir().map_err(|error| error.to_string())?;
    let home = temporary_home.path().to_path_buf();
    let config_home = default_config_home(&home);
    let founder_target: SshTarget = format!("root@{}", founder.bridge_ip).parse()?;
    let operator = SshPeerKey::generate("dind operation operator".to_owned())
        .map_err(|error| error.to_string())?;
    let store = OperatorContextStore::new(&config_home);
    store
        .persist_peer_new(&founder_target, &operator)
        .map_err(|error| error.to_string())?;

    let handoff = run_founding(docker, founder, &operator, service_urls).await?;
    store
        .persist(&founder_target, handoff.clone(), &operator)
        .map_err(|error| error.to_string())?;
    let cli = artifact_dir().join("ployz");
    for joiner in joiners {
        let token = create_join_token(&cli, &home, &founder_target)?;
        join_machine(docker, joiner, &token).await?;
    }

    let (corrosion_address, corrosion_token) = corrosion_access(docker, founder).await?;
    let roster = wait_for_roster(
        docker,
        founder,
        &corrosion_address,
        &corrosion_token,
        1 + joiners.len(),
    )
    .await?;
    let founder_machine_id = roster
        .iter()
        .find(|(_, document)| document.name.as_str() == FOUNDER_NAME)
        .map(|(id, _)| id.clone())
        .ok_or_else(|| "joined roster omitted the founder".to_owned())?;
    let mut joined = Vec::with_capacity(joiners.len());
    for joiner in joiners {
        let (machine_id, document) = roster
            .iter()
            .find(|(_, document)| document.name.as_str() == joiner.name)
            .ok_or_else(|| format!("joined roster omitted machine {}", joiner.name))?;
        let target: SshTarget = format!("root@{}", joiner.bridge_ip).parse()?;
        store
            .persist_peer_new(&target, &operator)
            .map_err(|error| error.to_string())?;
        store
            .persist(
                &target,
                SshContextHandoff {
                    cluster_id: handoff.cluster_id.clone(),
                    provider: handoff.provider,
                    machine_transport: document.transport.clone(),
                },
                &operator,
            )
            .map_err(|error| error.to_string())?;
        let (api_address, dns_address) = match &document.transport {
            MachineTransport::Wireguard {
                addr_v6, subnet_v4, ..
            } => (format!("[{addr_v6}]:2020"), subnet_v4.bridge_gateway_ipv4()),
            MachineTransport::Tailscale { .. } => {
                return Err("operation DinD proofs require builtin WireGuard".to_owned());
            }
        };
        wait_for_cli_output(
            &cli,
            &home,
            &["machine", "ls", "--target", target.as_str()],
            |output| {
                output.status.success()
                    && output
                        .stdout
                        .windows(FOUNDER_NAME.len())
                        .any(|window| window == FOUNDER_NAME.as_bytes())
            },
            "joined machine API through its persisted operator context",
        )?;
        joined.push(JoinedMachine {
            name: joiner.name.clone(),
            machine_id: machine_id.clone(),
            target,
            api_address,
            dns_address,
        });
    }

    Ok(OperatorFixture {
        _temporary_home: temporary_home,
        home,
        cli,
        founder_target,
        founder_machine_id,
        joiners: joined,
    })
}

pub async fn start_mutable_registry(
    docker: &Docker,
    founder: &DindMachine,
    joiners: &[&DindMachine],
) -> Result<String, String> {
    let IpAddr::V4(registry_ip) = founder.bridge_ip else {
        return Err("operation-deploy registry requires an IPv4 DinD bridge".to_owned());
    };
    let registry = format!("{registry_ip}:{REGISTRY_PORT}");
    for machine in std::iter::once(founder).chain(joiners.iter().copied()) {
        configure_insecure_registry(docker, machine, &registry).await?;
    }
    wait_for_inner_command(
        docker,
        founder,
        &["docker", "image", "inspect", "registry:2.8.3"],
        "preloaded registry image",
    )
    .await?;
    wait_for_inner_command(
        docker,
        founder,
        &["docker", "image", "inspect", "nginx:1.27-alpine"],
        "preloaded HTTP image",
    )
    .await?;
    exec_ok(
        docker,
        founder,
        &[
            "docker",
            "run",
            "--detach",
            "--name",
            "ployz-operation-registry",
            "--publish",
            &format!("{REGISTRY_PORT}:{REGISTRY_PORT}"),
            "registry:2.8.3",
        ],
    )
    .await?;
    for joiner in joiners {
        wait_for_registry(docker, joiner, &registry).await?;
    }

    let image = format!("{registry}/operation-http:latest");
    exec_ok(
        docker,
        founder,
        &[
            "docker",
            "create",
            "--name",
            "ployz-operation-http-source",
            "nginx:1.27-alpine",
        ],
    )
    .await?;
    exec_ok(
        docker,
        founder,
        &["docker", "commit", "ployz-operation-http-source", &image],
    )
    .await?;
    exec_ok(
        docker,
        founder,
        &["docker", "rm", "ployz-operation-http-source"],
    )
    .await?;
    exec_ok(docker, founder, &["docker", "push", &image]).await?;
    exec_ok(docker, founder, &["docker", "image", "rm", &image]).await?;
    Ok(image)
}

pub fn run_cli(operator: &OperatorFixture, args: &[&str]) -> Result<Output, String> {
    run_cli_bounded(&operator.cli, &operator.home, args)
}

pub fn create_namespace(
    operator: &OperatorFixture,
    namespace: &str,
    target: &SshTarget,
) -> Result<(), String> {
    let created = run_cli(
        operator,
        &[
            "namespace",
            "create",
            namespace,
            "--target",
            target.as_str(),
        ],
    )?;
    require_success(&created, "namespace create")
}

pub fn create_namespace_and_deploy(
    operator: &OperatorFixture,
    namespace: &str,
    service: &str,
    image: &str,
    secret_name: &str,
    secret_value: &str,
) -> Result<OperationRowId, String> {
    let founder_target = operator.founder_target.clone();
    create_namespace(operator, namespace, &founder_target)?;
    let environment = format!("{secret_name}={secret_value}");
    let deployed = run_cli(
        operator,
        &[
            "deploy",
            namespace,
            service,
            image,
            "--env",
            &environment,
            "--target",
            operator.founder_target.as_str(),
        ],
    )?;
    parse_deploy_operation(&deployed, "first deploy", secret_value)
}

pub fn parse_deploy_operation(
    output: &Output,
    description: &str,
    secret_value: &str,
) -> Result<OperationRowId, String> {
    require_success(output, description)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    require(
        !stdout.contains(secret_value)
            && !String::from_utf8_lossy(&output.stderr).contains(secret_value),
        format!("{description} output exposed the environment value"),
    )?;
    let operation_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("accepted operation "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| format!("{description} output omitted its operation id: {stdout}"))?;
    OperationRowId::try_new(operation_id).map_err(|error| error.to_string())
}

pub fn assert_cluster_wide_operation_terminal(
    operator: &OperatorFixture,
    operation_id: &OperationRowId,
) -> Result<(), String> {
    for target in std::iter::once(&operator.founder_target)
        .chain(operator.joiners.iter().map(|joined| &joined.target))
    {
        wait_for_cli_output(
            &operator.cli,
            &operator.home,
            &["ops", "list", "--target", target.as_str()],
            |output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).lines().any(|line| {
                        line.contains(operation_id.as_str()) && line.contains("completed")
                    })
            },
            &format!("terminal operation through {target}"),
        )?;
    }
    let Some(watch_via) = operator.joiners.first() else {
        return Err("cluster-wide operation proof requires a joined machine".to_owned());
    };
    let watched = run_cli_bounded(
        &operator.cli,
        &operator.home,
        &[
            "ops",
            "watch",
            operation_id.as_str(),
            "--target",
            watch_via.target.as_str(),
        ],
    )?;
    require_success(&watched, "operation watch through joined machine")?;
    let stdout = String::from_utf8_lossy(&watched.stdout);
    let terminal = format!("{operation_id} deploy completed");
    require(
        stdout.lines().any(|line| line == terminal),
        format!("operation watch omitted its terminal state: {stdout}"),
    )
}

pub async fn assert_first_revision_container_is_gone(
    docker: &Docker,
    machines: &[&DindMachine],
    first_operation: &OperationRowId,
) -> Result<(), String> {
    let filter = format!("label={OPERATION_ROW_ID_LABEL}={first_operation}");
    for machine in machines.iter().copied() {
        let listed = exec_ok(
            docker,
            machine,
            &["docker", "ps", "--all", "--quiet", "--filter", &filter],
        )
        .await?;
        require(
            listed.stdout.trim().is_empty(),
            format!(
                "first revision container survived on {}: {}",
                machine.name, listed.stdout
            ),
        )?;
    }
    Ok(())
}

pub async fn assert_dns_and_http(
    docker: &Docker,
    dns_client: &DindMachine,
    service_driver: &DindMachine,
    resolver: Ipv4Addr,
    hostname: &str,
    container_ip: Ipv4Addr,
    expected_body: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("DNS was not queried");
    let mut resolved = false;
    while Instant::now() < deadline {
        match query_dns(docker, dns_client, resolver, hostname).await {
            Ok(addresses) if addresses.contains(&container_ip) => {
                resolved = true;
                break;
            }
            Ok(addresses) => last = format!("resolved {addresses:?}"),
            Err(error) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    require(
        resolved,
        format!("{hostname} did not resolve to {container_ip}: {last}"),
    )?;
    let resolved = format!("{hostname}:80:{container_ip}");
    let url = format!("http://{hostname}/");
    let response = exec_ok(
        docker,
        service_driver,
        &[
            "curl",
            "--noproxy",
            "*",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "5",
            "--resolve",
            &resolved,
            &url,
        ],
    )
    .await?;
    require(
        response.stdout.contains(expected_body),
        format!("driver machine did not serve {expected_body:?}: {response:?}"),
    )
}

pub async fn push_second_revision(
    docker: &Docker,
    founder: &DindMachine,
    image: &str,
    body: &str,
) -> Result<(), String> {
    let write_body = format!("echo {body} > /usr/share/nginx/html/index.html");
    exec_ok(
        docker,
        founder,
        &[
            "docker",
            "run",
            "--name",
            "ployz-operation-http-second",
            "nginx:1.27-alpine",
            "sh",
            "-c",
            &write_body,
        ],
    )
    .await?;
    exec_ok(
        docker,
        founder,
        &[
            "docker",
            "commit",
            "--change",
            "CMD [\"nginx\", \"-g\", \"daemon off;\"]",
            "ployz-operation-http-second",
            image,
        ],
    )
    .await?;
    exec_ok(
        docker,
        founder,
        &["docker", "rm", "ployz-operation-http-second"],
    )
    .await?;
    exec_ok(docker, founder, &["docker", "push", image]).await?;
    exec_ok(docker, founder, &["docker", "image", "rm", image]).await?;
    Ok(())
}

pub async fn assert_gateway_http(
    docker: &Docker,
    gateway: &DindMachine,
    hostname: &str,
    expected_body: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("gateway was not queried");
    while Instant::now() < deadline {
        match fetch_gateway_http(docker, gateway, hostname).await {
            Ok(body) if body.contains(expected_body) => return Ok(()),
            Ok(body) => last = format!("served an unexpected successful body: {body:?}"),
            Err(error) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!(
        "gateway did not serve {expected_body:?} for {hostname}: {last}"
    ))
}

pub async fn gateway_status(
    docker: &Docker,
    gateway: &DindMachine,
    hostname: &str,
) -> Result<(u16, String), String> {
    let output = exec_ok(
        docker,
        gateway,
        &[
            "curl",
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--max-time",
            "5",
            "--header",
            &format!("Host: {hostname}"),
            "--write-out",
            "\n%{http_code}",
            "http://127.0.0.1/",
        ],
    )
    .await?;
    let Some((body, status)) = output.stdout.rsplit_once('\n') else {
        return Err(format!("gateway response omitted status: {output:?}"));
    };
    let status = status
        .trim()
        .parse::<u16>()
        .map_err(|error| format!("gateway returned invalid status {status:?}: {error}"))?;
    Ok((status, body.to_owned()))
}

pub async fn wait_for_gateway_status(
    docker: &Docker,
    gateway: &DindMachine,
    hostname: &str,
    expected_status: u16,
) -> Result<String, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("gateway was not queried");
    while Instant::now() < deadline {
        match gateway_status(docker, gateway, hostname).await {
            Ok((status, body)) if status == expected_status => return Ok(body),
            Ok((status, body)) => last = format!("HTTP {status}: {body}"),
            Err(error) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!(
        "gateway did not return HTTP {expected_status} for {hostname}: {last}"
    ))
}

pub async fn fetch_gateway_http(
    docker: &Docker,
    gateway: &DindMachine,
    hostname: &str,
) -> Result<String, String> {
    let (status, body) = gateway_status(docker, gateway, hostname).await?;
    require(
        (200..300).contains(&status),
        format!("gateway returned HTTP {status}: {body}"),
    )?;
    Ok(body)
}

pub async fn public_lens(
    docker: &Docker,
    requester: &DindMachine,
    api_address: &str,
    collection: LensCollection,
) -> Result<LensSnapshot, String> {
    let url = format!("http://{api_address}{}", lens_route(collection));
    let outcome = exec_ok(
        docker,
        requester,
        &[
            "curl",
            "--noproxy",
            "*",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "3",
            &url,
        ],
    )
    .await?;
    serde_json::from_str(outcome.stdout.trim())
        .map_err(|error| format!("public lens returned invalid JSON: {error}"))
}

pub fn require_success(output: &Output, operation: &str) -> Result<(), String> {
    require(
        output.status.success(),
        format!(
            "{operation} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

async fn run_founding(
    docker: &Docker,
    machine: &DindMachine,
    operator: &SshPeerKey,
    service_urls: &str,
) -> Result<SshContextHandoff, String> {
    let endpoint = SocketAddr::new(machine.bridge_ip, DEFAULT_WIREGUARD_LISTEN_PORT);
    let manifest = format!("file://{RELEASE_MANIFEST}");
    let command = [
        "env".to_owned(),
        format!("PLOYZ_RELEASE_MANIFEST_URL={manifest}"),
        "/opt/ployz/artifacts/ployz".to_owned(),
        "init".to_owned(),
        "--storage".to_owned(),
        "plain".to_owned(),
        "--container-network".to_owned(),
        "10.210.0.0/16".to_owned(),
        "--service-urls".to_owned(),
        service_urls.to_owned(),
        "--cluster-name".to_owned(),
        CLUSTER_NAME.to_owned(),
        "--machine-name".to_owned(),
        FOUNDER_NAME.to_owned(),
        "--wireguard-endpoint".to_owned(),
        endpoint.to_string(),
        "--driver-peer-id".to_owned(),
        operator.peer_id.to_string(),
        "--driver-peer-name".to_owned(),
        operator.peer_name.clone(),
        "--driver-peer-public-key".to_owned(),
        operator.public_key.as_str().to_owned(),
    ];
    let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
    let outcome = exec_in_container(docker, &machine.container_id, &refs)
        .await
        .map_err(|error| error.to_string())?;
    require(
        outcome.success(),
        format!("founding command failed: {outcome:?}"),
    )?;
    parse_handoff(&outcome.stdout)
}

fn parse_handoff(stdout: &str) -> Result<SshContextHandoff, String> {
    let mut handoffs = stdout
        .lines()
        .filter(|line| line.starts_with(SSH_CONTEXT_HANDOFF_PREFIX));
    let Some(line) = handoffs.next() else {
        return Err(format!("founding output omitted context handoff: {stdout}"));
    };
    require(
        handoffs.next().is_none(),
        "founding emitted more than one context handoff",
    )?;
    SshContextHandoff::decode_handoff(line).map_err(|error| error.to_string())
}

fn create_join_token(cli: &Path, home: &Path, target: &SshTarget) -> Result<JoinBlob, String> {
    let output = run_cli_bounded(
        cli,
        home,
        &[
            "token",
            "create",
            "--ttl",
            "1h",
            "--target",
            target.as_str(),
        ],
    )?;
    require_success(&output, "join token create")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let blob = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("JOIN_BLOB='")
                .and_then(|value| value.strip_suffix('\''))
        })
        .ok_or_else(|| format!("token output omitted JOIN_BLOB: {stdout}"))?;
    JoinBlob::try_parse(blob).map_err(|error| error.to_string())
}

async fn join_machine(
    docker: &Docker,
    machine: &DindMachine,
    blob: &JoinBlob,
) -> Result<(), String> {
    let endpoint = SocketAddr::new(machine.bridge_ip, DEFAULT_WIREGUARD_LISTEN_PORT);
    let outcome = exec_in_container(
        docker,
        &machine.container_id,
        &[
            "/opt/ployz/artifacts/ployz",
            "machine",
            "join",
            blob.expose(),
            "--storage",
            "plain",
            "--wireguard-endpoint",
            &endpoint.to_string(),
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(
        outcome.success() && outcome.stdout.contains("Joined machine"),
        format!("fresh machine join failed: {outcome:?}"),
    )?;
    wait_for_inner_command(
        docker,
        machine,
        &[
            "systemctl",
            "is-active",
            "ployz-corrosion.service",
            "ployzd-keeper.service",
            "ployzd-api.service",
            "ployzd-dns.service",
        ],
        "joined machine services",
    )
    .await
}

async fn wait_for_roster(
    docker: &Docker,
    machine: &DindMachine,
    address: &str,
    token: &str,
    minimum: usize,
) -> Result<BTreeMap<MachineRowId, MachineDocument>, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("roster was not queried");
    while Instant::now() < deadline {
        match corrosion_query(
            docker,
            machine,
            address,
            token,
            "SELECT id, document FROM machines",
        )
        .await
        {
            Ok(rows) => match parse_roster(rows) {
                Ok(roster) if roster.len() >= minimum && minimum < 3 => return Ok(roster),
                Ok(roster) if roster.len() >= minimum => {
                    match corrosion_query(
                        docker,
                        machine,
                        address,
                        token,
                        "SELECT COUNT(*) FROM __corro_members WHERE json_extract(foca_state, '$.state') = 'Alive'",
                    )
                    .await
                    {
                        Ok(rows)
                            if matches!(
                                rows.as_slice(),
                                [row] if matches!(row.as_slice(), [SqliteValue::Integer(count)] if *count > 0)
                            ) =>
                        {
                            return Ok(roster);
                        }
                        Ok(rows) => last = format!("Corrosion membership was not ready: {rows:?}"),
                        Err(error) => last = error,
                    }
                }
                Ok(roster) => last = format!("only {} roster rows", roster.len()),
                Err(error) => last = error,
            },
            Err(error) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("machine roster did not converge: {last}"))
}

fn parse_roster(
    rows: Vec<Vec<SqliteValue>>,
) -> Result<BTreeMap<MachineRowId, MachineDocument>, String> {
    let mut roster = BTreeMap::new();
    for row in rows {
        let [SqliteValue::Text(id), SqliteValue::Text(document)] = row.as_slice() else {
            return Err(format!("machine query returned an invalid row: {row:?}"));
        };
        let id = MachineRowId::try_new(id.clone()).map_err(|error| error.to_string())?;
        let document = serde_json::from_str(document)
            .map_err(|error| format!("machine row was invalid: {error}"))?;
        roster.insert(id, document);
    }
    Ok(roster)
}

async fn query_dns(
    docker: &Docker,
    dns_client: &DindMachine,
    resolver: Ipv4Addr,
    hostname: &str,
) -> Result<Vec<Ipv4Addr>, String> {
    let outcome = exec_in_container(
        docker,
        &dns_client.container_id,
        &[
            "python3",
            "-c",
            DNS_QUERY_PROGRAM,
            &resolver.to_string(),
            hostname,
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(outcome.success(), format!("DNS query failed: {outcome:?}"))?;
    outcome
        .stdout
        .lines()
        .map(|line| {
            line.trim()
                .parse::<Ipv4Addr>()
                .map_err(|error| format!("invalid DNS answer {line:?}: {error}"))
        })
        .collect()
}

async fn configure_insecure_registry(
    docker: &Docker,
    machine: &DindMachine,
    registry: &str,
) -> Result<(), String> {
    let program = "import json,sys; p='/etc/docker/daemon.json'; d=json.load(open(p)); r=d.setdefault('insecure-registries', []); x=sys.argv[1]; r.append(x) if x not in r else None; open(p, 'w').write(json.dumps(d, indent=2)+'\\n')";
    exec_ok(docker, machine, &["python3", "-c", program, registry]).await?;
    exec_ok(docker, machine, &["systemctl", "restart", "docker.service"]).await?;
    wait_for_inner_command(
        docker,
        machine,
        &["docker", "info"],
        "Docker after registry configuration",
    )
    .await
}

async fn wait_for_registry(
    docker: &Docker,
    machine: &DindMachine,
    registry: &str,
) -> Result<(), String> {
    let url = format!("http://{registry}/v2/");
    wait_for_inner_command(
        docker,
        machine,
        &[
            "curl",
            "--noproxy",
            "*",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "3",
            &url,
        ],
        "run-scoped registry from the joined machine",
    )
    .await
}

async fn wait_for_inner_command(
    docker: &Docker,
    machine: &DindMachine,
    command: &[&str],
    description: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("command was not attempted");
    while Instant::now() < deadline {
        match exec_in_container(docker, &machine.container_id, command).await {
            Ok(outcome) if outcome.success() => return Ok(()),
            Ok(outcome) => last = format!("{outcome:?}"),
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("timed out waiting for {description}: {last}"))
}

fn wait_for_cli_output(
    cli: &Path,
    home: &Path,
    args: &[&str],
    predicate: impl Fn(&Output) -> bool,
    description: &str,
) -> Result<Output, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("CLI was not run");
    while Instant::now() < deadline {
        match run_cli_bounded(cli, home, args) {
            Ok(output) if predicate(&output) => return Ok(output),
            Ok(output) => {
                last = format!(
                    "status={} stdout={} stderr={}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(error) => last = error,
        }
        thread::sleep(WAIT_DELAY);
    }
    Err(format!("timed out waiting for {description}: {last}"))
}

fn run_cli_bounded(cli: &Path, home: &Path, args: &[&str]) -> Result<Output, String> {
    let child = spawn_cli(cli, home, args)?;
    wait_for_child(child, CLI_BUDGET)
}

fn spawn_cli(cli: &Path, home: &Path, args: &[&str]) -> Result<Child, String> {
    Command::new(cli)
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run shipped CLI {}: {error}", cli.display()))
}

fn wait_for_child(mut child: Child, budget: Duration) -> Result<Output, String> {
    let deadline = Instant::now() + budget;
    loop {
        if child
            .try_wait()
            .map_err(|error| format!("could not poll shipped CLI: {error}"))?
            .is_some()
        {
            return child
                .wait_with_output()
                .map_err(|error| format!("could not collect shipped CLI output: {error}"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .map_err(|error| format!("could not collect timed-out CLI output: {error}"))?;
            return Err(format!(
                "shipped CLI exceeded {budget:?}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}
