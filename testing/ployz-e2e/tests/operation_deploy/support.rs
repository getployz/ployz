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
use ployz_core::corrosion::{
    MachineDocument, MachineTransport, ServiceDocument, Sha256Hex, SqliteValue,
};
use ployz_core::ids::{MachineRowId, OperationRowId};
use ployz_core::join::JoinBlob;
use ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT;
use ployz_core::{LensCollection, LensSnapshot, lens_route};
use ployz_e2e::dind::{
    DindMachine, RELEASE_MANIFEST, artifact_dir, corrosion_access, corrosion_query,
    exec_in_container, exec_ok, install_local_release_channel, require,
};

const CLUSTER_NAME: &str = "dind-operation-deploy";
pub(super) const FOUNDER_NAME: &str = "machine-one";
const WAIT_BUDGET: Duration = Duration::from_secs(60);
const WAIT_DELAY: Duration = Duration::from_millis(250);
const CLI_BUDGET: Duration = Duration::from_secs(180);
pub(super) const REGISTRY_PORT: u16 = 5_000;
/// Docker label carrying the owning operation row id on every v2-managed
/// container; mirrors the deploy engine's v2 label codec.
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

pub(super) struct OperatorFixture {
    _temporary_home: tempfile::TempDir,
    home: PathBuf,
    cli: PathBuf,
    pub(super) founder_target: SshTarget,
    pub(super) founder_machine_id: MachineRowId,
    pub(super) joiners: Vec<JoinedMachine>,
}

impl OperatorFixture {
    /// The operator context store home the shipped CLI reads under `$HOME`.
    pub(super) fn config_home(&self) -> PathBuf {
        default_config_home(&self.home)
    }
}

/// One joined machine's operator-facing coordinates.
pub(super) struct JoinedMachine {
    /// The machine's ployz roster name (its probed hostname).
    pub(super) name: String,
    pub(super) machine_id: MachineRowId,
    pub(super) target: SshTarget,
    pub(super) api_address: String,
    pub(super) dns_address: Ipv4Addr,
}

pub(super) struct PublicDeployRows {
    service: ServiceDocument,
    pub(super) container_ip: Ipv4Addr,
    service_container_deploys: Vec<OperationRowId>,
    encoded: String,
}

/// The two distinguishable HTTP bodies observed across a blue/green cutover.
pub(super) struct RevisionBodies<'a> {
    pub(super) first: &'a str,
    pub(super) second: &'a str,
}

