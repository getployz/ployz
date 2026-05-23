use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

mod control;
mod pebble;
mod probes;
mod process;

use self::control::unix_json_request;
use self::probes::{curl_https, dns_a_lookup, http_get, parse_role_probe};
use self::process::{
    build_node_bin_if_needed, command_output, docker_run, redact_args, resolve_node_bin,
};

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
                "--linux-wireguard-listen-port".to_string(),
                wireguard_listen_port(node).to_string(),
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
        let lookup_command = format!("nslookup {host} || true");
        let nslookup = docker_run(
            [
                "run",
                "--rm",
                "--network",
                network,
                "--dns",
                dns_server,
                "busybox:latest",
                "sh",
                "-c",
                lookup_command.as_str(),
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
            let _ = Command::new("ip")
                .args(["link", "delete", "dev", &format!("ployz-{node}")])
                .status();
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

fn wireguard_listen_port(node: &str) -> u16 {
    match node {
        "node-a" => 51820,
        "node-b" => 51821,
        "node-c" => 51822,
        _ => 51820,
    }
}
