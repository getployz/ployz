use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use hickory_proto::op::{Message, MessageType, OpCode, Query};
use hickory_proto::rr::{Name, RData, RecordType};
use serde::{Deserialize, Serialize};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const ROLE_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const DNS_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone)]
pub(crate) struct ProductHarness {
    root: PathBuf,
    node_bin: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProductCommandRecord {
    pub program: String,
    pub args: Vec<String>,
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
pub(crate) struct ProductCommandOutput {
    pub record: ProductCommandRecord,
}

#[derive(Debug)]
pub(crate) struct ProductChild {
    name: String,
    child: Child,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ServingRoleProbe {
    pub listen_addr: SocketAddr,
    pub loaded_gateway_revision: String,
    pub loaded_dns_revision: String,
    pub freshness: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HttpProbe {
    pub addr: SocketAddr,
    pub host: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DnsProbe {
    pub addr: SocketAddr,
    pub host: String,
    pub answer: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeInstanceMetadata {
    pid: Option<u32>,
}

impl ProductHarness {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        let node_bin = resolve_node_bin()?;
        Ok(Self { root, node_bin })
    }

    pub(crate) fn node_dir(&self, node: &str) -> PathBuf {
        self.root.join("nodes").join(node)
    }

    pub(crate) fn control_socket(&self, role: &str) -> PathBuf {
        self.root.join("control").join(format!("{role}.sock"))
    }

    pub(crate) fn daemon_control_socket(&self, node: &str) -> PathBuf {
        self.root
            .join("control")
            .join(format!("daemon-{node}.sock"))
    }

    pub(crate) fn init_node(&self, node: &str) -> Result<ProductCommandOutput, String> {
        self.node_command([
            "init",
            "--state",
            self.node_dir(node)
                .to_str()
                .ok_or("node path is not UTF-8")?,
            "--island",
            "prod",
            "--node-id",
            node,
        ])
    }

    pub(crate) fn invite(&self, node: &str) -> Result<String, String> {
        let output = self.node_command([
            "invite",
            "--state",
            self.node_dir(node)
                .to_str()
                .ok_or("node path is not UTF-8")?,
            "--ttl-ms",
            "60000",
        ])?;
        Ok(output.record.stdout.trim().to_string())
    }

    pub(crate) fn join(&self, node: &str, token: &str) -> Result<ProductCommandOutput, String> {
        self.node_command([
            "join",
            "--state",
            self.node_dir(node)
                .to_str()
                .ok_or("node path is not UTF-8")?,
            "--token",
            token,
            "--node-id",
            node,
        ])
    }

    pub(crate) fn admission(&self, node: &str) -> Result<String, String> {
        let output = self.node_command([
            "admission",
            "--state",
            self.node_dir(node)
                .to_str()
                .ok_or("node path is not UTF-8")?,
        ])?;
        Ok(output.record.stdout.trim().to_string())
    }

    pub(crate) fn admit(
        &self,
        founder: &str,
        request: &str,
    ) -> Result<ProductCommandOutput, String> {
        self.node_command([
            "admit",
            "--state",
            self.node_dir(founder)
                .to_str()
                .ok_or("founder path is not UTF-8")?,
            "--request",
            request,
        ])
    }

    pub(crate) fn run_daemon_once(
        &self,
        node: &str,
        run_for_ms: u64,
    ) -> Result<ProductCommandOutput, String> {
        self.node_command([
            "daemon",
            "--state",
            self.node_dir(node)
                .to_str()
                .ok_or("node path is not UTF-8")?,
            "--run-for-ms",
            &run_for_ms.to_string(),
        ])
    }

    pub(crate) fn spawn_daemon(&self, node: &str, run_for_ms: u64) -> Result<ProductChild, String> {
        let control_socket = self.daemon_control_socket(node);
        self.spawn_node(
            format!("daemon-{node}"),
            [
                "daemon".to_string(),
                "--state".to_string(),
                self.node_dir(node).display().to_string(),
                "--run-for-ms".to_string(),
                run_for_ms.to_string(),
                "--control".to_string(),
                control_socket.display().to_string(),
            ],
        )
    }

    pub(crate) fn wait_daemon_status(&self, node: &str) -> Result<ProductCommandRecord, String> {
        let socket = self.daemon_control_socket(node);
        let deadline = Instant::now() + ROLE_WAIT_TIMEOUT;
        loop {
            match self.node_command([
                "daemon-status",
                "--control",
                socket.to_str().ok_or("daemon control path is not UTF-8")?,
            ]) {
                Ok(output) if output.record.stdout.contains("node_agent_handlers=6") => {
                    return Ok(output.record);
                }
                Ok(output) => {
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "daemon {node} status did not become ready: {}",
                            output.record.stdout
                        ));
                    }
                }
                Err(error) => {
                    if Instant::now() >= deadline {
                        return Err(format!("daemon {node} did not become ready: {error}"));
                    }
                    thread::sleep(POLL);
                }
            }
        }
    }

    pub(crate) fn deploy(
        &self,
        node: &str,
        target_node: &str,
        revision: &str,
        hostname: &str,
    ) -> Result<ProductCommandOutput, String> {
        self.node_command([
            "deploy",
            "--state",
            self.node_dir(node)
                .to_str()
                .ok_or("node path is not UTF-8")?,
            "--deploy-id",
            "deploy-smoke",
            "--target-node",
            target_node,
            "--service",
            "web",
            "--revision",
            revision,
            "--hostname",
            hostname,
        ])
    }

    pub(crate) fn spawn_gateway(&self, node: &str) -> Result<ProductChild, String> {
        self.spawn_serving_role("gateway", node, self.control_socket("gateway"))
    }

    pub(crate) fn spawn_dns(&self, node: &str) -> Result<ProductChild, String> {
        self.spawn_serving_role("dns", node, self.control_socket("dns"))
    }

    pub(crate) fn wait_role(&self, socket: &Path) -> Result<ServingRoleProbe, String> {
        let deadline = Instant::now() + ROLE_WAIT_TIMEOUT;
        loop {
            match self.role_request(socket, "readiness") {
                Ok(probe) => return Ok(probe),
                Err(error) => {
                    if Instant::now() >= deadline {
                        return Err(format!("serving role did not become ready: {error}"));
                    }
                    thread::sleep(POLL);
                }
            }
        }
    }

    pub(crate) fn role_request(
        &self,
        socket: &Path,
        request: &str,
    ) -> Result<ServingRoleProbe, String> {
        let value = unix_json_request(socket, &serde_json::Value::String(request.to_string()))?;
        parse_role_probe(value)
    }

    pub(crate) fn shutdown_role(&self, socket: &Path) -> Result<(), String> {
        let response =
            unix_json_request(socket, &serde_json::Value::String("shutdown".to_string()))?;
        if response
            .pointer("/status")
            .and_then(serde_json::Value::as_str)
            == Some("success")
            && response
                .pointer("/data/event")
                .and_then(serde_json::Value::as_str)
                == Some("shutdown")
        {
            return Ok(());
        }
        Err(format!("serving role shutdown failed: {response}"))
    }

    pub(crate) fn wait_http(
        &self,
        addr: SocketAddr,
        host: &str,
        expected: &str,
    ) -> Result<HttpProbe, String> {
        let deadline = Instant::now() + HTTP_WAIT_TIMEOUT;
        loop {
            match http_get(addr, host) {
                Ok(body) if body.contains(expected) => {
                    return Ok(HttpProbe {
                        addr,
                        host: host.to_string(),
                        body,
                    });
                }
                Ok(body) => {
                    if Instant::now() >= deadline {
                        return Err(format!("HTTP body did not contain '{expected}': {body}"));
                    }
                }
                Err(error) => {
                    if Instant::now() >= deadline {
                        return Err(error);
                    }
                }
            }
            thread::sleep(POLL);
        }
    }

    pub(crate) fn wait_dns(
        &self,
        addr: SocketAddr,
        host: &str,
        expected: &str,
    ) -> Result<DnsProbe, String> {
        let deadline = Instant::now() + DNS_WAIT_TIMEOUT;
        loop {
            match dns_a_lookup(addr, host) {
                Ok(answer) if answer == expected => {
                    return Ok(DnsProbe {
                        addr,
                        host: host.to_string(),
                        answer,
                    });
                }
                Ok(answer) => {
                    if Instant::now() >= deadline {
                        return Err(format!("DNS answer was '{answer}', expected '{expected}'"));
                    }
                }
                Err(error) => {
                    if Instant::now() >= deadline {
                        return Err(error);
                    }
                }
            }
            thread::sleep(POLL);
        }
    }

    pub(crate) fn cleanup_runtime_processes(&self, nodes: &[&str]) -> Result<usize, String> {
        let mut killed = 0;
        for node in nodes {
            let instances_dir = self.node_dir(node).join("runtime").join("instances");
            let Ok(entries) = fs::read_dir(&instances_dir) else {
                continue;
            };
            for entry in entries {
                let entry =
                    entry.map_err(|error| format!("read runtime entry '{}': {error}", node))?;
                let metadata_path = entry.path().join("instance.json");
                let Ok(bytes) = fs::read(&metadata_path) else {
                    continue;
                };
                let metadata: RuntimeInstanceMetadata = serde_json::from_slice(&bytes)
                    .map_err(|error| format!("decode '{}': {error}", metadata_path.display()))?;
                let Some(pid) = metadata.pid else {
                    continue;
                };
                let _ = Command::new("kill")
                    .arg("-TERM")
                    .arg(pid.to_string())
                    .status();
                killed += 1;
            }
        }
        Ok(killed)
    }

    fn spawn_serving_role(
        &self,
        role: &str,
        node: &str,
        socket: PathBuf,
    ) -> Result<ProductChild, String> {
        self.spawn_node(
            format!("{role}-{node}"),
            [
                role.to_string(),
                "--state".to_string(),
                self.node_dir(node).display().to_string(),
                "--listen".to_string(),
                "127.0.0.1:0".to_string(),
                "--control".to_string(),
                socket.display().to_string(),
            ],
        )
    }

    fn spawn_node<I>(&self, name: String, args: I) -> Result<ProductChild, String>
    where
        I: IntoIterator<Item = String>,
    {
        let args = args.into_iter().collect::<Vec<_>>();
        let redacted_args = redact_args(args.clone());
        let child = Command::new(&self.node_bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("spawn {name}: {error}; args={redacted_args:?}"))?;
        Ok(ProductChild { name, child })
    }

    fn node_command<I, S>(&self, args: I) -> Result<ProductCommandOutput, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let args = args
            .into_iter()
            .map(|arg| arg.as_ref().to_string())
            .collect::<Vec<_>>();
        let redacted_args = redact_args(args.clone());
        let mut child = Command::new(&self.node_bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("spawn mvp-node: {error}; args={redacted_args:?}"))?;
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    let output = child
                        .wait_with_output()
                        .map_err(|error| format!("collect mvp-node output: {error}"))?;
                    return command_output(self.node_bin.as_path(), args, output);
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!(
                            "mvp-node command timed out; args={redacted_args:?}"
                        ));
                    }
                }
                Err(error) => {
                    return Err(format!(
                        "poll mvp-node command: {error}; args={redacted_args:?}"
                    ));
                }
            }
            thread::sleep(POLL);
        }
    }
}

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

