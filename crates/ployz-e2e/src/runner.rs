use crate::cli::Scenario;
use crate::error::{Error, Result};
use crate::scenarios;
use crate::support::{
    CommandOutput, docker_outer, docker_outer_raw, parse_ready, pick_free_port, run_command,
    run_command_expect_ok, wait_until,
};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const SSH_WAIT_TIMEOUT: Duration = Duration::from_secs(90);
const DAEMON_WAIT_TIMEOUT: Duration = Duration::from_secs(90);
const READY_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const STATE_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const CONTAINER_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub(crate) name: String,
    container_name: String,
    ssh_port: u16,
    pub(crate) outer_ip: String,
}

#[derive(Debug)]
pub(crate) struct ScenarioRun {
    scenario: Scenario,
    image: String,
    root_dir: PathBuf,
    payload_dir: PathBuf,
    outer_network: String,
    private_key_path: PathBuf,
    public_key_path: PathBuf,
    public_key: String,
    keep_failed: bool,
    nodes: Vec<Node>,
}

impl ScenarioRun {
    pub(crate) fn new(
        scenario: Scenario,
        image: &str,
        artifacts_root: &Path,
        keep_failed: bool,
    ) -> Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::Io(format!("system clock before unix epoch: {error}")))?
            .as_secs();
        let run_id = format!("{}-{timestamp}-{}", scenario.as_str(), Uuid::new_v4());
        let root_dir = artifacts_root.join(&run_id);
        let key_dir = root_dir.join("keys");
        let payload_dir = artifacts_root.join("payload-cache");

        fs::create_dir_all(&key_dir).map_err(|error| {
            Error::Io(format!("create key dir '{}': {error}", key_dir.display()))
        })?;

        let private_key_path = key_dir.join("id_ed25519");
        let public_key_path = key_dir.join("id_ed25519.pub");
        let run = Self {
            scenario,
            image: image.to_string(),
            root_dir,
            payload_dir,
            outer_network: format!("ployz-e2e-net-{run_id}"),
            private_key_path,
            public_key_path,
            public_key: String::new(),
            keep_failed,
            nodes: Vec::new(),
        };
        run.ensure_payload()?;
        run.write_metadata()?;
        Ok(run)
    }

    pub(crate) fn execute(&mut self) -> Result<()> {
        self.generate_ssh_keypair()?;
        self.create_outer_network()?;
        self.start_nodes(self.scenario.node_names())?;
        scenarios::run(self)
    }

    pub(crate) fn cleanup(&self, failed: bool) {
        if failed && self.keep_failed {
            return;
        }

        for node in &self.nodes {
            let _ = docker_outer(["rm", "-f", node.container_name.as_str()]);
        }
        let _ = docker_outer(["network", "rm", self.outer_network.as_str()]);
    }

    pub(crate) fn collect_failure_artifacts(&self) -> Result<()> {
        let logs_dir = self.root_dir.join("logs");
        fs::create_dir_all(&logs_dir).map_err(|error| {
            Error::Io(format!("create logs dir '{}': {error}", logs_dir.display()))
        })?;

        for node in &self.nodes {
            let container_logs =
                docker_outer_raw(["logs", node.container_name.as_str()]).unwrap_or_default();
            fs::write(
                logs_dir.join(format!("{}-container.log", node.name)),
                container_logs.stdout,
            )
            .map_err(|error| {
                Error::Io(format!(
                    "write node log '{}': {error}",
                    logs_dir
                        .join(format!("{}-container.log", node.name))
                        .display()
                ))
            })?;

            let dockerd_log = self
                .ssh_run(node, "cat /var/log/dockerd.log")
                .unwrap_or_else(|_| CommandOutput::default());
            fs::write(
                logs_dir.join(format!("{}-dockerd.log", node.name)),
                dockerd_log.stdout,
            )
            .map_err(|error| {
                Error::Io(format!(
                    "write dockerd log '{}': {error}",
                    logs_dir
                        .join(format!("{}-dockerd.log", node.name))
                        .display()
                ))
            })?;

            let inner_docker_ps = self
                .ssh_run(node, "docker ps -a --format '{{.ID}} {{.Names}} {{.Status}}'")
                .unwrap_or_else(|_| CommandOutput::default());
            fs::write(
                logs_dir.join(format!("{}-inner-docker-ps.txt", node.name)),
                inner_docker_ps.stdout,
            )
            .map_err(|error| {
                Error::Io(format!(
                    "write inner docker ps '{}': {error}",
                    logs_dir
                        .join(format!("{}-inner-docker-ps.txt", node.name))
                        .display()
                ))
            })?;

            let status = self
                .ssh_run(node, "ployzd --json status")
                .unwrap_or_else(|_| CommandOutput::default());
            fs::write(
                logs_dir.join(format!("{}-status.json", node.name)),
                status.stdout,
            )
            .map_err(|error| {
                Error::Io(format!(
                    "write node status '{}': {error}",
                    logs_dir
                        .join(format!("{}-status.json", node.name))
                        .display()
                ))
            })?;

            let machine_ls = self
                .ssh_run(node, "ployzd machine ls")
                .unwrap_or_else(|_| CommandOutput::default());
            fs::write(
                logs_dir.join(format!("{}-machine-ls.txt", node.name)),
                machine_ls.stdout,
            )
            .map_err(|error| {
                Error::Io(format!(
                    "write machine ls '{}': {error}",
                    logs_dir
                        .join(format!("{}-machine-ls.txt", node.name))
                        .display()
                ))
            })?;

            let copy_target = self.root_dir.join(format!("{}-var-lib-ployz", node.name));
            let destination = copy_target.to_string_lossy().into_owned();
            let source = format!("{}:/var/lib/ployz", node.container_name);
            let _ = docker_outer_raw(["cp", source.as_str(), destination.as_str()]);
        }

        Ok(())
    }

    #[must_use]
    pub(crate) fn scenario(&self) -> Scenario {
        self.scenario
    }

    pub(crate) fn node(&self, name: &str) -> Result<&Node> {
        self.nodes
            .iter()
            .find(|node| node.name == name)
            .ok_or_else(|| Error::Message(format!("node '{name}' is not running")))
    }

    pub(crate) fn setup_founder_and_joiner(&self) -> Result<()> {
        self.mesh_init_founder()?;
        self.wait_mesh_ready_default(self.node("founder")?)?;
        let founder = self.node("founder")?;
        let joiner = self.node("joiner")?;
        let add_joiner = format!(
            "ployzd machine add --identity /e2e-keys/id_ed25519 root@{}",
            joiner.outer_ip
        );
        self.ssh_expect_ok(founder, &add_joiner)?;
        self.wait_machine_state_default(founder, "joiner", "disabled")?;
        self.wait_machine_state_default(founder, "joiner", "enabled")
    }

    pub(crate) fn mesh_init_founder(&self) -> Result<()> {
        let founder = self.node("founder")?;
        self.ssh_expect_ok(founder, "ployzd mesh init alpha")?;
        Ok(())
    }

    pub(crate) fn wait_mesh_ready_default(&self, node: &Node) -> Result<()> {
        self.wait_mesh_ready(node, READY_WAIT_TIMEOUT)
    }

    pub(crate) fn wait_machine_state_default(
        &self,
        node: &Node,
        machine_id: &str,
        expected_state: &str,
    ) -> Result<()> {
        self.wait_machine_state(node, machine_id, expected_state, STATE_WAIT_TIMEOUT)
    }

    pub(crate) fn wait_machine_absent_default(
        &self,
        node: &Node,
        machine_id: &str,
    ) -> Result<()> {
        self.wait_machine_absent(node, machine_id, STATE_WAIT_TIMEOUT)
    }

    pub(crate) fn wait_service_container(
        &self,
        node: &Node,
        namespace: &str,
        service: &str,
    ) -> Result<()> {
        wait_until(CONTAINER_WAIT_TIMEOUT, || {
            let output = self.ssh_run(
                node,
                &format!(
                    "docker ps -a --filter label=dev.ployz.namespace={namespace} --filter label=dev.ployz.service={service} --format '{{{{.Names}}}}'"
                ),
            )?;
            let names: Vec<&str> = output
                .stdout
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect();
            Ok(!names.is_empty())
        })
        .map_err(|error| {
            Error::Message(format!(
                "service '{service}' in namespace '{namespace}' did not create a workload: {error}"
            ))
        })
    }

    pub(crate) fn ssh_expect_ok(&self, node: &Node, script: &str) -> Result<CommandOutput> {
        let output = self.ssh_run(node, script)?;
        if output.status.success() {
            return Ok(output);
        }
        Err(Error::CommandFailed {
            command: format!("ssh {} -> {script}", node.name),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    pub(crate) fn ssh_run(&self, node: &Node, script: &str) -> Result<CommandOutput> {
        let target = "root@127.0.0.1";
        let key = self.private_key_path.to_string_lossy().into_owned();
        run_command(
            "ssh",
            &[
                "-F",
                "/dev/null",
                "-i",
                key.as_str(),
                "-p",
                &node.ssh_port.to_string(),
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "ConnectTimeout=5",
                target,
                script,
            ],
        )
    }

    fn create_outer_network(&self) -> Result<()> {
        let _ = docker_outer(["network", "rm", self.outer_network.as_str()]);
        docker_outer(["network", "create", self.outer_network.as_str()])?;
        Ok(())
    }

    fn ensure_payload(&self) -> Result<()> {
        if self.payload_dir.exists() {
            fs::remove_dir_all(&self.payload_dir).map_err(|error| {
                Error::Io(format!(
                    "remove stale payload dir '{}': {error}",
                    self.payload_dir.display()
                ))
            })?;
        }
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| Error::Message("failed to resolve repo root".into()))?
            .to_path_buf();
        let script = repo_root.join("scripts/build-install-payload.sh");
        run_command_expect_ok(
            "bash",
            &[
                script.to_string_lossy().as_ref(),
                "--output",
                self.payload_dir.to_string_lossy().as_ref(),
            ],
        )?;
        Ok(())
    }

    fn start_nodes(&mut self, names: &[&str]) -> Result<()> {
        for name in names {
            let ssh_port = pick_free_port()?;
            let container_name = format!("ployz-e2e-{}-{name}", self.scenario.as_str());
            let _ = docker_outer(["rm", "-f", container_name.as_str()]);

            let key_mount = format!(
                "{}:/e2e-keys:ro",
                self.private_key_path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.root_dir.join("keys"))
                    .to_string_lossy()
            );
            let payload_mount = format!("{}:/e2e-payload:ro", self.payload_dir.to_string_lossy());

            docker_outer([
                "run",
                "-d",
                "--privileged",
                "--name",
                container_name.as_str(),
                "--hostname",
                name,
                "--network",
                self.outer_network.as_str(),
                "-p",
                &format!("{ssh_port}:22"),
                "-e",
                &format!("PLOYZ_E2E_SSH_AUTHORIZED_KEY={}", self.public_key),
                "-v",
                key_mount.as_str(),
                "-v",
                payload_mount.as_str(),
                self.image.as_str(),
            ])?;

            let outer_ip = docker_outer([
                "inspect",
                "--format",
                "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
                container_name.as_str(),
            ])?
            .stdout
            .trim()
            .to_string();
            if outer_ip.is_empty() {
                return Err(Error::Message(format!(
                    "node '{name}' did not receive an outer network IP"
                )));
            }

            let node = Node {
                name: (*name).to_string(),
                container_name,
                ssh_port,
                outer_ip,
            };
            self.wait_for_ssh(&node)?;
            self.wait_for_daemon(&node)?;
            self.nodes.push(node);
            self.write_metadata()?;
        }

        Ok(())
    }

    fn wait_for_ssh(&self, node: &Node) -> Result<()> {
        wait_until(SSH_WAIT_TIMEOUT, || match self.ssh_run(node, "true") {
            Ok(output) => Ok(output.status.success()),
            Err(_) => Ok(false),
        })
        .map_err(|error| {
            Error::Message(format!(
                "ssh did not become ready on {}: {error}",
                node.name
            ))
        })
    }

    fn wait_for_daemon(&self, node: &Node) -> Result<()> {
        wait_until(DAEMON_WAIT_TIMEOUT, || match self.ssh_run(node, "ployzd status") {
            Ok(output) => Ok(output.status.success()),
            Err(_) => Ok(false),
        })
        .map_err(|error| {
            Error::Message(format!(
                "daemon did not become ready on {}: {error}",
                node.name
            ))
        })
    }

    fn wait_mesh_ready(&self, node: &Node, timeout: Duration) -> Result<()> {
        wait_until(timeout, || {
            let Ok(output) = self.ssh_run(node, "ployzd --plain mesh ready --json") else {
                return Ok(false);
            };
            if !output.status.success() {
                return Ok(false);
            }
            parse_ready(output.stdout.trim())
        })
        .map_err(|error| {
            Error::Message(format!(
                "mesh did not become ready on {}: {error}",
                node.name
            ))
        })
    }

    fn wait_machine_state(
        &self,
        node: &Node,
        machine_id: &str,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<()> {
        wait_until(timeout, || {
            let Ok(output) = self.ssh_run(node, "ployzd machine ls") else {
                return Ok(false);
            };
            if !output.status.success() {
                return Ok(false);
            }
            Ok(machine_state(&output.stdout, machine_id) == Some(expected_state))
        })
        .map_err(|error| {
            Error::Message(format!(
                "machine '{machine_id}' did not reach state '{expected_state}' on {}: {error}",
                node.name
            ))
        })
    }

    fn wait_machine_absent(&self, node: &Node, machine_id: &str, timeout: Duration) -> Result<()> {
        wait_until(timeout, || {
            let Ok(output) = self.ssh_run(node, "ployzd machine ls") else {
                return Ok(false);
            };
            if !output.status.success() {
                return Ok(false);
            }
            Ok(machine_state(&output.stdout, machine_id).is_none())
        })
        .map_err(|error| {
            Error::Message(format!(
                "machine '{machine_id}' was not removed on {}: {error}",
                node.name
            ))
        })
    }

    fn generate_ssh_keypair(&mut self) -> Result<()> {
        if self.private_key_path.exists() {
            fs::remove_file(&self.private_key_path).map_err(|error| {
                Error::Io(format!(
                    "remove stale ssh key '{}': {error}",
                    self.private_key_path.display()
                ))
            })?;
        }
        if self.public_key_path.exists() {
            fs::remove_file(&self.public_key_path).map_err(|error| {
                Error::Io(format!(
                    "remove stale ssh public key '{}': {error}",
                    self.public_key_path.display()
                ))
            })?;
        }

        let key_path = self.private_key_path.to_string_lossy().into_owned();
        run_command_expect_ok(
            "ssh-keygen",
            &["-q", "-t", "ed25519", "-N", "", "-f", key_path.as_str()],
        )?;

        self.public_key = fs::read_to_string(&self.public_key_path).map_err(|error| {
            Error::Io(format!(
                "read ssh public key '{}': {error}",
                self.public_key_path.display()
            ))
        })?;
        self.public_key = self.public_key.trim().to_string();
        self.write_metadata()?;
        Ok(())
    }

    fn write_metadata(&self) -> Result<()> {
        let mut metadata = String::new();
        let _ = writeln!(&mut metadata, "scenario={}", self.scenario.as_str());
        let _ = writeln!(&mut metadata, "image={}", self.image);
        let _ = writeln!(&mut metadata, "outer_network={}", self.outer_network);
        let _ = writeln!(
            &mut metadata,
            "private_key={}",
            self.private_key_path.display()
        );
        let _ = writeln!(
            &mut metadata,
            "public_key={}",
            self.public_key_path.display()
        );
        for node in &self.nodes {
            let _ = writeln!(
                &mut metadata,
                "node={} container={} ssh_port={} ip={}",
                node.name, node.container_name, node.ssh_port, node.outer_ip
            );
        }
        fs::write(self.root_dir.join("metadata.env"), metadata).map_err(|error| {
            Error::Io(format!(
                "write metadata '{}': {error}",
                self.root_dir.join("metadata.env").display()
            ))
        })
    }
}

fn machine_state<'a>(machine_ls: &'a str, machine_id: &str) -> Option<&'a str> {
    machine_ls.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let Some([id, status, participation]) = collect_prefix(&mut fields) else {
            return None;
        };
        if id != machine_id {
            return None;
        }
        let _ = status;
        Some(participation)
    })
}

fn collect_prefix<'a, I>(iter: &mut I) -> Option<[&'a str; 3]>
where
    I: Iterator<Item = &'a str>,
{
    let first = iter.next()?;
    let second = iter.next()?;
    let third = iter.next()?;
    Some([first, second, third])
}
