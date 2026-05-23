use std::env;
use std::process::Command;
use std::time::Instant;

use serde::Serialize;

use crate::metrics::{reset_dir, scenario_dir, write_json};

const ENABLE_ENV: &str = "MVP_CONTAINER_OVERLAY_DNS_SMOKE";
const SCENARIO: &str = "container-overlay-dns-smoke";

#[derive(Debug, Serialize)]
struct ContainerOverlayDnsSmokeReport {
    scenario: &'static str,
    enabled: bool,
    skipped_reason: Option<String>,
    missing_prerequisites: Vec<String>,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir(SCENARIO);
    reset_dir(&root)?;
    let enabled = env::var(ENABLE_ENV).ok().as_deref() == Some("1");
    if !enabled {
        let report = ContainerOverlayDnsSmokeReport {
            scenario: SCENARIO,
            enabled,
            skipped_reason: Some(format!("set {ENABLE_ENV}=1 to run the privileged smoke")),
            missing_prerequisites: Vec::new(),
            elapsed_ms: started.elapsed().as_millis(),
        };
        let json = write_json(&root.join("container-overlay-dns-smoke.json"), &report)?;
        eprintln!("SKIP {SCENARIO} {json}");
        return Ok(());
    }

    let missing_prerequisites = missing_prerequisites();
    if !missing_prerequisites.is_empty() {
        let report = ContainerOverlayDnsSmokeReport {
            scenario: SCENARIO,
            enabled,
            skipped_reason: Some("host prerequisites are missing".to_string()),
            missing_prerequisites: missing_prerequisites.clone(),
            elapsed_ms: started.elapsed().as_millis(),
        };
        let _ = write_json(&root.join("container-overlay-dns-smoke.json"), &report)?;
        return Err(format!(
            "{SCENARIO} prerequisites missing: {}",
            missing_prerequisites.join(", ")
        ));
    }

    let report = ContainerOverlayDnsSmokeReport {
        scenario: SCENARIO,
        enabled,
        skipped_reason: Some(
            "two-node overlay execution is not wired yet; preflight passed".to_string(),
        ),
        missing_prerequisites,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let _ = write_json(&root.join("container-overlay-dns-smoke.json"), &report)?;
    Err(
        "container-overlay-dns-smoke preflight passed, but two-node execution is not wired yet"
            .to_string(),
    )
}

fn missing_prerequisites() -> Vec<String> {
    let mut missing = Vec::new();
    if !is_root() {
        missing.push("root privileges".to_string());
    }
    for command in ["docker", "ip", "iptables", "ployz-bpfctl"] {
        if !command_available(command) {
            missing.push(format!("command `{command}`"));
        }
    }
    if env::var_os("MVP_NODE_BIN").is_none() && !command_available("mvp-node") {
        missing.push("MVP_NODE_BIN or `mvp-node` in PATH".to_string());
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
