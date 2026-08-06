//! Generic per-machine diagnostics under `target/dind-evidence/<run>/<machine>`.

use super::DindError;
use super::cluster::DindRunId;
use super::exec::{ExecOutcome, exec_in_container};
use super::machine::DindMachine;
use bollard::Docker;
use std::path::{Path, PathBuf};

const EVIDENCE_DIR_NAME: &str = "dind-evidence";

#[must_use]
pub fn evidence_dir(run_id: &DindRunId) -> PathBuf {
    target_dir().join(EVIDENCE_DIR_NAME).join(run_id.as_str())
}

/// Captures bounded system, network, role-process, Corrosion, and eBPF diagnostics.
pub async fn capture_machine_evidence(
    docker: &Docker,
    run_id: &DindRunId,
    machine: &DindMachine,
) -> Result<PathBuf, DindError> {
    let machine_dir = evidence_dir(run_id).join(&machine.name);
    std::fs::create_dir_all(&machine_dir).map_err(|source| DindError::EvidenceIo {
        path: machine_dir.clone(),
        message: source.to_string(),
    })?;

    let captures: [(&str, &[&str]); 25] = [
        (
            "journal.txt",
            &["journalctl", "--no-pager", "--lines", "2000"],
        ),
        (
            "systemctl-failed.txt",
            &["systemctl", "--failed", "--no-pager"],
        ),
        ("docker-ps.txt", &["docker", "ps", "-a"]),
        ("wireguard.txt", &["wg", "show", "all", "dump"]),
        ("addresses.txt", &["ip", "-details", "address", "show"]),
        (
            "routes-v4.txt",
            &["ip", "-4", "route", "show", "table", "all"],
        ),
        (
            "routes-v6.txt",
            &["ip", "-6", "route", "show", "table", "all"],
        ),
        (
            "ipv6-sysctls.txt",
            &[
                "sh",
                "-c",
                "sysctl net.ipv6.conf.all.disable_ipv6 net.ipv6.conf.default.disable_ipv6 net.ipv6.conf.all.forwarding net.ipv6.conf.default.forwarding",
            ],
        ),
        ("firewall.txt", &["ufw", "status", "verbose"]),
        ("iptables.txt", &["iptables-save"]),
        ("ip6tables.txt", &["ip6tables-save"]),
        ("sockets.txt", &["ss", "-lntup"]),
        (
            "ployz-journals.txt",
            &[
                "journalctl",
                "--no-pager",
                "--lines",
                "2000",
                "-u",
                "ployz-keeper.service",
                "-u",
                "ployz-api.service",
                "-u",
                "ployzd-dns.service",
            ],
        ),
        (
            "corrosion-journal.txt",
            &[
                "journalctl",
                "--no-pager",
                "--lines",
                "2000",
                "-u",
                "corrosion.service",
            ],
        ),
        (
            "corrosion-config.txt",
            &[
                "sh",
                "-c",
                "sed -E 's/(bearer-token[[:space:]]*=[[:space:]]*).*/\\1\"[REDACTED]\"/' /etc/ployz/corrosion.toml",
            ],
        ),
        (
            "corrosion-health.txt",
            &[
                "sh",
                "-c",
                "if [ -f /var/lib/ployz/ployzd.env ] && [ -f /var/lib/ployz/corrosion-token ]; then addr=$(sed -n 's/^PLOYZ_CORROSION_API_ADDR=//p' /var/lib/ployz/ployzd.env); token=$(cat /var/lib/ployz/corrosion-token); else addr=127.0.0.1:8080; token=ployz-dind-corrosion; fi; curl --noproxy '*' --fail --silent --show-error -H \"Authorization: Bearer ${token}\" \"http://${addr}/v1/health\"",
            ],
        ),
        (
            "machine-status.txt",
            &[
                "sh",
                "-c",
                "if [ -f /var/lib/ployz/ployzd.env ] && [ -f /var/lib/ployz/corrosion-token ]; then addr=$(sed -n 's/^PLOYZ_CORROSION_API_ADDR=//p' /var/lib/ployz/ployzd.env); token=$(cat /var/lib/ployz/corrosion-token); else addr=127.0.0.1:8080; token=ployz-dind-corrosion; fi; curl --noproxy '*' --fail --silent --show-error -H \"Authorization: Bearer ${token}\" -H 'Accept: application/json' -H 'Content-Type: application/json' --data '\"SELECT machine_id AS id, document FROM machine_status ORDER BY machine_id\"' \"http://${addr}/v1/queries\"",
            ],
        ),
        (
            "ebpf-pins.txt",
            &[
                "sh",
                "-c",
                "find /sys/fs/bpf/ployz /sys/fs/bpf/ployz-isolation -maxdepth 2 -printf '%y %p\\n' 2>&1",
            ],
        ),
        (
            "ebpf-status.txt",
            &[
                "sh",
                "-c",
                "/opt/ployz/artifacts/ployz-ebpf-ctl status ployz-test0 2>&1",
            ],
        ),
        (
            "ebpf-status-br-ployz.txt",
            &[
                "sh",
                "-c",
                "/opt/ployz/artifacts/ployz-ebpf-ctl status br-ployz 2>&1",
            ],
        ),
        ("pid1-cgroup.txt", &["cat", "/proc/1/cgroup"]),
        (
            "isolation-cgroup.txt",
            &[
                "sh",
                "-c",
                "scope=$(sed -n 's/^0::\\(.*\\)\\/init\\.scope$/\\1/p' /proc/1/cgroup); bpftool cgroup show \"/sys/fs/cgroup${scope}\" 2>&1",
            ],
        ),
        (
            "isolation-status.txt",
            &[
                "sh",
                "-c",
                "/opt/ployz/artifacts/ployz-ebpf-ctl --pin-path /sys/fs/bpf/ployz isolation status 2>&1",
            ],
        ),
        (
            "isolation-namespaces.txt",
            &[
                "sh",
                "-c",
                "bpftool -j map dump pinned /sys/fs/bpf/ployz-isolation/namespaces 2>&1",
            ],
        ),
        (
            "ebpf-routes.txt",
            &[
                "sh",
                "-c",
                "bpftool -j map dump pinned /sys/fs/bpf/ployz/routes 2>&1",
            ],
        ),
    ];
    for (file_name, command) in captures {
        let content = match exec_in_container(docker, &machine.container_id, command).await {
            Ok(outcome) => render_outcome(&outcome),
            Err(error) => format!("evidence capture failed: {error}\n"),
        };
        let path = machine_dir.join(file_name);
        std::fs::write(&path, content).map_err(|source| DindError::EvidenceIo {
            path: path.clone(),
            message: source.to_string(),
        })?;
    }
    Ok(machine_dir)
}

fn render_outcome(outcome: &ExecOutcome) -> String {
    let ExecOutcome {
        exit_code,
        stdout,
        stderr,
    } = outcome;
    if *exit_code == 0 && stderr.is_empty() {
        return stdout.clone();
    }
    format!("{stdout}\n--- exit code {exit_code}; stderr ---\n{stderr}")
}

fn target_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(dir);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
}