fn command_output(
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

fn redact_args(args: Vec<String>) -> Vec<String> {
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

fn resolve_node_bin() -> Result<PathBuf, String> {
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

fn unix_json_request(
    socket: &Path,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let bytes = serde_json::to_vec(request).map_err(|error| format!("encode request: {error}"))?;
    let mut stream = connect_unix(socket)?;
    stream
        .write_all(&bytes)
        .map_err(|error| format!("write '{}': {error}", socket.display()))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| format!("shutdown write '{}': {error}", socket.display()))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| format!("read '{}': {error}", socket.display()))?;
    serde_json::from_slice(&response).map_err(|error| {
        format!(
            "decode response from '{}': {error}; body={}",
            socket.display(),
            String::from_utf8_lossy(&response)
        )
    })
}

fn connect_unix(socket: &Path) -> Result<UnixStream, String> {
    let deadline = Instant::now() + ROLE_WAIT_TIMEOUT;
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(format!("connect '{}': {error}", socket.display()));
                }
                thread::sleep(POLL);
            }
        }
    }
}

fn parse_role_probe(response: serde_json::Value) -> Result<ServingRoleProbe, String> {
    if response
        .pointer("/status")
        .and_then(serde_json::Value::as_str)
        != Some("success")
    {
        return Err(format!("role request failed: {response}"));
    }
    let event = response
        .pointer("/data/event")
        .and_then(serde_json::Value::as_str);
    if !matches!(event, Some("status" | "reloaded")) {
        return Err(format!("unexpected role response: {response}"));
    }
    let listen_addr = response
        .pointer("/data/listen_addr")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("role response missing listen_addr: {response}"))?
        .parse::<SocketAddr>()
        .map_err(|error| format!("parse role listen_addr: {error}; response={response}"))?;
    let loaded_gateway_revision = response
        .pointer("/data/serving/loaded_revisions/gateway")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("role response missing gateway revision: {response}"))?
        .to_string();
    let loaded_dns_revision = response
        .pointer("/data/serving/loaded_revisions/dns")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("role response missing dns revision: {response}"))?
        .to_string();
    let freshness = response
        .pointer("/data/serving/freshness")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("role response missing freshness: {response}"))?
        .to_string();
    Ok(ServingRoleProbe {
        listen_addr,
        loaded_gateway_revision,
        loaded_dns_revision,
        freshness,
    })
}

