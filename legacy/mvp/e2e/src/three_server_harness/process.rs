use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::{POLL, ProductChild, ProductCommandOutput, ProductCommandRecord};

impl ProductChild {
    pub(crate) fn wait(&mut self) -> Result<ExitStatus, String> {
        self.child
            .wait()
            .map_err(|error| format!("wait {}: {error}", self.name))
    }

    pub(crate) fn kill(&mut self) -> Result<ExitStatus, String> {
        if self
            .child
            .try_wait()
            .map_err(|error| format!("poll {}: {error}", self.name))?
            .is_none()
        {
            self.child
                .kill()
                .map_err(|error| format!("kill {}: {error}", self.name))?;
        }
        self.child
            .wait()
            .map_err(|error| format!("wait {}: {error}", self.name))
    }
}

impl Drop for ProductChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub(super) fn build_node_bin_if_needed() -> Result<(), String> {
    if std::env::var_os("MVP_NODE_BIN").is_some() {
        return Ok(());
    }
    let current =
        std::env::current_exe().map_err(|error| format!("resolve current executable: {error}"))?;
    let manifest = current
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|root| root.join("Cargo.toml"))
        .ok_or_else(|| format!("resolve MVP manifest from '{}'", current.display()))?;
    let status = Command::new("cargo")
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("-p")
        .arg("mvp-node")
        .arg("--features")
        .arg("docker-runtime,linux-wireguard")
        .status()
        .map_err(|error| format!("spawn cargo build for mvp-node: {error}"))?;
    if status.success() {
        return Ok(());
    }
    Err(format!(
        "cargo build for mvp-node failed with status {status}; manifest={}",
        manifest.display()
    ))
}

pub(super) fn command_output(
    program: &Path,
    args: Vec<String>,
    output: std::process::Output,
) -> Result<ProductCommandOutput, String> {
    let record = ProductCommandRecord {
        program: program.display().to_string(),
        args: redact_args(args),
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };
    if output.status.success() {
        return Ok(ProductCommandOutput { record });
    }
    Err(format!(
        "command failed status={} args={:?} stdout={} stderr={}",
        record.status, record.args, record.stdout, record.stderr
    ))
}

pub(super) fn redact_args(args: Vec<String>) -> Vec<String> {
    let mut redacted = Vec::with_capacity(args.len());
    let mut redact_next = false;
    for arg in args {
        if redact_next {
            redacted.push("<redacted>".to_string());
            redact_next = false;
            continue;
        }
        redact_next = matches!(arg.as_str(), "--token" | "--request");
        redacted.push(arg);
    }
    redacted
}

pub(super) fn resolve_node_bin() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("MVP_NODE_BIN").map(PathBuf::from) {
        if path.exists() {
            return Ok(path);
        }
        return Err(format!("MVP_NODE_BIN does not exist: {}", path.display()));
    }
    let current =
        std::env::current_exe().map_err(|error| format!("resolve current executable: {error}"))?;
    let Some(target_dir) = current.parent() else {
        return Err(format!(
            "current executable has no parent: {}",
            current.display()
        ));
    };
    let candidate = target_dir.join("mvp-node");
    if candidate.exists() {
        return Ok(candidate);
    }
    Err(format!(
        "mvp-node binary not found at '{}'; run `cargo build -p mvp-node` or set MVP_NODE_BIN",
        candidate.display()
    ))
}

pub(super) fn docker_run<I, S>(args: I, timeout: Duration) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect::<Vec<_>>();
    let mut child = Command::new("docker")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn docker: {error}; args={args:?}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|error| format!("collect docker output: {error}"))?;
                if output.status.success() {
                    return String::from_utf8(output.stdout).map_err(|error| error.to_string());
                }
                return Err(format!(
                    "docker command failed status={} args={:?} stdout={} stderr={}",
                    output.status.code().unwrap_or(-1),
                    args,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("docker command timed out; args={args:?}"));
                }
            }
            Err(error) => return Err(format!("poll docker command: {error}; args={args:?}")),
        }
        thread::sleep(POLL);
    }
}

pub(super) fn free_loopback_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("bind free loopback port: {error}"))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|error| error.to_string())
}

pub(super) fn wait_tcp(addr: (&str, u16)) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!("timed out waiting for {}:{}", addr.0, addr.1))
}
