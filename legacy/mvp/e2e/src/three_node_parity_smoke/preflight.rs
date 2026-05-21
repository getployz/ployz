use std::process::Command;

use super::report::PrivilegedPreflight;

pub(super) fn require_privileged_prerequisites() -> Result<PrivilegedPreflight, String> {
    let missing_prerequisites = missing_privileged_prerequisites();
    if !missing_prerequisites.is_empty() {
        return Err(format!(
            "three-node-parity-smoke prerequisites missing: {}",
            missing_prerequisites.join(", ")
        ));
    }
    Ok(PrivilegedPreflight {
        missing_prerequisites,
    })
}

fn missing_privileged_prerequisites() -> Vec<String> {
    let mut missing = Vec::new();
    if !cfg!(target_os = "linux") {
        missing.push("Linux host".to_string());
    }
    if !is_root() {
        missing.push("root privileges".to_string());
    }
    for command in ["curl", "docker", "ip", "iptables", "ployz-bpfctl"] {
        if !command_available(command) {
            missing.push(format!("command `{command}`"));
        }
    }
    if command_available("docker") && !docker_daemon_available() {
        missing.push("running Docker daemon".to_string());
    }
    missing
}

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|uid| uid.trim() == "0")
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--help")
        .output()
        .map(|output| {
            output.status.success() || !output.stderr.is_empty() || !output.stdout.is_empty()
        })
        .unwrap_or(false)
}

fn docker_daemon_available() -> bool {
    Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