fn http_get(addr: SocketAddr, host: &str) -> Result<String, String> {
    let mut stream =
        TcpStream::connect(addr).map_err(|error| format!("connect HTTP {addr}: {error}"))?;
    stream
        .write_all(format!("GET / HTTP/1.1\r\nhost: {host}\r\n\r\n").as_bytes())
        .map_err(|error| format!("write HTTP {addr}: {error}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("read HTTP {addr}: {error}"))?;
    response
        .split_once("\r\n\r\n")
        .map(|(_headers, body)| body.to_string())
        .ok_or_else(|| format!("HTTP response missing body separator: {response}"))
}

fn dns_a_lookup(addr: SocketAddr, host: &str) -> Result<String, String> {
    let name = Name::from_ascii(format!("{host}."))
        .map_err(|error| format!("parse DNS query name '{host}': {error}"))?;
    let mut request = Message::new(9, MessageType::Query, OpCode::Query);
    request.add_query(Query::query(name, RecordType::A));
    let request = request
        .to_vec()
        .map_err(|error| format!("encode DNS query: {error}"))?;
    let socket =
        UdpSocket::bind("127.0.0.1:0").map_err(|error| format!("bind DNS client: {error}"))?;
    socket
        .set_read_timeout(Some(DNS_WAIT_TIMEOUT))
        .map_err(|error| format!("set DNS timeout: {error}"))?;
    socket
        .send_to(&request, addr)
        .map_err(|error| format!("send DNS query to {addr}: {error}"))?;
    let mut packet = [0_u8; 1232];
    let (len, _) = socket
        .recv_from(&mut packet)
        .map_err(|error| format!("receive DNS response from {addr}: {error}"))?;
    let response = Message::from_vec(&packet[..len])
        .map_err(|error| format!("decode DNS response: {error}"))?;
    response
        .answers
        .iter()
        .find_map(|record| match &record.data {
            RData::A(address) => Some(address.to_string()),
            _ => None,
        })
        .ok_or_else(|| format!("DNS response contained no A answer: {response:?}"))
}