pub(super) async fn found_and_join(
    docker: &Docker,
    founder: &DindMachine,
    joiners: &[&DindMachine],
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

    let handoff = run_founding(docker, founder, &operator).await?;
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
        // A joined machine's roster name is its probed hostname, which the
        // DinD harness sets to the machine container's name.
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

pub(super) async fn start_mutable_registry(
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

/// Runs the shipped CLI under the fixture's operator home, bounded by the
/// standard CLI budget.
pub(super) fn run_cli(operator: &OperatorFixture, args: &[&str]) -> Result<Output, String> {
    run_cli_bounded(&operator.cli, &operator.home, args)
}

pub(super) fn create_namespace(
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

pub(super) fn create_namespace_and_deploy(
    operator: &OperatorFixture,
    namespace: &str,
    service: &str,
    image: &str,
    secret_name: &str,
    secret_value: &str,
) -> Result<OperationRowId, String> {
    let founder_target = operator.founder_target.clone();
    create_namespace(operator, namespace, &founder_target)?;

    let deploy = spawn_deploy(
        operator,
        namespace,
        service,
        image,
        secret_name,
        secret_value,
    )?;
    let deployed = wait_for_child(deploy, CLI_BUDGET)?;
    parse_deploy_operation(&deployed, "first deploy", secret_value)
}

pub(super) fn spawn_deploy(
    operator: &OperatorFixture,
    namespace: &str,
    service: &str,
    image: &str,
    secret_name: &str,
    secret_value: &str,
) -> Result<Child, String> {
    let environment = format!("{secret_name}={secret_value}");
    spawn_cli(
        &operator.cli,
        &operator.home,
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
    )
}

pub(super) fn parse_deploy_operation(
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

pub(super) fn assert_cluster_wide_operation_replay(
    operator: &OperatorFixture,
    operation_id: &OperationRowId,
    secret_value: &str,
) -> Result<String, String> {
    for target in std::iter::once(&operator.founder_target)
        .chain(operator.joiners.iter().map(|joined| &joined.target))
    {
        wait_for_cli_output(
            &operator.cli,
            &operator.home,
            &["ops", "list", "--target", target.as_str()],
            |output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains(operation_id.as_str())
                    && String::from_utf8_lossy(&output.stdout).contains("completed")
            },
            &format!("terminal operation through {target}"),
        )?;
    }

    let Some(replay_via) = operator.joiners.first() else {
        return Err("operation replay requires at least one joined machine".to_owned());
    };
    let mut replays = Vec::new();
    for _ in 0..2 {
        let replay = run_cli_bounded(
            &operator.cli,
            &operator.home,
            &[
                "ops",
                "watch",
                operation_id.as_str(),
                "--target",
                replay_via.target.as_str(),
            ],
        )?;
        require_success(&replay, "operation replay through joined machine")?;
        let stdout = String::from_utf8(replay.stdout).map_err(|error| error.to_string())?;
        let sequences = evidence_sequences(&stdout)?;
        let expected = (1..=u64::try_from(sequences.len()).map_err(|error| error.to_string())?)
            .collect::<Vec<_>>();
        require(
            sequences == expected
                && stdout.contains("terminal deploy completed")
                && !stdout.contains(secret_value),
            format!("operation replay was not complete and duplicate-free: {stdout}"),
        )?;
        replays.push(stdout);
    }
    require(
        matches!(replays.as_slice(), [first, second] if first == second),
        "two full operation replays returned different durable evidence",
    )?;
    let Some(replay) = replays.into_iter().next() else {
        return Err("operation replay was not captured".to_owned());
    };
    Ok(replay)
}

/// The revision-gated cutover leaves its evidence in claim, commit, drain,
/// clean order; each marker is the `ops watch` rendering of one durable event.
pub(super) fn assert_replay_orders_cutover_evidence(replay: &str) -> Result<(), String> {
    let mut remainder = replay;
    for marker in [
        "operation claim won",
        "rows committed",
        "drained",
        "incumbent removed",
    ] {
        let Some(offset) = remainder.find(marker) else {
            return Err(format!(
                "cutover replay lacked {marker:?} in claim/commit/drain/clean order: {replay}"
            ));
        };
        remainder = &remainder[offset + marker.len()..];
    }
    Ok(())
}

pub(super) async fn wait_for_public_deploy_rows(
    docker: &Docker,
    requester: &DindMachine,
    api_address: &str,
    service_name: &str,
    operation_id: &OperationRowId,
) -> Result<PublicDeployRows, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("public lenses were not queried");
    while Instant::now() < deadline {
        let services = public_lens(docker, requester, api_address, LensCollection::Services).await;
        let containers =
            public_lens(docker, requester, api_address, LensCollection::Containers).await;
        let operations =
            public_lens(docker, requester, api_address, LensCollection::Operations).await;
        match (services, containers, operations) {
            (
                Ok(LensSnapshot::Services { rows: service_rows }),
                Ok(LensSnapshot::Containers {
                    rows: container_rows,
                }),
                Ok(LensSnapshot::Operations {
                    rows: operation_rows,
                }),
            ) => {
                let service = service_rows.iter().find(|row| {
                    row.document.name.as_str() == service_name
                        && &row.document.operation_id == operation_id
                });
                if let Some(service) = service {
                    let container = container_rows.iter().find(|row| {
                        row.document.service_id == service.id
                            && &row.document.deploy == operation_id
                    });
                    let operation = operation_rows.iter().find(|row| &row.id == operation_id);
                    if let (Some(container), Some(_operation)) = (container, operation) {
                        let service_container_deploys = container_rows
                            .iter()
                            .filter(|row| row.document.service_id == service.id)
                            .map(|row| row.document.deploy.clone())
                            .collect::<Vec<_>>();
                        let service = service.document.clone();
                        let container_ip = container.document.ip;
                        let machines =
                            public_lens(docker, requester, api_address, LensCollection::Machines)
                                .await?;
                        let machine_status = public_lens(
                            docker,
                            requester,
                            api_address,
                            LensCollection::MachineStatus,
                        )
                        .await?;
                        let encoded = serde_json::to_string(&(
                            &service_rows,
                            &container_rows,
                            &operation_rows,
                            &machines,
                            &machine_status,
                        ))
                        .map_err(|error| error.to_string())?;
                        return Ok(PublicDeployRows {
                            service,
                            container_ip,
                            service_container_deploys,
                            encoded,
                        });
                    }
                }
                last = "joined API lenses had not converged the service and container".to_owned();
            }
            (Ok(services), Ok(containers), Ok(operations)) => {
                last = format!(
                    "public API returned the wrong lenses: services={services:?} containers={containers:?} operations={operations:?}"
                )
            }
            (Err(error), _, _) => last = error,
            (_, Err(error), _) => last = error,
            (_, _, Err(error)) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("public deploy rows did not converge: {last}"))
}

pub(super) async fn assert_driver_local_evidence_is_secret_free(
    docker: &Docker,
    driver: &DindMachine,
    operation_id: &OperationRowId,
    secret_value: &str,
) -> Result<(), String> {
    let path = format!("/var/lib/ployz/api/evidence/{operation_id}.jsonl");
    let evidence = exec_ok(docker, driver, &["cat", &path]).await?;
    require(
        !evidence.stdout.contains(secret_value) && !evidence.stderr.contains(secret_value),
        format!("driver-local operation evidence exposed an environment value at {path}"),
    )
}

pub(super) fn assert_public_rows_are_digest_pinned_and_secret_free(
    rows: &PublicDeployRows,
    requested_image: &str,
    secret_name: &str,
    secret_value: &str,
    expected_fingerprint: &Sha256Hex,
) -> Result<(), String> {
    require(
        rows.service.image.as_str() != requested_image
            && rows.service.image.as_str().contains("@sha256:"),
        format!(
            "service row did not replace mutable image {requested_image} with a digest pin: {}",
            rows.service.image.as_str()
        ),
    )?;
    require(
        !rows.encoded.contains(secret_value)
            && rows.service.env_fingerprints.len() == 1
            && rows.service.env_fingerprints.get(secret_name) == Some(expected_fingerprint),
        format!(
            "public rows retained an environment value or the wrong fingerprint: {}",
            rows.encoded
        ),
    )
}

/// The converged post-cutover truth: the service row activates the second
/// deploy under a fresh digest pin while naming the first pin as previous,
/// and exactly one container row survives, owned by the second operation.
pub(super) fn assert_cutover_rows(
    second: &PublicDeployRows,
    first: &PublicDeployRows,
    second_operation: &OperationRowId,
) -> Result<(), String> {
    require(
        second.service.active_deploy == *second_operation,
        format!(
            "service row did not activate the second deploy: {}",
            second.encoded
        ),
    )?;
    require(
        second.service.previous_image.as_ref() == Some(&first.service.image),
        format!(
            "service row did not name the first image as previous_image: {}",
            second.encoded
        ),
    )?;
    require(
        second.service.image != first.service.image,
        format!(
            "second deploy did not re-resolve the mutable tag to a new digest: {}",
            second.encoded
        ),
    )?;
    require(
        matches!(
            second.service_container_deploys.as_slice(),
            [deploy] if deploy == second_operation
        ),
        format!(
            "service did not converge to exactly one container row owned by the second deploy: {:?}",
            second.service_container_deploys
        ),
    )
}

/// Pushes a distinguishable second revision under the same mutable tag so a
/// redeploy of the incumbent must re-resolve the registry manifest.
pub(super) async fn push_second_revision(
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

pub(super) async fn assert_dns_and_http(
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

/// Observes service traffic while the spawned second deploy runs: the first
/// revision's body must keep serving until the response flips to the second
/// revision, and no other body may ever appear. Connect-level failures are
/// retried as races; a served wrong body fails immediately.
pub(super) async fn watch_cutover_traffic(
    docker: &Docker,
    dns_client: &DindMachine,
    http_client: &DindMachine,
    resolver: Ipv4Addr,
    hostname: &str,
    bodies: RevisionBodies<'_>,
    deploy: Child,
) -> Result<Output, String> {
    let deadline = Instant::now() + CLI_BUDGET;
    let mut deploy = Some(deploy);
    let mut deploy_output = None;
    let mut served_first = false;
    let mut served_second = false;
    while deploy_output.is_none() || !served_second {
        if Instant::now() >= deadline {
            if let Some(mut child) = deploy.take() {
                let _ = child.kill();
            }
            return Err(format!(
                "cutover did not complete within {CLI_BUDGET:?}: \
                 deploy_finished={} served_first={served_first} served_second={served_second}",
                deploy_output.is_some(),
            ));
        }
        if let Some(mut child) = deploy.take() {
            if child
                .try_wait()
                .map_err(|error| format!("could not poll second deploy: {error}"))?
                .is_some()
            {
                let output = child
                    .wait_with_output()
                    .map_err(|error| format!("could not collect second deploy output: {error}"))?;
                require_success(&output, "second deploy")?;
                deploy_output = Some(output);
            } else {
                deploy = Some(child);
            }
        }
        if let Ok(addresses) = query_dns(docker, dns_client, resolver, hostname).await
            && let [address, ..] = addresses.as_slice()
            && let Some(body) = fetch_http_body(docker, http_client, hostname, *address).await?
        {
            if body.contains(bodies.second) {
                served_second = true;
            } else if body.contains(bodies.first) {
                require(
                    !served_second,
                    format!("first revision served again after the cutover flip: {body}"),
                )?;
                served_first = true;
            } else {
                return Err(format!("cutover window served an unexpected body: {body}"));
            }
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    require(
        served_first,
        "the first revision never served during the cutover window",
    )?;
    deploy_output.ok_or_else(|| "second deploy output was not collected".to_owned())
}

/// After the cutover cleans, no Docker container labelled with the first
/// operation may survive on any machine.
pub(super) async fn assert_first_revision_container_is_gone(
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

/// One HTTP fetch against a resolved address. `Ok(None)` is a connect-level
/// failure the caller may retry; served error pages come back as bodies.
async fn fetch_http_body(
    docker: &Docker,
    http_client: &DindMachine,
    hostname: &str,
    address: Ipv4Addr,
) -> Result<Option<String>, String> {
    let resolved = format!("{hostname}:80:{address}");
    let url = format!("http://{hostname}/");
    let outcome = exec_in_container(
        docker,
        &http_client.container_id,
        &[
            "curl",
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--max-time",
            "5",
            "--resolve",
            &resolved,
            &url,
        ],
    )
    .await
    .map_err(|error| error.to_string())?;
    if outcome.success() {
        Ok(Some(outcome.stdout))
    } else {
        Ok(None)
    }
}

async fn run_founding(
    docker: &Docker,
    machine: &DindMachine,
    operator: &SshPeerKey,
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
        "disabled".to_owned(),
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
                Ok(roster) if roster.len() >= minimum => return Ok(roster),
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

pub(super) async fn public_lens(
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

fn evidence_sequences(output: &str) -> Result<Vec<u64>, String> {
    output
        .lines()
        // The watch renderer indents multi-line evidence details (placement
        // bids, targets, eliminations) as continuation lines; only unindented
        // lines open a durable event and carry its sequence.
        .filter(|line| !line.starts_with(' '))
        .map(|line| {
            let Some((sequence, _)) = line.split_once('\t') else {
                return Err(format!(
                    "operation evidence line omitted its sequence: {line:?}"
                ));
            };
            sequence
                .parse::<u64>()
                .map_err(|error| format!("invalid evidence sequence {sequence:?}: {error}"))
        })
        .collect()
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
                )
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

pub(super) fn require_success(output: &Output, operation: &str) -> Result<(), String> {
    require(
        output.status.success(),
        format!(
            "{operation} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}
