use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::PebbleAcme;
use super::process::{free_loopback_port, wait_tcp};

impl PebbleAcme {
    pub(crate) fn start(output_root: &Path) -> Result<Self, String> {
        docker_available_for_pebble()?;
        let pebble_dir = resolve_pebble_dir()?;
        let port = free_loopback_port()?;
        let management_port = free_loopback_port()?;
        let output = Command::new("docker")
            .arg("run")
            .arg("-d")
            .arg("--rm")
            .arg("-p")
            .arg(format!("127.0.0.1:{port}:14000"))
            .arg("-p")
            .arg(format!("127.0.0.1:{management_port}:15000"))
            .arg("-e")
            .arg("PEBBLE_VA_NOSLEEP=1")
            .arg("-e")
            .arg("PEBBLE_VA_ALWAYS_VALID=1")
            .arg("-v")
            .arg(format!("{}:/e2e-pebble:ro", pebble_dir.display()))
            .arg("ghcr.io/letsencrypt/pebble:latest")
            .arg("-config")
            .arg("/e2e-pebble/pebble-config.json")
            .arg("-strict=false")
            .output()
            .map_err(|error| format!("start Pebble container: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "docker run Pebble failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let directory_url = format!("https://127.0.0.1:{port}/dir");
        let root_ca_path = pebble_dir.join("pebble.minica.pem");
        wait_tcp(("127.0.0.1", port))?;
        wait_https_directory(&directory_url, &root_ca_path)?;
        let issued_root_ca_path = output_root.join("pebble-issued-root.pem");
        fetch_issued_root_ca(management_port, &root_ca_path, &issued_root_ca_path)?;
        Ok(Self {
            directory_url,
            root_ca_path,
            issued_root_ca_path,
            id,
        })
    }
}

impl Drop for PebbleAcme {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .arg("rm")
            .arg("-f")
            .arg(&self.id)
            .output();
    }
}

fn resolve_pebble_dir() -> Result<PathBuf, String> {
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    for root in [
        current.as_path(),
        current.parent().unwrap_or(current.as_path()),
    ] {
        let candidate = root.join("packaging/e2e/pebble");
        if candidate.join("pebble-config.json").exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "resolve Pebble config directory from '{}'",
        current.display()
    ))
}

fn docker_available_for_pebble() -> Result<(), String> {
    let status = Command::new("docker")
        .arg("version")
        .arg("--format")
        .arg("{{.Server.Version}}")
        .status()
        .map_err(|error| format!("docker is required for Pebble: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("docker is required for Pebble and is not available".to_string())
    }
}

fn wait_https_directory(url: &str, root_ca: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let status = Command::new("curl")
            .arg("--silent")
            .arg("--fail")
            .arg("--cacert")
            .arg(root_ca)
            .arg(url)
            .status();
        if status.is_ok_and(|status| status.success()) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!("timed out waiting for Pebble directory {url}"))
}

fn fetch_issued_root_ca(
    management_port: u16,
    server_root_ca: &Path,
    output_path: &Path,
) -> Result<(), String> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let status = Command::new("curl")
        .arg("--silent")
        .arg("--fail")
        .arg("--cacert")
        .arg(server_root_ca)
        .arg(format!("https://127.0.0.1:{management_port}/roots/0"))
        .arg("--output")
        .arg(output_path)
        .status()
        .map_err(|error| format!("fetch Pebble issued root CA: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("fetch Pebble issued root CA failed: {status}"))
    }
}
