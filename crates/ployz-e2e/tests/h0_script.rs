use std::process::Command;

#[test]
fn hetzner_h0_rejects_invalid_run_id_before_external_work() {
    let output = Command::new("sh")
        .arg(script_path())
        .arg("up")
        .arg("--run-id")
        .arg("Bad_Run")
        .output()
        .expect("script runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).contains("run id must contain only lowercase letters"));
}

#[test]
fn hetzner_h0_requires_ebpf_bytecode_before_hcloud() {
    let temp = temp_path("ployz-h0-ssh-key");
    let nats = temp_path("ployz-h0-nats-server");
    let ebpf_ctl = temp_path("ployz-h0-ebpf-ctl");
    let missing_ebpf = temp_path("ployz-h0-missing-ebpf");
    std::fs::write(&temp, "key").expect("ssh key can be written");
    std::fs::write(&nats, "nats").expect("nats artifact can be written");
    std::fs::write(&ebpf_ctl, "ctl").expect("ebpf ctl artifact can be written");
    let _ = std::fs::remove_file(&missing_ebpf);
    let output = Command::new("sh")
        .arg(script_path())
        .arg("up")
        .arg("--run-id")
        .arg("valid-run")
        .env("HCLOUD_TOKEN", "token")
        .env("HETZNER_SSH_KEY", "key")
        .env("PLOYZ_SSH_PRIVATE_KEY", &temp)
        .env("PLOYZ_ACCEPTANCE_NATS_SERVER", &nats)
        .env("PLOYZ_ACCEPTANCE_EBPF_CTL", &ebpf_ctl)
        .env("PLOYZ_ACCEPTANCE_EBPF_BYTECODE", &missing_ebpf)
        .output()
        .expect("script runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).contains("Ployz eBPF bytecode does not exist"));
    let _ = std::fs::remove_file(temp);
    let _ = std::fs::remove_file(nats);
    let _ = std::fs::remove_file(ebpf_ctl);
}

#[test]
fn hetzner_h0_script_drives_the_product_path() {
    let script = std::fs::read_to_string(script_path()).expect("script is readable");

    assert!(script.contains(" init join-template "));
    assert!(script.contains("tunnel identity --secret-key-file"));
    assert!(script.contains("core_iroh_public_key="));
    assert!(script.contains("edge_bootstrap_nats_url=\"nats://127.0.0.1:7423\""));
    assert!(script.contains("--runtime-nats-url '$edge_runtime_nats_url'"));
    assert!(script.contains("--trusted-first-node core_1"));
    assert!(script.contains("--core-iroh-public-key '$core_iroh_public_key'"));
    assert!(script.contains("--core-iroh-direct-address '${core_ip}:${core_iroh_port}'"));
    assert!(script.contains("--secret-delivery-file"));
    assert!(!script.contains("--core-iroh-public-key acceptance-core"));
    assert!(!script.contains("--nats-credentials"));
    assert!(!script.contains("--trusted-nats-config-sha256"));
    assert!(script.contains(" init --node core_1 --node-public-ip '$core_ip'"));
    assert!(script.contains(" machine add --node edge_2"));
    assert!(script.contains("start-bootstrap-tunnel"));
    assert!(script.contains("PLOYZ_TUNNEL_LISTEN_ADDR='$edge_bootstrap_tunnel_listen_addr'"));
    assert!(script.contains("PLOYZ_TUNNEL_CORE_PUBLIC_KEY='$core_iroh_public_key'"));
    assert!(script.contains("PLOYZ_TUNNEL_CORE_DIRECT_ADDRS='${core_ip}:${core_iroh_port}'"));
    assert!(script.contains(" sh '$remote_ployz_sh' --join-token"));
    assert!(script.contains("PLOYZ_NATS_URL='$edge_bootstrap_nats_url' PLOYZ_NODE_PUBLIC_IP='$edge_ip' sh '$remote_ployz_sh'"));
    assert!(script.contains("wait_for_machine_ready \"$core_ip\" edge_2 \"$edge_ip\""));
    assert!(script.contains("gateway current"));
    assert!(script.contains("inspect-edge-through-runtime-tunnel"));
    assert!(script.contains(
        "PLOYZ_NATS_URL='$edge_runtime_nats_url' '$remote_ployzctl' machine inspect edge_2"
    ));
    assert!(script.contains(" deploy --detach"));
    assert!(script.contains(" ops watch op_deploy_smoke"));
    assert!(script.contains("wait_for_smoke_service core \"$core_ip\""));
    assert!(script.contains("wait_for_smoke_service edge \"$edge_ip\""));
    assert!(script.contains("curl-smoke-${name}.log"));
    assert!(script.contains(
        "curl -fsS --connect-timeout 2 --max-time 5 -H 'Host: smoke.local' \"http://${ip}:8080/\""
    ));
    assert!(script.contains("PLOYZ_ACCEPTANCE_EBPF_BYTECODE"));
    assert!(script.contains("PLOYZ_ACCEPTANCE_EBPF_CTL"));
    assert!(script.contains("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"));
    assert!(script.contains("/usr/local/bin/ployz-ebpf-ctl"));
    assert!(script.contains("wireguard-tools iproute2"));
    assert!(script.contains("mount -t bpf bpf /sys/fs/bpf"));
    assert!(!script.contains("ip link add dev ployz-wg0 type wireguard"));
    assert!(!script.contains("PLOYZ_ACCEPTANCE_RUNTIME_NATS_URL"));
    assert!(!script.contains("PLOYZ_ACCEPTANCE_MACHINE_JOIN_TEMPLATE"));
    assert!(!script.contains("\"join_bundle\""));
    assert!(!script.contains("provider trait"));
}

fn script_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/hetzner-two-node-acceptance.sh")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("{name}-{}", std::process::id()))
}
