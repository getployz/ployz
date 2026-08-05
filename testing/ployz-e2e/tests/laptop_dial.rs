use bollard::Docker;
use ployz::commands::SshTarget;
use ployz::init::ssh::{SshPeerKey, default_config_home};
use ployz::mesh::context::{LoadedOperatorContext, OperatorContextStore, context_path};
use ployz_core::corrosion::{PeerDocument, PeerTransport, SqliteValue};
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, DindMachine, MachineSpec, artifact_dir, connect_docker,
    corrosion_access, corrosion_query, e2e_enabled, exec_ok, install_local_release_channel,
    keep_requested, machine_image, require, write_file_in_container,
};
use std::fs;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const PEER_NAME: &str = "dind laptop";
const CLUSTER_NAME: &str = "dind-laptop-dial";
const MACHINE_NAME: &str = "machine-one";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unprivileged_cli_reuses_one_peer_to_list_machines_over_userspace_wireguard() {
    if !e2e_enabled() {
        eprintln!("skipping laptop-dial DinD proof; set PLOYZ_DIND_E2E=1 to enable it");
        return;
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    panic!("the pinned laptop-dial proof supports only Linux x86_64");

    assert_unprivileged_host().expect("the laptop dial must run without root");
    let docker = connect_docker().expect("connect to Docker for laptop-dial proof");
    let cluster = DindCluster::provision(
        &docker,
        DindClusterSpec {
            artifact_dir: artifact_dir(),
            machines: vec![MachineSpec {
                image: machine_image(),
            }],
        },
    )
    .await
    .expect("provision machine one");

    let result = exercise_laptop_dial(&docker, &cluster).await;
    if let Err(error) = &result {
        match cluster.capture_evidence().await {
            Ok(path) => eprintln!("laptop-dial evidence captured under {}", path.display()),
            Err(capture_error) => eprintln!("laptop-dial evidence capture failed: {capture_error}"),
        }
        eprintln!("laptop-dial proof failed: {error}");
    }
    if keep_requested() {
        eprintln!(
            "retaining DinD run {} because PLOYZ_DIND_KEEP=1",
            cluster.run_id()
        );
    } else {
        cluster.teardown().await.expect("tear down laptop-dial run");
    }
    result.unwrap_or_else(|error| panic!("laptop-dial proof failed: {error}"));
}

async fn exercise_laptop_dial(docker: &Docker, cluster: &DindCluster) -> Result<(), String> {
    let [machine] = cluster.machines() else {
        return Err("laptop-dial proof requires exactly one machine".to_owned());
    };
    install_local_release_channel(docker, machine).await?;
    install_remote_cli_fixture(docker, machine).await?;

    let temporary_home = tempfile::tempdir().map_err(|error| error.to_string())?;
    let ssh_fixture = tempfile::tempdir().map_err(|error| error.to_string())?;
    write_ssh_fixture(ssh_fixture.path())?;
    let config_home = default_config_home(temporary_home.path());
    let target: SshTarget = format!("root@{}", machine.bridge_ip).parse()?;
    let endpoint = SocketAddr::new(machine.bridge_ip, 51_820);
    let cli = artifact_dir().join("ployz");
    let init = run_ssh_init(
        &cli,
        temporary_home.path(),
        ssh_fixture.path(),
        machine,
        &target,
    )?;
    require(
        init.status.success(),
        format!(
            "shipped SSH init failed: stdout={} stderr={}",
            String::from_utf8_lossy(&init.stdout),
            String::from_utf8_lossy(&init.stderr)
        ),
    )?;
    let init_stdout = String::from_utf8_lossy(&init.stdout);
    require(
        init_stdout.contains("Found") && init_stdout.contains(MACHINE_NAME),
        format!("shipped SSH init omitted the remote founding summary: {init_stdout}"),
    )?;

    let contexts = OperatorContextStore::new(&config_home);
    let loaded = contexts
        .load_target(&target)
        .map_err(|error| format!("shipped SSH init did not persist its handoff: {error}"))?;
    let peer = SshPeerKey::load(&config_home, &target)
        .map_err(|error| format!("shipped SSH init did not persist its peer key: {error}"))?;
    require(
        peer.peer_name == PEER_NAME,
        format!(
            "SSH init persisted peer name {:?}, expected {PEER_NAME:?}",
            peer.peer_name
        ),
    )?;
    let machine_api_v6 = match loaded {
        LoadedOperatorContext::BuiltinWireguard(context) => {
            require(
                context.machine_endpoint == endpoint,
                format!(
                    "founding recorded {}, expected direct bridge endpoint {endpoint}",
                    context.machine_endpoint
                ),
            )?;
            context.machine_address
        }
        LoadedOperatorContext::UnsupportedProvider(context) => {
            return Err(format!(
                "laptop-dial proof requires builtin WireGuard, but context for {} uses unsupported provider {:?}",
                context.target.as_str(),
                context.provider
            ));
        }
    };
    assert_private_operator_state(&config_home, &target)?;
    assert_machine_ula_is_not_host_reachable(machine_api_v6)?;

    let key_before = read_only_file(&single_file(&config_home.join("mesh/init-peers"))?)?;
    for attempt in 1..=2 {
        let output = run_machine_list(&cli, temporary_home.path(), &target)?;
        require(
            output.status.success(),
            format!(
                "machine ls attempt {attempt} failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        require(
            stdout.contains(MACHINE_NAME),
            format!("machine ls attempt {attempt} omitted {MACHINE_NAME}: {stdout}"),
        )?;
    }
    wait_for_ready_status(&cli, temporary_home.path(), &target).await?;
    let key_after = read_only_file(&single_file(&config_home.join("mesh/init-peers"))?)?;
    require(
        key_before == key_after,
        "machine ls changed the authoritative operator peer key",
    )?;

    let (corrosion_address, corrosion_token) = corrosion_access(docker, machine).await?;
    let peer_count = corrosion_query(
        docker,
        machine,
        &corrosion_address,
        &corrosion_token,
        "SELECT COUNT(*) FROM peers",
    )
    .await?;
    require(
        peer_count == vec![vec![SqliteValue::Integer(1)]],
        format!("repeated laptop dials changed peer membership: {peer_count:?}"),
    )?;
    assert_only_peer_is_operator(
        corrosion_query(
            docker,
            machine,
            &corrosion_address,
            &corrosion_token,
            "SELECT id, document FROM peers",
        )
        .await?,
        &peer,
    )
}

fn assert_only_peer_is_operator(
    rows: Vec<Vec<SqliteValue>>,
    peer: &SshPeerKey,
) -> Result<(), String> {
    let [row] = rows.as_slice() else {
        return Err(format!("expected exactly one peer row, found {rows:?}"));
    };
    let [SqliteValue::Text(id), SqliteValue::Text(document)] = row.as_slice() else {
        return Err(format!("peer query returned an invalid row: {row:?}"));
    };
    let document: PeerDocument = serde_json::from_str(document)
        .map_err(|error| format!("peer row contains an invalid document: {error}"))?;
    let public_key = match &document.transport {
        PeerTransport::Wireguard { pubkey, .. } => pubkey,
        PeerTransport::Tailscale { .. } => {
            return Err("builtin founding enrolled a Tailscale peer".to_owned());
        }
    };
    require(
        id == &peer.peer_id.to_string()
            && document.name == peer.peer_name
            && public_key == &peer.public_key,
        format!("peer row does not match persisted operator identity: id={id} {document:?}"),
    )
}

fn assert_unprivileged_host() -> Result<(), String> {
    let output = Command::new("id")
        .arg("-u")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not inspect host uid: {error}"))?;
    require(output.status.success(), "host id -u failed")?;
    let uid = String::from_utf8_lossy(&output.stdout);
    require(
        uid.trim() != "0",
        "laptop-dial E2E must execute as an unprivileged host user",
    )
}

fn assert_machine_ula_is_not_host_reachable(address: std::net::Ipv6Addr) -> Result<(), String> {
    let socket = SocketAddr::new(IpAddr::V6(address), 2_020);
    let result = TcpStream::connect_timeout(&socket, Duration::from_millis(500));
    require(
        result.is_err(),
        format!("machine API ULA {address} was directly reachable before the userspace dial"),
    )
}

fn run_machine_list(cli: &Path, home: &Path, target: &SshTarget) -> Result<Output, String> {
    Command::new(cli)
        .args(["machine", "ls", "--target", target.as_str()])
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run shipped CLI {}: {error}", cli.display()))
}

async fn wait_for_ready_status(cli: &Path, home: &Path, target: &SshTarget) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut last = String::from("status was not attempted");
    while Instant::now() < deadline {
        let output = Command::new(cli)
            .args(["status", "--target", target.as_str()])
            .env("HOME", home)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("could not run shipped CLI {}: {error}", cli.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && status_is_ready_and_caught_up(&stdout) {
            return Ok(());
        }
        last = format!(
            "exit {:?} stdout={:?} stderr={:?}",
            output.status.code(),
            stdout.trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(format!(
        "shipped status did not report a ready, caught-up machine table in 45s: {last}"
    ))
}

fn status_is_ready_and_caught_up(output: &str) -> bool {
    let lines = output.lines().collect::<Vec<_>>();
    lines
        .iter()
        .any(|line| line.starts_with(&format!("cluster\t{CLUSTER_NAME}\t")))
        && lines
            .iter()
            .any(|line| line.starts_with("sync\tcaught up\t"))
        && lines.contains(&"barrier\tready")
        && lines.contains(&"NAME\tHANDSHAKE\tADDR")
        && lines.iter().any(|line| {
            matches!(
                line.split('\t').collect::<Vec<_>>().as_slice(),
                [name, "self", address] if *name == MACHINE_NAME && !address.is_empty()
            )
        })
}

fn assert_private_operator_state(config_home: &Path, target: &SshTarget) -> Result<(), String> {
    let key = single_file(&config_home.join("mesh/init-peers"))?;
    let context = context_path(config_home, target);
    for path in [&key, &context] {
        let metadata = fs::metadata(path)
            .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
        require(
            metadata.permissions().mode() & 0o777 == 0o600,
            format!("{} is not mode 0600", path.display()),
        )?;
    }
    Ok(())
}

fn single_file(directory: &Path) -> Result<std::path::PathBuf, String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("could not read {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let [entry] = entries.as_slice() else {
        return Err(format!(
            "{} contains {} entries, expected one",
            directory.display(),
            entries.len()
        ));
    };
    Ok(entry.path())
}

fn read_only_file(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))
}

/// SSH is an external subprocess seam. The fixture replaces only that
/// transport executable and forwards the shipped CLI's exact remote command
/// into the isolated Linux machine.
fn write_ssh_fixture(directory: &Path) -> Result<(), String> {
    let path = directory.join("ssh");
    fs::write(
        &path,
        r#"#!/usr/bin/env bash
set -euo pipefail
while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do
  shift
done
if [ "$#" -ne 3 ] || [ "$1" != "--" ]; then
  echo "SSH fixture expected --, target, and one remote command" >&2
  exit 64
fi
shift
target="$1"
remote_command="$2"
if [ "${target}" != "${PLOYZ_DIND_SSH_TARGET:?}" ]; then
  echo "SSH fixture received unexpected target ${target}" >&2
  exit 64
fi
exec docker exec \
  --env "PLOYZ_RELEASE_MANIFEST_URL=file:///run/ployz-dind-release.env" \
  "${PLOYZ_DIND_SSH_CONTAINER:?}" \
  sh -c "${remote_command}"
"#,
    )
    .map_err(|error| format!("could not write SSH fixture {}: {error}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("could not make SSH fixture executable: {error}"))
}

async fn install_remote_cli_fixture(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let curl = r#"#!/bin/sh
set -eu
for argument do
  if [ "${argument}" = "https://ployz.sh" ]; then
    printf ':\n'
    exit 0
  fi
done
exec /usr/bin/curl "$@"
"#;
    let sudo = "#!/bin/sh\nexec \"$@\"\n";
    write_file_in_container(
        docker,
        &machine.container_id,
        "/usr/local/bin/curl",
        curl,
        "0755",
    )
    .await
    .map_err(|error| error.to_string())?;
    write_file_in_container(
        docker,
        &machine.container_id,
        "/usr/local/bin/sudo",
        sudo,
        "0755",
    )
    .await
    .map_err(|error| error.to_string())?;
    exec_ok(
        docker,
        machine,
        &[
            "ln",
            "-sf",
            "/opt/ployz/artifacts/ployz",
            "/usr/local/bin/ployz",
        ],
    )
    .await?;
    Ok(())
}

fn run_ssh_init(
    cli: &Path,
    home: &Path,
    ssh_fixture: &Path,
    machine: &DindMachine,
    target: &SshTarget,
) -> Result<Output, String> {
    let inherited_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned());
    let path = format!("{}:{inherited_path}", ssh_fixture.display());
    Command::new(cli)
        .args([
            "init",
            target.as_str(),
            "--storage",
            "plain",
            "--container-network",
            "10.210.0.0/16",
            "--service-urls",
            "disabled",
            "--cluster-name",
            CLUSTER_NAME,
            "--machine-name",
            MACHINE_NAME,
        ])
        .env("HOME", home)
        .env("USER", "dind")
        .env("PATH", path)
        .env("PLOYZ_DIND_SSH_CONTAINER", &machine.container_id)
        .env("PLOYZ_DIND_SSH_TARGET", target.as_str())
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run shipped SSH init {}: {error}", cli.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_subprocess_fixture_is_valid_bash() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        write_ssh_fixture(directory.path()).expect("write SSH fixture");
        let fixture = directory.path().join("ssh");
        let output = Command::new("bash")
            .arg("-n")
            .arg(fixture)
            .output()
            .expect("check SSH fixture syntax");
        assert!(
            output.status.success(),
            "SSH fixture is invalid: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn status_output_reports_a_ready_caught_up_machine_table() {
        let output = "cluster\tdind-laptop-dial\t01ARZ3NDEKTSV4RRFFQ69G5FAV\t1 machine\n\
sync\tcaught up\tlag p99 0\t(as seen from machine-one)\n\
barrier\tready\n\
\n\
NAME\tHANDSHAKE\tADDR\n\
machine-one\tself\tfd12:3456:789a::1\n";

        assert!(status_is_ready_and_caught_up(output));
        assert!(!status_is_ready_and_caught_up(
            &output.replace("sync\tcaught up", "sync\tno lag sample")
        ));
        assert!(!status_is_ready_and_caught_up(
            &output.replace("NAME\tHANDSHAKE\tADDR", "NAME\tADDR")
        ));
    }
}
