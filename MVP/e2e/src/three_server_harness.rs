use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use hickory_proto::op::{Message, MessageType, OpCode, Query};
use hickory_proto::rr::{Name, RData, RecordType};
use serde::{Deserialize, Serialize};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
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
    pub tls_listen_addr: Option<SocketAddr>,
    pub loaded_generation: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ContainerDnsClientProbe {
    pub network: String,
    pub dns_server: String,
    pub host: String,
    pub nslookup_stdout: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HttpsProbe {
    pub addr: SocketAddr,
    pub host: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PebbleAcme {
    pub directory_url: String,
    pub root_ca_path: PathBuf,
    pub issued_root_ca_path: PathBuf,
    id: String,
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

    pub(crate) fn install(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        build_node_bin_if_needed()?;
        let source = resolve_node_bin()?;
        let bin_dir = root.join("install").join("bin");
        fs::create_dir_all(&bin_dir)
            .map_err(|error| format!("create install bin dir '{}': {error}", bin_dir.display()))?;
        let node_bin = bin_dir.join("mvp-node");
        fs::copy(&source, &node_bin).map_err(|error| {
            format!(
                "install mvp-node from '{}' to '{}': {error}",
                source.display(),
                node_bin.display()
            )
        })?;
        let mut permissions = fs::metadata(&node_bin)
            .map_err(|error| format!("stat installed mvp-node '{}': {error}", node_bin.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&node_bin, permissions).map_err(|error| {
            format!("chmod installed mvp-node '{}': {error}", node_bin.display())
        })?;
        Ok(Self { root, node_bin })
    }

    pub(crate) fn node_bin(&self) -> &Path {
        &self.node_bin
    }

    pub(crate) fn node_dir(&self, node: &str) -> PathBuf {
        self.root.join("nodes").join(node)
    }

    pub(crate) fn control_socket(&self, role: &str) -> PathBuf {
        self.root.join("control").join(format!("{role}.sock"))
    }

    pub(crate) fn role_control_socket(&self, node: &str, role: &str) -> PathBuf {
        self.root
            .join("control")
            .join(format!("{role}-{node}.sock"))
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

    pub(crate) fn bootstrap_node(&self, node: &str) -> Result<ProductCommandOutput, String> {
        self.node_command([
            "bootstrap",
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

    pub(crate) fn help(&self) -> Result<ProductCommandOutput, String> {
        self.node_command(["--help"])
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

    pub(crate) fn spawn_docker_daemon(
        &self,
        node: &str,
        run_for_ms: u64,
        image: &str,
        service_port: u16,
        command: &str,
    ) -> Result<ProductChild, String> {
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
                "--linux-wireguard-ifname".to_string(),
                format!("ployz-{node}"),
                "--runtime".to_string(),
                "docker".to_string(),
                "--image".to_string(),
                image.to_string(),
                "--service-port".to_string(),
                service_port.to_string(),
                "--container-command".to_string(),
                command.to_string(),
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
                Ok(output)
                    if output.record.stdout.contains("node_agent_handlers=6")
                        || output.record.stdout.contains("\"node_agent_handlers\":6") =>
                {
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

    pub(crate) fn deploy_via_daemon(
        &self,
        coordinator_node: &str,
        target_node: &str,
        revision: &str,
        hostname: &str,
    ) -> Result<ProductCommandOutput, String> {
        self.deploy_via_daemon_with_id(
            coordinator_node,
            "deploy-smoke",
            target_node,
            revision,
            hostname,
        )
    }

    pub(crate) fn deploy_via_daemon_with_id(
        &self,
        coordinator_node: &str,
        deploy_id: &str,
        target_node: &str,
        revision: &str,
        hostname: &str,
    ) -> Result<ProductCommandOutput, String> {
        let socket = self.daemon_control_socket(coordinator_node);
        self.node_command([
            "deploy",
            "--control",
            socket.to_str().ok_or("daemon control path is not UTF-8")?,
            "--deploy-id",
            deploy_id,
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

    pub(crate) fn deploy_service_via_daemon(
        &self,
        coordinator_node: &str,
        deploy_id: &str,
        target_node: &str,
        service: &str,
        revision: &str,
        hostname: &str,
    ) -> Result<ProductCommandOutput, String> {
        let socket = self.daemon_control_socket(coordinator_node);
        self.node_command([
            "deploy",
            "--control",
            socket.to_str().ok_or("daemon control path is not UTF-8")?,
            "--deploy-id",
            deploy_id,
            "--target-node",
            target_node,
            "--service",
            service,
            "--revision",
            revision,
            "--hostname",
            hostname,
        ])
    }

    pub(crate) fn deploy_status(
        &self,
        node: &str,
        deploy_id: &str,
    ) -> Result<ProductCommandOutput, String> {
        self.node_command([
            "deploy-status",
            "--state",
            self.node_dir(node)
                .to_str()
                .ok_or("node path is not UTF-8")?,
            "--deploy-id",
            deploy_id,
        ])
    }

    pub(crate) fn spawn_gateway(&self, node: &str) -> Result<ProductChild, String> {
        self.spawn_serving_role("gateway", node, self.control_socket("gateway"))
    }

    pub(crate) fn spawn_dns(&self, node: &str) -> Result<ProductChild, String> {
        self.spawn_serving_role("dns", node, self.control_socket("dns"))
    }

    pub(crate) fn spawn_node_gateway(&self, node: &str) -> Result<ProductChild, String> {
        self.spawn_serving_role("gateway", node, self.role_control_socket(node, "gateway"))
    }

    pub(crate) fn spawn_node_gateway_tls(&self, node: &str) -> Result<ProductChild, String> {
        self.spawn_node(
            format!("gateway-{node}"),
            [
                "gateway".to_string(),
                "--state".to_string(),
                self.node_dir(node).display().to_string(),
                "--listen".to_string(),
                "127.0.0.1:0".to_string(),
                "--tls-listen".to_string(),
                "127.0.0.1:0".to_string(),
                "--control".to_string(),
                self.role_control_socket(node, "gateway")
                    .display()
                    .to_string(),
            ],
        )
    }

    pub(crate) fn spawn_node_dns(&self, node: &str) -> Result<ProductChild, String> {
        self.spawn_serving_role("dns", node, self.role_control_socket(node, "dns"))
    }

    pub(crate) fn spawn_node_dns_at(
        &self,
        node: &str,
        listen: SocketAddr,
    ) -> Result<ProductChild, String> {
        self.spawn_node(
            format!("dns-{node}"),
            [
                "dns".to_string(),
                "--state".to_string(),
                self.node_dir(node).display().to_string(),
                "--listen".to_string(),
                listen.to_string(),
                "--control".to_string(),
                self.role_control_socket(node, "dns").display().to_string(),
            ],
        )
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

    pub(crate) fn reload_role(&self, socket: &Path) -> Result<ServingRoleProbe, String> {
        self.role_request(socket, "reload")
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

    pub(crate) fn wait_https(
        &self,
        addr: SocketAddr,
        host: &str,
        root_ca: &Path,
        expected: &str,
    ) -> Result<HttpsProbe, String> {
        let deadline = Instant::now() + HTTP_WAIT_TIMEOUT;
        loop {
            match curl_https(addr, host, root_ca) {
                Ok(body) if body.contains(expected) => {
                    return Ok(HttpsProbe {
                        addr,
                        host: host.to_string(),
                        body,
                    });
                }
                Ok(body) => {
                    if Instant::now() >= deadline {
                        return Err(format!("HTTPS body did not contain '{expected}': {body}"));
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

    pub(crate) fn run_container_dns_client(
        &self,
        network: &str,
        dns_server: &str,
        host: &str,
        url: &str,
    ) -> Result<ContainerDnsClientProbe, String> {
        let nslookup = docker_run(
            [
                "run",
                "--rm",
                "--network",
                network,
                "--dns",
                dns_server,
                "busybox:latest",
                "nslookup",
                host,
            ],
            COMMAND_TIMEOUT,
        )?;
        let body = docker_run(
            [
                "run",
                "--rm",
                "--network",
                network,
                "--dns",
                dns_server,
                "busybox:latest",
                "wget",
                "-qO-",
                url,
            ],
            COMMAND_TIMEOUT,
        )?;
        Ok(ContainerDnsClientProbe {
            network: network.to_string(),
            dns_server: dns_server.to_string(),
            host: host.to_string(),
            nslookup_stdout: nslookup,
            body,
        })
    }

    pub(crate) fn acme_issue_with_pebble(
        &self,
        node: &str,
        hostname: &str,
        gateway: SocketAddr,
        gateway_control: &Path,
        pebble: &PebbleAcme,
    ) -> Result<ProductCommandOutput, String> {
        self.node_command_with_env(
            [
                ("PLOYZ_ACME_DIRECTORY_URL", pebble.directory_url.as_str()),
                (
                    "PLOYZ_ACME_ROOT_CA_PATH",
                    pebble
                        .root_ca_path
                        .to_str()
                        .ok_or("Pebble root CA path is not UTF-8")?,
                ),
            ],
            [
                "acme-issue".to_string(),
                "--state".to_string(),
                self.node_dir(node).display().to_string(),
                "--hostname".to_string(),
                hostname.to_string(),
                "--gateway".to_string(),
                format!("http://{gateway}"),
                "--gateway-control".to_string(),
                gateway_control.display().to_string(),
            ],
        )
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

    pub(crate) fn cleanup_docker_runtime(&self, nodes: &[&str]) -> Result<(), String> {
        for node in nodes {
            let output = Command::new("docker")
                .args([
                    "ps",
                    "-aq",
                    "--filter",
                    "label=dev.ployz.mvp.managed=true",
                    "--filter",
                    &format!("label=dev.ployz.mvp.node={node}"),
                ])
                .output()
                .map_err(|error| format!("list Docker containers for {node}: {error}"))?;
            if output.status.success() {
                let containers = String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                if !containers.is_empty() {
                    let status = Command::new("docker")
                        .arg("rm")
                        .arg("-f")
                        .args(&containers)
                        .status()
                        .map_err(|error| format!("remove Docker containers for {node}: {error}"))?;
                    if !status.success() {
                        return Err(format!("docker rm failed for {node} with status {status}"));
                    }
                }
            }
            let _ = Command::new("docker")
                .args(["network", "rm", &format!("ployz-mvp-{node}")])
                .status();
        }
        Ok(())
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
        self.node_command_with_env(std::iter::empty::<(&str, &str)>(), args)
    }

    fn node_command_with_env<I, S, E, K, V>(
        &self,
        envs: E,
        args: I,
    ) -> Result<ProductCommandOutput, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let args = args
            .into_iter()
            .map(|arg| arg.as_ref().to_string())
            .collect::<Vec<_>>();
        let redacted_args = redact_args(args.clone());
        let mut command = Command::new(&self.node_bin);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in envs {
            command.env(key.as_ref(), value.as_ref());
        }
        let mut child = command
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

impl PebbleAcme {
    pub(crate) fn start(output_root: &Path) -> Result<Self, String> {
        docker_available_for_pebble()?;
        let repo = std::env::current_dir().map_err(|error| error.to_string())?;
        let pebble_dir = repo.join("packaging/e2e/pebble");
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

fn build_node_bin_if_needed() -> Result<(), String> {
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
    let tls_listen_addr = response
        .pointer("/data/tls_listen_addr")
        .and_then(serde_json::Value::as_str)
        .map(|value| {
            value.parse::<SocketAddr>().map_err(|error| {
                format!("parse role tls_listen_addr: {error}; response={response}")
            })
        })
        .transpose()?;
    let loaded_generation = response
        .pointer("/data/serving/loaded_revisions/generation")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("role response missing generation: {response}"))?
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
        tls_listen_addr,
        loaded_generation,
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

fn curl_https(addr: SocketAddr, host: &str, root_ca: &Path) -> Result<String, String> {
    let output = Command::new("curl")
        .arg("--silent")
        .arg("--show-error")
        .arg("--fail")
        .arg("--cacert")
        .arg(root_ca)
        .arg("--resolve")
        .arg(format!("{host}:{}:127.0.0.1", addr.port()))
        .arg("--header")
        .arg(format!("Host: {host}"))
        .arg(format!("https://{host}:{}/", addr.port()))
        .output()
        .map_err(|error| format!("run curl HTTPS check: {error}"))?;
    if output.status.success() {
        return String::from_utf8(output.stdout).map_err(|error| error.to_string());
    }
    Err(format!(
        "curl HTTPS check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn docker_run<I, S>(args: I, timeout: Duration) -> Result<String, String>
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

fn free_loopback_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("bind free loopback port: {error}"))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|error| error.to_string())
}

fn wait_tcp(addr: (&str, u16)) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!("timed out waiting for {}:{}", addr.0, addr.1))
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
