use crate::cli::Scenario;
use crate::error::{Error, Result};
use crate::scenarios;
use crate::support::{
    CommandOutput, docker_outer, docker_outer_raw, parse_ready, pick_free_port, run_command,
    run_command_expect_ok, wait_until,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const SSH_WAIT_TIMEOUT: Duration = Duration::from_secs(90);
const DAEMON_WAIT_TIMEOUT: Duration = Duration::from_secs(90);
const READY_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const STATE_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const CONTAINER_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const PARTITION_INPUT_CHAIN: &str = "PLOYZ_E2E_PARTITION_INPUT";
const PARTITION_OUTPUT_CHAIN: &str = "PLOYZ_E2E_PARTITION_OUTPUT";
const E2E_PAYLOAD_BUILD_PROFILE: &str = "debug";
const CORROSION_LOG_PATH_ENV: &str = "PLOYZ_CORROSION_LOG_PATH";
const CORROSION_RUST_LOG_ENV: &str = "PLOYZ_CORROSION_RUST_LOG";

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
    image_id: String,
    image_platform: String,
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
        let ImageMetadata {
            id: image_id,
            platform: image_platform,
        } = image_metadata(image)?;
        let run = Self {
            scenario,
            image: image.to_string(),
            image_id,
            image_platform,
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
        self.log_progress("starting scenario");
        self.generate_ssh_keypair()?;
        self.create_outer_network()?;
        self.start_nodes(self.scenario.node_names())?;
        scenarios::run(self)
    }

    pub(crate) fn log_progress(&self, step: &str) {
        eprintln!(
            "[ployz-e2e:{}] {}",
            self.root_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(self.scenario.as_str()),
            step
        );
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
                .ssh_run(
                    node,
                    "docker ps -a --format '{{.ID}} {{.Names}} {{.Status}}'",
                )
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

    pub(crate) fn mesh_init(&self, node_name: &str, network: &str) -> Result<()> {
        self.ssh_expect_ok_name(node_name, &format!("ployzd mesh init {network}"))?;
        Ok(())
    }

    pub(crate) fn wait_mesh_ready_name(&self, node_name: &str) -> Result<()> {
        self.wait_mesh_ready_default(self.node(node_name)?)
    }

    pub(crate) fn machine_add(&self, controller_name: &str, target_name: &str) -> Result<()> {
        self.machine_add_many(controller_name, &[target_name])
    }

    pub(crate) fn machine_add_many(
        &self,
        controller_name: &str,
        target_names: &[&str],
    ) -> Result<()> {
        let controller = self.node(controller_name)?;
        let command = self.machine_add_command(target_names)?;
        self.ssh_expect_ok(controller, &command)?;
        Ok(())
    }

    pub(crate) fn machine_add_command(&self, target_names: &[&str]) -> Result<String> {
        let mut command = String::from("ployzd machine add --identity /e2e-keys/id_ed25519");

        for target_name in target_names {
            let target = self.node(target_name)?;
            let _ = write!(&mut command, " root@{}", target.outer_ip);
        }

        Ok(command)
    }

    pub(crate) fn machine_remove_force(
        &self,
        controller_name: &str,
        machine_id: &str,
    ) -> Result<()> {
        self.ssh_expect_ok_name(
            controller_name,
            &format!("ployzd machine rm {machine_id} --force"),
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn tick_node(&self, node_name: &str, repeat: u32) -> Result<()> {
        self.ssh_expect_ok_name(node_name, &format!("ployzd debug tick --repeat {repeat}"))?;
        Ok(())
    }

    pub(crate) fn tick_nodes(&self, node_names: &[&str], repeat: u32) -> Result<()> {
        let commands = node_names
            .iter()
            .map(|node_name| (*node_name, format!("ployzd debug tick --repeat {repeat}")))
            .collect::<Vec<_>>();
        self.ssh_expect_ok_concurrent(&commands)?;
        Ok(())
    }

    pub(crate) fn wait_mesh_ready_default(&self, node: &Node) -> Result<()> {
        self.wait_mesh_ready(node, READY_WAIT_TIMEOUT)
    }

    pub(crate) fn wait_machine_state_name(
        &self,
        node_name: &str,
        machine_id: &str,
        expected_state: &str,
    ) -> Result<()> {
        self.wait_machine_state_default(self.node(node_name)?, machine_id, expected_state)
    }

    pub(crate) fn wait_machine_state_default(
        &self,
        node: &Node,
        machine_id: &str,
        expected_state: &str,
    ) -> Result<()> {
        self.wait_machine_state(node, machine_id, expected_state, STATE_WAIT_TIMEOUT)
    }

    pub(crate) fn wait_machine_absent_default(&self, node: &Node, machine_id: &str) -> Result<()> {
        self.wait_machine_absent(node, machine_id, STATE_WAIT_TIMEOUT)
    }

    pub(crate) fn wait_machine_absent_name(&self, node_name: &str, machine_id: &str) -> Result<()> {
        self.wait_machine_absent_default(self.node(node_name)?, machine_id)
    }

    pub(crate) fn wait_all_machine_states(
        &self,
        node_name: &str,
        machine_ids: &[&str],
        expected_state: &str,
    ) -> Result<()> {
        let node = self.node(node_name)?;
        let joined_ids = machine_ids.join(", ");

        wait_until(STATE_WAIT_TIMEOUT, || {
            let Ok(output) = self.ssh_run(node, "ployzd machine ls") else {
                return Ok(false);
            };
            if !output.status.success() {
                return Ok(false);
            }
            Ok(machine_ids.iter().all(|machine_id| {
                machine_state(&output.stdout, machine_id).as_deref() == Some(expected_state)
            }))
        })
        .map_err(|error| {
            Error::Message(format!(
                "machines '{joined_ids}' did not reach state '{expected_state}' on {}: {error}",
                node.name
            ))
        })
    }

    pub(crate) fn wait_for_settled_machine_states(
        &self,
        node_name: &str,
        expected_states: &[(&str, &str)],
    ) -> Result<()> {
        let node = self.node(node_name)?;
        let expected_count = expected_states.len();
        let expected_labels = expected_states
            .iter()
            .map(|(machine_id, state)| format!("{machine_id}:{state}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut last_snapshot: Option<Vec<MachineRow>> = None;
        let mut consecutive_matches: u8 = 0;

        wait_until(STATE_WAIT_TIMEOUT, || {
            let Ok(output) = self.ssh_run(node, "ployzd machine ls") else {
                return Ok(false);
            };
            if !output.status.success() {
                return Ok(false);
            }

            let snapshot = machine_rows(&output.stdout);
            if snapshot.len() != expected_count {
                consecutive_matches = 0;
                last_snapshot = None;
                return Ok(false);
            }
            if !expected_states.iter().all(|(machine_id, expected_state)| {
                snapshot.iter().any(|row| {
                    row.id == *machine_id
                        && row.participation == *expected_state
                        && row.subnet != "—"
                })
            }) {
                consecutive_matches = 0;
                last_snapshot = None;
                return Ok(false);
            }

            if last_snapshot.as_ref() == Some(&snapshot) {
                consecutive_matches = consecutive_matches.saturating_add(1);
            } else {
                consecutive_matches = 1;
                last_snapshot = Some(snapshot);
            }

            Ok(consecutive_matches >= 3)
        })
        .map_err(|error| {
            Error::Message(format!(
                "machine state did not settle on {} for [{}]: {error}",
                node.name, expected_labels
            ))
        })
    }

    pub(crate) fn wait_for_settled_machine_states_with_ticks(
        &self,
        node_name: &str,
        expected_states: &[(&str, &str)],
        tick_nodes: &[&str],
        repeat: u32,
    ) -> Result<()> {
        let node = self.node(node_name)?;
        let expected_count = expected_states.len();
        let expected_labels = expected_states
            .iter()
            .map(|(machine_id, state)| format!("{machine_id}:{state}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut last_snapshot: Option<Vec<MachineRow>> = None;
        let mut consecutive_matches: u8 = 0;

        wait_until(STATE_WAIT_TIMEOUT, || {
            self.tick_nodes(tick_nodes, repeat)?;

            let Ok(output) = self.ssh_run(node, "ployzd machine ls") else {
                return Ok(false);
            };
            if !output.status.success() {
                return Ok(false);
            }

            let snapshot = machine_rows(&output.stdout);
            if snapshot.len() != expected_count {
                consecutive_matches = 0;
                last_snapshot = None;
                return Ok(false);
            }
            if !expected_states.iter().all(|(machine_id, expected_state)| {
                snapshot.iter().any(|row| {
                    row.id == *machine_id
                        && row.participation == *expected_state
                        && row.subnet != "—"
                })
            }) {
                consecutive_matches = 0;
                last_snapshot = None;
                return Ok(false);
            }

            if last_snapshot.as_ref() == Some(&snapshot) {
                consecutive_matches = consecutive_matches.saturating_add(1);
            } else {
                consecutive_matches = 1;
                last_snapshot = Some(snapshot);
            }

            Ok(consecutive_matches >= 3)
        })
        .map_err(|error| {
            Error::Message(format!(
                "machine state did not settle on {} for [{}]: {error}",
                node.name, expected_labels
            ))
        })
    }

    pub(crate) fn assert_unique_machine_subnets(&self, node_name: &str) -> Result<()> {
        let output = self.ssh_expect_ok_name(node_name, "ployzd machine ls")?;
        let mut seen: BTreeMap<String, String> = BTreeMap::new();

        for prefix in machine_rows(&output.stdout) {
            if !prefix.subnet.contains('/') {
                continue;
            }
            if let Some(existing) = seen.insert(prefix.subnet.clone(), prefix.id.clone()) {
                return Err(Error::Message(format!(
                    "duplicate subnet '{}' reported by {} for machines '{}' and '{}'",
                    prefix.subnet, node_name, existing, prefix.id
                )));
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn wait_for_unique_machine_subnets(&self, node_name: &str) -> Result<()> {
        wait_until(STATE_WAIT_TIMEOUT, || {
            match self.assert_unique_machine_subnets(node_name) {
                Ok(()) => Ok(true),
                Err(_) => Ok(false),
            }
        })
        .map_err(|error| {
            Error::Message(format!(
                "machine subnets did not become unique on {node_name}: {error}"
            ))
        })
    }

    pub(crate) fn wait_for_unique_machine_subnets_with_ticks(
        &self,
        node_name: &str,
        tick_nodes: &[&str],
        repeat: u32,
    ) -> Result<()> {
        wait_until(STATE_WAIT_TIMEOUT, || {
            self.tick_nodes(tick_nodes, repeat)?;
            match self.assert_unique_machine_subnets(node_name) {
                Ok(()) => Ok(true),
                Err(_) => Ok(false),
            }
        })
        .map_err(|error| {
            Error::Message(format!(
                "machine subnets did not become unique on {node_name}: {error}"
            ))
        })
    }

    pub(crate) fn wait_for_machine_ids_with_subnets(
        &self,
        node_name: &str,
        machine_ids: &[&str],
    ) -> Result<()> {
        let node = self.node(node_name)?;
        let joined_ids = machine_ids.join(", ");

        wait_until(STATE_WAIT_TIMEOUT, || {
            let Ok(output) = self.ssh_run(node, "ployzd machine ls") else {
                return Ok(false);
            };
            if !output.status.success() {
                return Ok(false);
            }

            let snapshot = machine_rows(&output.stdout);
            Ok(machine_ids.iter().all(|machine_id| {
                snapshot
                    .iter()
                    .any(|row| row.id == *machine_id && row.subnet != "—")
            }))
        })
        .map_err(|error| {
            Error::Message(format!(
                "machines '{joined_ids}' did not appear with subnets on {}: {error}",
                node.name
            ))
        })
    }

    pub(crate) fn partition_groups(&self, left: &[&str], right: &[&str]) -> Result<()> {
        self.clear_partition_rules()?;

        for node in &self.nodes {
            self.install_partition_chains(node)?;
        }

        for left_name in left {
            let left_node = self.node(left_name)?;
            for right_name in right {
                let right_node = self.node(right_name)?;
                self.add_partition_drop_rule(left_node, &right_node.outer_ip)?;
                self.add_partition_drop_rule(right_node, &left_node.outer_ip)?;
            }
        }

        Ok(())
    }

    pub(crate) fn clear_partition_rules(&self) -> Result<()> {
        for node in &self.nodes {
            self.ssh_expect_ok(
                node,
                &format!(
                    "sh -lc 'iptables -N {PARTITION_INPUT_CHAIN} 2>/dev/null || true; \
                     iptables -N {PARTITION_OUTPUT_CHAIN} 2>/dev/null || true; \
                     iptables -F {PARTITION_INPUT_CHAIN}; \
                     iptables -F {PARTITION_OUTPUT_CHAIN}; \
                     iptables -C INPUT -j {PARTITION_INPUT_CHAIN} 2>/dev/null || iptables -I INPUT 1 -j {PARTITION_INPUT_CHAIN}; \
                     iptables -C OUTPUT -j {PARTITION_OUTPUT_CHAIN} 2>/dev/null || iptables -I OUTPUT 1 -j {PARTITION_OUTPUT_CHAIN}'"
                ),
            )?;
        }

        Ok(())
    }

    pub(crate) fn wait_service_container_name(
        &self,
        node_name: &str,
        namespace: &str,
        service: &str,
    ) -> Result<()> {
        self.wait_service_container(self.node(node_name)?, namespace, service)
    }

    pub(crate) fn ssh_expect_ok_name(
        &self,
        node_name: &str,
        script: &str,
    ) -> Result<CommandOutput> {
        self.ssh_expect_ok(self.node(node_name)?, script)
    }

    pub(crate) fn ssh_expect_ok_concurrent(
        &self,
        commands: &[(&str, String)],
    ) -> Result<Vec<CommandOutput>> {
        let mut handles = Vec::with_capacity(commands.len());

        for (node_name, script) in commands {
            let node = self.node(node_name)?.clone();
            let private_key_path = self.private_key_path.clone();
            let script = script.clone();
            handles.push(thread::spawn(move || {
                ssh_run_with_key(private_key_path.as_path(), &node, &script)
            }));
        }

        let mut outputs = Vec::with_capacity(commands.len());
        for ((node_name, script), handle) in commands.iter().zip(handles) {
            let output = handle.join().map_err(|_| {
                Error::Message(format!(
                    "concurrent ssh command panicked on node '{node_name}'"
                ))
            })??;
            if output.status.success() {
                outputs.push(output);
                continue;
            }
            return Err(Error::CommandFailed {
                command: format!("ssh {node_name} -> {script}"),
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }

        Ok(outputs)
    }

    pub(crate) fn ssh_run_name(&self, node_name: &str, script: &str) -> Result<CommandOutput> {
        self.ssh_run(self.node(node_name)?, script)
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
        ssh_run_with_key(self.private_key_path.as_path(), node, script)
    }

    fn install_partition_chains(&self, node: &Node) -> Result<()> {
        self.ssh_expect_ok(
            node,
            &format!(
                "sh -lc 'iptables -N {PARTITION_INPUT_CHAIN} 2>/dev/null || true; \
                 iptables -N {PARTITION_OUTPUT_CHAIN} 2>/dev/null || true; \
                 iptables -F {PARTITION_INPUT_CHAIN}; \
                 iptables -F {PARTITION_OUTPUT_CHAIN}; \
                 iptables -C INPUT -j {PARTITION_INPUT_CHAIN} 2>/dev/null || iptables -I INPUT 1 -j {PARTITION_INPUT_CHAIN}; \
                 iptables -C OUTPUT -j {PARTITION_OUTPUT_CHAIN} 2>/dev/null || iptables -I OUTPUT 1 -j {PARTITION_OUTPUT_CHAIN}'"
            ),
        )?;
        Ok(())
    }

    fn add_partition_drop_rule(&self, node: &Node, peer_outer_ip: &str) -> Result<()> {
        self.ssh_expect_ok(
            node,
            &format!(
                "sh -lc 'iptables -A {PARTITION_INPUT_CHAIN} -s {peer_outer_ip} -j DROP; \
                 iptables -A {PARTITION_OUTPUT_CHAIN} -d {peer_outer_ip} -j DROP'"
            ),
        )?;
        Ok(())
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
                "--target-platform",
                self.image_platform.as_str(),
                "--profile",
                E2E_PAYLOAD_BUILD_PROFILE,
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
            let ssh_mapping = format!("{ssh_port}:22");
            let authorized_key = format!("PLOYZ_E2E_SSH_AUTHORIZED_KEY={}", self.public_key);
            let image_name = format!("PLOYZ_E2E_IMAGE={}", self.image);
            let image_id = format!("PLOYZ_E2E_IMAGE_ID={}", self.image_id);
            let scenario_name = format!("PLOYZ_E2E_SCENARIO={}", self.scenario.as_str());
            let node_name = format!("PLOYZ_E2E_NODE={name}");
            let run_id = format!(
                "PLOYZ_E2E_RUN_ID={}",
                self.root_dir
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
            );
            let mut args = vec![
                "run".to_string(),
                "-d".to_string(),
                "--privileged".to_string(),
                "--name".to_string(),
                container_name.clone(),
                "--hostname".to_string(),
                (*name).to_string(),
                "--network".to_string(),
                self.outer_network.clone(),
                "-p".to_string(),
                ssh_mapping,
                "-e".to_string(),
                authorized_key,
                "-e".to_string(),
                image_name,
                "-e".to_string(),
                image_id,
                "-e".to_string(),
                scenario_name,
                "-e".to_string(),
                node_name,
                "-e".to_string(),
                run_id,
            ];

            for env_name in [CORROSION_LOG_PATH_ENV, CORROSION_RUST_LOG_ENV] {
                if let Ok(value) = std::env::var(env_name) {
                    args.push("-e".to_string());
                    args.push(format!("{env_name}={value}"));
                }
            }

            args.push("-v".to_string());
            args.push(key_mount);
            args.push("-v".to_string());
            args.push(payload_mount);
            args.push(self.image.clone());

            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run_command_expect_ok("docker", &arg_refs)?;

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
            self.nodes.push(node);
            self.write_metadata()?;
        }

        for node in &self.nodes {
            self.wait_for_ssh(node)?;
        }
        for node in &self.nodes {
            self.wait_for_daemon(node)?;
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
        wait_until(DAEMON_WAIT_TIMEOUT, || {
            match self.ssh_run(node, "ployzd status") {
                Ok(output) => Ok(output.status.success()),
                Err(_) => Ok(false),
            }
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
            Ok(machine_state(&output.stdout, machine_id).as_deref() == Some(expected_state))
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
        let _ = writeln!(&mut metadata, "image_id={}", self.image_id);
        let _ = writeln!(&mut metadata, "image_platform={}", self.image_platform);
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

#[derive(Debug)]
struct ImageMetadata {
    id: String,
    platform: String,
}

fn image_metadata(image: &str) -> Result<ImageMetadata> {
    let output = docker_outer([
        "image",
        "inspect",
        "--format",
        "{{.Id}} {{.Os}}/{{.Architecture}}",
        image,
    ])?;
    let metadata = output.stdout.trim();
    let mut parts = metadata.split_whitespace();
    let Some(image_id) = parts.next() else {
        return Err(Error::Message(format!(
            "docker image inspect returned empty id for '{image}'"
        )));
    };
    let Some(image_platform) = parts.next() else {
        return Err(Error::Message(format!(
            "docker image inspect returned empty platform for '{image}'"
        )));
    };
    Ok(ImageMetadata {
        id: image_id.to_string(),
        platform: image_platform.to_string(),
    })
}

fn machine_state(machine_ls: &str, machine_id: &str) -> Option<String> {
    machine_ls
        .lines()
        .filter_map(parse_machine_row)
        .find_map(|row| {
            if row.id == machine_id {
                return Some(row.participation);
            }
            None
        })
}

fn ssh_run_with_key(private_key_path: &Path, node: &Node, script: &str) -> Result<CommandOutput> {
    let target = "root@127.0.0.1";
    let key = private_key_path.to_string_lossy().into_owned();
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct MachineRow {
    id: String,
    participation: String,
    subnet: String,
}

fn parse_machine_row(line: &str) -> Option<MachineRow> {
    let mut fields = line.split_whitespace();
    let id = fields.next()?;
    let _status = fields.next()?;
    let participation = fields.next()?;
    let _liveness = fields.next()?;
    let _overlay = fields.next()?;
    let subnet = fields.next()?;
    if id == "ID" {
        return None;
    }
    Some(MachineRow {
        id: id.to_string(),
        participation: participation.to_string(),
        subnet: subnet.to_string(),
    })
}

fn machine_rows(machine_ls: &str) -> Vec<MachineRow> {
    machine_ls.lines().filter_map(parse_machine_row).collect()
}
