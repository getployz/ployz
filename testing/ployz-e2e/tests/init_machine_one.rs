use bollard::Docker;
use ployz_core::corrosion::SqliteValue;
use ployz_core::{API_MAJOR, ApiRefusal, LensCollection, LensSnapshot, lens_route};
use ployz_e2e::dind::{
    ARTIFACT_ROOT, DindCluster, DindClusterSpec, DindMachine, ExecOutcome, MachineSpec,
    RELEASE_MANIFEST, artifact_dir, assert_keeper_isolation_root, connect_docker, corrosion_query,
    e2e_enabled, env_value, exec_in_container, exec_ok, install_local_release_channel,
    keep_requested, machine_image, render_release_manifest, require, write_release_manifest,
};
use serde_json::Value;
use std::collections::BTreeMap;

const DRIVER_PEER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAP";
const DRIVER_PEER_NAME: &str = "dind-driver";
const CLUSTER_NAME: &str = "dind-init";
const MACHINE_NAME: &str = "machine-one";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shipped_cli_founds_machine_one_and_retries_safely() {
    if !e2e_enabled() {
        eprintln!("skipping machine-one DinD proof; set PLOYZ_DIND_E2E=1 to enable it");
        return;
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    panic!("the pinned Corrosion machine-one proof supports only Linux x86_64");

    let docker = connect_docker().expect("connect to Docker for machine-one proof");
    let cluster = DindCluster::provision(
        &docker,
        DindClusterSpec {
            artifact_dir: artifact_dir(),
            machines: vec![
                MachineSpec {
                    image: machine_image(),
                },
                MachineSpec {
                    image: machine_image(),
                },
            ],
        },
    )
    .await
    .expect("provision privileged machine one");

    let result = exercise_machine_one(&docker, &cluster).await;
    if let Err(error) = &result {
        match cluster.capture_evidence().await {
            Ok(path) => eprintln!("machine-one evidence captured under {}", path.display()),
            Err(capture_error) => eprintln!("machine-one evidence capture failed: {capture_error}"),
        }
        eprintln!("machine-one proof failed: {error}");
    }
    if keep_requested() {
        eprintln!(
            "retaining DinD run {} because PLOYZ_DIND_KEEP=1",
            cluster.run_id()
        );
    } else {
        cluster.teardown().await.expect("tear down machine-one run");
    }
    result.unwrap_or_else(|error| panic!("machine-one proof failed: {error}"));
}

async fn exercise_machine_one(docker: &Docker, cluster: &DindCluster) -> Result<(), String> {
    let [clean_machine, interrupted_machine] = cluster.machines() else {
        return Err("machine-one proof requires two isolated machines".to_owned());
    };
    exercise_clean_complete_and_foreign(docker, clean_machine).await?;
    exercise_interrupted_resume(docker, interrupted_machine).await
}

async fn exercise_clean_complete_and_foreign(
    docker: &Docker,
    machine: &DindMachine,
) -> Result<(), String> {
    let _manifest = install_local_release_channel(docker, machine).await?;
    let peer_key = driver_public_key(docker, machine).await?;

    let first = run_init(docker, machine, &peer_key).await?;
    require(
        first.stdout.contains("Found") && first.stdout.contains("Next: ployz token create"),
        format!("first init did not print found summary: {first:?}"),
    )?;
    assert_runtime(docker, machine, &peer_key).await?;

    let second = run_init(docker, machine, &peer_key).await?;
    require(
        second.stdout.contains("Already founded")
            && second.stdout.contains("Next: ployz token create"),
        format!("second init did not print complete no-op summary: {second:?}"),
    )?;
    assert_runtime(docker, machine, &peer_key).await?;

    exec_ok(docker, machine, &["touch", "/var/lib/ployz/join-complete"]).await?;
    let before = effect_fingerprint(docker, machine).await?;
    let refused = run_init_allow_failure(docker, machine, &peer_key).await?;
    require(
        !refused.success()
            && refused.stderr.contains("ployz machine reset")
            && !refused.stderr.contains("--force"),
        format!("foreign-state init did not return the exact repair refusal: {refused:?}"),
    )?;
    let after = effect_fingerprint(docker, machine).await?;
    require(
        before == after,
        format!(
            "foreign-state refusal changed founding effects\nbefore:\n{before}\nafter:\n{after}"
        ),
    )
}

async fn exercise_interrupted_resume(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let manifest = install_local_release_channel(docker, machine).await?;
    write_release_manifest(docker, machine, &corrupt_ployzd_digest(&manifest)?).await?;
    let peer_key = driver_public_key(docker, machine).await?;
    let interrupted = run_init_allow_failure(docker, machine, &peer_key).await?;
    require(
        !interrupted.success(),
        format!("wrong artifact digest unexpectedly founded a cluster: {interrupted:?}"),
    )?;
    exec_ok(
        docker,
        machine,
        &["test", "-s", "/var/lib/ployz/cluster-id"],
    )
    .await?;
    let complete = exec_in_container(
        docker,
        &machine.container_id,
        &["test", "-e", "/var/lib/ployz/founding-complete"],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(
        !complete.success(),
        "interrupted founding was incorrectly marked complete",
    )?;

    write_release_manifest(docker, machine, &manifest).await?;
    let resumed = run_init(docker, machine, &peer_key).await?;
    require(
        resumed.stdout.contains("Resumed") && resumed.stdout.contains("Next: ployz token create"),
        format!("partial founding did not resume through the shipped CLI: {resumed:?}"),
    )?;
    assert_runtime(docker, machine, &peer_key).await
}

async fn driver_public_key(docker: &Docker, machine: &DindMachine) -> Result<String, String> {
    let peer_key = exec_ok(docker, machine, &["sh", "-c", "wg genkey | wg pubkey"])
        .await?
        .stdout
        .trim()
        .to_owned();
    if peer_key.is_empty() {
        return Err("wg genkey returned an empty driver public key".to_owned());
    }
    Ok(peer_key)
}

fn corrupt_ployzd_digest(manifest: &str) -> Result<String, String> {
    let mut replaced = false;
    let mut output = String::new();
    for line in manifest.lines() {
        if line.starts_with("PLOYZD_SHA256=") {
            output.push_str(&format!("PLOYZD_SHA256={}\n", "0".repeat(64)));
            replaced = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    require(replaced, "local release manifest omitted PLOYZD_SHA256")?;
    Ok(output)
}

async fn run_init(
    docker: &Docker,
    machine: &DindMachine,
    peer_key: &str,
) -> Result<ExecOutcome, String> {
    let outcome = run_init_allow_failure(docker, machine, peer_key).await?;
    if outcome.success() {
        Ok(outcome)
    } else {
        Err(format!("ployz init failed: {outcome:?}"))
    }
}

async fn run_init_allow_failure(
    docker: &Docker,
    machine: &DindMachine,
    peer_key: &str,
) -> Result<ExecOutcome, String> {
    let manifest = format!("file://{RELEASE_MANIFEST}");
    let command = [
        "env",
        &format!("PLOYZ_RELEASE_MANIFEST_URL={manifest}"),
        "/opt/ployz/artifacts/ployz",
        "init",
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
        "--driver-peer-id",
        DRIVER_PEER_ID,
        "--driver-peer-name",
        DRIVER_PEER_NAME,
        "--driver-peer-public-key",
        peer_key,
    ];
    exec_in_container(docker, &machine.container_id, &command)
        .await
        .map_err(|error| error.to_string())
}

async fn assert_runtime(
    docker: &Docker,
    machine: &DindMachine,
    peer_key: &str,
) -> Result<(), String> {
    exec_ok(
        docker,
        machine,
        &[
            "systemctl",
            "is-active",
            "ployzd-keeper.service",
            "ployz-corrosion.service",
            "ployzd-api.service",
            "ployzd-gateway.service",
            "ployzd-dns.service",
        ],
    )
    .await?;
    assert_keeper_isolation_root(docker, machine, "ployzd-keeper.service").await?;
    exec_ok(
        docker,
        machine,
        &["test", "-f", "/etc/systemd/system/ployzd-gateway.service"],
    )
    .await?;
    exec_ok(
        docker,
        machine,
        &["test", "-f", "/etc/systemd/system/ployzd-dns.service"],
    )
    .await?;
    exec_ok(
        docker,
        machine,
        &["systemctl", "is-enabled", "ployzd-gateway.service"],
    )
    .await?;
    assert_gateway_privileges(docker, machine).await?;
    exec_ok(
        docker,
        machine,
        &["systemctl", "is-enabled", "ployzd-dns.service"],
    )
    .await?;

    let network = exec_ok(
        docker,
        machine,
        &[
            "docker",
            "network",
            "inspect",
            "ployz",
            "--format",
            "{{json .IPAM.Config}}",
        ],
    )
    .await?;
    let ipam: Value = serde_json::from_str(network.stdout.trim())
        .map_err(|error| format!("Docker network returned invalid IPAM JSON: {error}"))?;
    require(
        ipam.as_array().is_some_and(|configs| {
            matches!(configs.as_slice(), [config] if config.get("Subnet").and_then(Value::as_str) == Some("10.210.0.0/24"))
        }),
        format!("ployz network does not have exactly the first /24: {ipam}"),
    )?;

    let env = read_file(docker, machine, "/var/lib/ployz/ployzd.env").await?;
    let corrosion_address = env_value(&env, "PLOYZ_CORROSION_API_ADDR")?;
    let api_address = env_value(&env, "PLOYZ_API_LISTEN_ADDR")?;
    assert_machines_lens(docker, machine, &api_address).await?;
    let bootstrap_file = exec_in_container(
        docker,
        &machine.container_id,
        &["test", "-e", "/var/lib/ployz/bootstrap-credential"],
    )
    .await
    .map_err(|error| error.to_string())?;
    require(
        !bootstrap_file.success(),
        "bootstrap credential remained after ordinary API readiness",
    )?;
    let token = read_file(docker, machine, "/var/lib/ployz/corrosion-token")
        .await?
        .trim()
        .to_owned();
    let rows = corrosion_query(
        docker,
        machine,
        &corrosion_address,
        &token,
        "SELECT (SELECT COUNT(*) FROM cluster), (SELECT COUNT(*) FROM machines), (SELECT COUNT(*) FROM peers), (SELECT COUNT(*) FROM tokens), (SELECT COUNT(*) FROM namespaces)",
    )
    .await?;
    require(
        rows == vec![vec![
            SqliteValue::Integer(1),
            SqliteValue::Integer(1),
            SqliteValue::Integer(1),
            SqliteValue::Integer(0),
            SqliteValue::Integer(0),
        ]],
        format!("founding row counts were not cluster+machine+driver only: {rows:?}"),
    )?;

    let documents = corrosion_query(
        docker,
        machine,
        &corrosion_address,
        &token,
        "SELECT document FROM cluster UNION ALL SELECT document FROM machines UNION ALL SELECT document FROM peers",
    )
    .await?;
    assert_documents(&documents, peer_key)?;
    let peers = exec_ok(docker, machine, &["wg", "show", "ployz0", "peers"])
        .await?
        .stdout;
    require(
        peers.lines().any(|line| line.trim() == peer_key),
        format!("Keeper did not converge the enrolled driver peer: {peers:?}"),
    )?;

    let version_url = format!("http://{api_address}/version");
    let version = exec_ok(
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
            &version_url,
        ],
    )
    .await?;
    let version: Value = serde_json::from_str(version.stdout.trim())
        .map_err(|error| format!("/version returned invalid JSON: {error}"))?;
    require(
        version.get("major").and_then(Value::as_u64) == Some(u64::from(API_MAJOR)),
        format!("/version returned the wrong API major: {version}"),
    )?;
    assert_founding_route_disabled(docker, machine, &api_address).await
}

async fn assert_gateway_privileges(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    let properties = exec_ok(
        docker,
        machine,
        &[
            "systemctl",
            "show",
            "ployzd-gateway.service",
            "--property=User",
            "--property=Group",
            "--property=NoNewPrivileges",
            "--property=AmbientCapabilities",
            "--property=CapabilityBoundingSet",
            "--property=SupplementaryGroups",
        ],
    )
    .await?
    .stdout;
    for expected in [
        "User=ployz-gateway",
        "Group=ployz-gateway",
        "NoNewPrivileges=yes",
        "AmbientCapabilities=cap_net_bind_service",
        "CapabilityBoundingSet=cap_net_bind_service",
        "SupplementaryGroups=",
    ] {
        require(
            properties.lines().any(|line| line == expected),
            format!("gateway unit omitted {expected:?}: {properties}"),
        )?;
    }

    let unit = exec_ok(
        docker,
        machine,
        &["systemctl", "cat", "ployzd-gateway.service"],
    )
    .await?
    .stdout;
    require(
        unit.lines()
            .any(|line| line == "EnvironmentFile=/var/lib/ployz/ployz-gateway.env"),
        format!("gateway unit does not use its scoped environment: {unit}"),
    )?;
    let environment = read_file(docker, machine, "/var/lib/ployz/ployz-gateway.env").await?;
    let names = environment
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .collect::<Vec<_>>();
    require(
        names
            == [
                "PLOYZ_CORROSION_API_ADDR",
                "PLOYZ_CORROSION_BEARER_TOKEN",
                "PLOYZ_CLUSTER_ID",
                "PLOYZ_MACHINE_ID",
                "PLOYZ_GATEWAY_LISTEN_ADDR",
            ],
        format!("gateway environment has a broader contract than required: {names:?}"),
    )
}

async fn assert_machines_lens(
    docker: &Docker,
    machine: &DindMachine,
    api_address: &str,
) -> Result<(), String> {
    let url = format!(
        "http://{api_address}{}",
        lens_route(LensCollection::Machines)
    );
    let outcome = exec_ok(
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
    )
    .await?;
    let snapshot: LensSnapshot = serde_json::from_str(outcome.stdout.trim())
        .map_err(|error| format!("machines lens returned invalid JSON: {error}"))?;
    let LensSnapshot::Machines { cluster, rows } = snapshot else {
        return Err("machines route returned another lens collection".to_owned());
    };
    require(
        cluster.name == CLUSTER_NAME
            && matches!(rows.as_slice(), [row] if row.document.name.as_str() == MACHINE_NAME),
        format!("machines lens did not expose machine one: cluster={cluster:?} rows={rows:?}"),
    )
}

async fn assert_founding_route_disabled(
    docker: &Docker,
    machine: &DindMachine,
    api_address: &str,
) -> Result<(), String> {
    let url = format!("http://{api_address}/founding");
    let outcome = exec_ok(
        docker,
        machine,
        &[
            "curl",
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--max-time",
            "3",
            "--request",
            "POST",
            "--header",
            "Content-Type: application/json",
            "--data",
            "{}",
            "--write-out",
            "\n%{http_code}",
            &url,
        ],
    )
    .await?;
    let Some((body, status)) = outcome.stdout.rsplit_once('\n') else {
        return Err(format!(
            "ordinary founding probe omitted HTTP status: {:?}",
            outcome.stdout
        ));
    };
    let refusal: ApiRefusal = serde_json::from_str(body)
        .map_err(|error| format!("ordinary founding refusal was invalid JSON: {error}"))?;
    require(
        status.trim() == "404" && refusal == ApiRefusal::UnsupportedRoute,
        format!("ordinary API left founding enabled: HTTP {status} {refusal:?}"),
    )
}

fn assert_documents(rows: &[Vec<SqliteValue>], peer_key: &str) -> Result<(), String> {
    require(
        rows.len() == 3,
        format!("expected three founding documents: {rows:?}"),
    )?;
    let mut kinds = BTreeMap::new();
    for row in rows {
        let [SqliteValue::Text(document)] = row.as_slice() else {
            return Err(format!(
                "founding query returned a non-document row: {row:?}"
            ));
        };
        let value: Value = serde_json::from_str(document)
            .map_err(|error| format!("founding row has invalid JSON: {error}"))?;
        if value.get("prefix").is_some() {
            kinds.insert("cluster", value);
        } else if value.get("lifecycle").is_some() {
            kinds.insert("machine", value);
        } else {
            kinds.insert("peer", value);
        }
    }
    let cluster = kinds.get("cluster").ok_or("cluster document missing")?;
    require(
        cluster.get("name").and_then(Value::as_str) == Some(CLUSTER_NAME)
            && cluster.get("prefix").and_then(Value::as_str) == Some("10.210.0.0/16")
            && cluster.get("storage_default").and_then(Value::as_str) == Some("plain")
            && cluster
                .pointer("/hostname_mode/mode")
                .and_then(Value::as_str)
                == Some("disabled"),
        format!("cluster document does not preserve init answers: {cluster}"),
    )?;
    let machine = kinds.get("machine").ok_or("machine document missing")?;
    require(
        machine.get("name").and_then(Value::as_str) == Some(MACHINE_NAME)
            && machine
                .pointer("/transport/subnet_v4")
                .and_then(Value::as_str)
                == Some("10.210.0.0/24")
            && machine.pointer("/storage/mode").and_then(Value::as_str) == Some("plain")
            && machine
                .pointer("/storage/reason/kind")
                .and_then(Value::as_str)
                == Some("flag"),
        format!("machine document does not preserve founding outcome: {machine}"),
    )?;
    let peer = kinds.get("peer").ok_or("driver peer document missing")?;
    require(
        peer.get("name").and_then(Value::as_str) == Some(DRIVER_PEER_NAME)
            && peer.pointer("/transport/pubkey").and_then(Value::as_str) == Some(peer_key),
        format!("driver peer document does not match the supplied driver: {peer}"),
    )
}

async fn effect_fingerprint(docker: &Docker, machine: &DindMachine) -> Result<String, String> {
    let outcome = exec_ok(
        docker,
        machine,
        &[
            "sh",
            "-c",
            "set -eu; find /var/lib/ployz /etc/systemd/system -maxdepth 2 -type f ! -name .init.lock ! -name 'corrosion.db*' ! -path '*/subscriptions/*' -print0 | sort -z | xargs -0 sha256sum; docker network inspect ployz --format '{{json .IPAM.Config}}'; systemctl is-active ployzd-keeper.service ployz-corrosion.service ployzd-api.service",
        ],
    )
    .await?;
    Ok(outcome.stdout)
}

async fn read_file(docker: &Docker, machine: &DindMachine, path: &str) -> Result<String, String> {
    Ok(exec_ok(docker, machine, &["cat", path]).await?.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digests() -> BTreeMap<&'static str, String> {
        [
            "ployzd",
            "ployz-ebpf-tc",
            "ployz-ebpf-ctl",
            "corrosion",
            "corrosion-schema-v1.sql",
        ]
        .into_iter()
        .map(|name| (name, "a".repeat(64)))
        .collect()
    }

    #[test]
    fn local_release_manifest_is_complete_and_uses_only_mounted_artifacts() {
        let manifest = render_release_manifest(&digests()).expect("manifest");
        for key in [
            "PLOYZD_URL",
            "PLOYZ_EBPF_TC_URL",
            "PLOYZ_EBPF_CTL_URL",
            "PLOYZ_CORROSION_URL",
            "PLOYZ_CORROSION_SCHEMA_URL",
        ] {
            assert!(manifest.contains(&format!("{key}={ARTIFACT_ROOT}/")));
        }
        assert!(!manifest.contains("github.com"));
        assert!(!manifest.contains("http://"));
        assert!(!manifest.contains("https://"));
    }

    #[test]
    fn interrupted_fixture_changes_only_the_ployzd_digest() {
        let manifest = render_release_manifest(&digests()).expect("manifest");
        let corrupt = corrupt_ployzd_digest(&manifest).expect("corrupt fixture");
        assert!(corrupt.contains(&format!("PLOYZD_SHA256={}", "0".repeat(64))));
        assert_eq!(
            manifest.lines().count(),
            corrupt.lines().count(),
            "corruption must preserve the release manifest shape"
        );
    }
}
