use crate::cli::{Scenario, ZfsMode};
use crate::error::{Error, Result};
use crate::scenarios;
use crate::support::{
    CommandOutput, DaemonJsonPayload, docker_outer, docker_outer_raw, parse_daemon_json_response,
    parse_ready, parse_ready_payload, pick_free_port, run_command, run_command_expect_ok,
    wait_until,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::fs::OpenOptions;
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
const PAYLOAD_STAMP_FILE: &str = ".payload-stamp";
const PAYLOAD_LOCK_FILE: &str = ".payload.lock";
const PAYLOAD_LOCK_TIMEOUT: Duration = Duration::from_secs(300);
const PAYLOAD_LOCK_POLL: Duration = Duration::from_millis(200);
const PEBBLE_IMAGE: &str = "ployz-e2e-preload/pebble:latest";
const PEBBLE_CHALLTESTSRV_IMAGE: &str = "ployz-e2e-preload/pebble-challtestsrv:latest";
const PEBBLE_WAIT_TIMEOUT: Duration = Duration::from_secs(60);

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
    zfs_mode: ZfsMode,
    root_dir: PathBuf,
    payload_dir: PathBuf,
    outer_network: String,
    private_key_path: PathBuf,
    public_key_path: PathBuf,
    public_key: String,
    keep_failed: bool,
    nodes: Vec<Node>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubnetExpectation {
    Present,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MachineExpectation<'a> {
    pub(crate) id: &'a str,
    pub(crate) lifecycle: &'a str,
    pub(crate) subnet: SubnetExpectation,
}

#[derive(Debug)]
struct NodeStartMounts {
    dind: String,
    entrypoint: String,
    preloaded_images: String,
    pebble: String,
}

fn node_start_mounts(repo_root: &Path) -> NodeStartMounts {
    NodeStartMounts {
        dind: format!(
            "{}:/usr/local/bin/e2e-dind.sh:ro",
            repo_root
                .join("packaging/e2e/e2e-dind.sh")
                .to_string_lossy()
        ),
        entrypoint: format!(
            "{}:/usr/local/bin/e2e-node-entrypoint.sh:ro",
            repo_root
                .join("packaging/e2e/e2e-node-entrypoint.sh")
                .to_string_lossy()
        ),
        preloaded_images: format!(
            "{}:/opt/ployz-e2e/preloaded-images:ro",
            repo_root
                .join("packaging/e2e/preloaded-images")
                .to_string_lossy()
        ),
        pebble: format!(
            "{}:/e2e-pebble:ro",
            repo_root.join("packaging/e2e/pebble").to_string_lossy()
        ),
    }
}

impl ScenarioRun {
    pub(crate) fn new(
        scenario: Scenario,
        image: &str,
        artifacts_root: &Path,
        keep_failed: bool,
        zfs_mode: ZfsMode,
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
            zfs_mode,
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
        self.log_progress(&format!(
            "starting scenario '{}' with image '{}'",
            self.scenario.as_str(),
            self.image
        ));
        self.generate_ssh_keypair()?;
        self.create_outer_network()?;
        self.start_nodes(self.scenario.node_names())?;
        self.log_progress("running scenario steps");
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
        if self.zfs_mode == ZfsMode::Real {
            self.cleanup_volume_smoke_zfs();
        }

        if failed && self.keep_failed {
            return;
        }

        for node in &self.nodes {
            let _ = docker_outer(["rm", "-f", node.container_name.as_str()]);
        }
        if self.scenario == Scenario::DeploySmoke {
            let _ = docker_outer(["rm", "-f", self.pebble_container_name().as_str()]);
            let _ = docker_outer(["rm", "-f", self.challtestsrv_container_name().as_str()]);
        }
        let _ = docker_outer(["network", "rm", self.outer_network.as_str()]);
    }

    fn cleanup_volume_smoke_zfs(&self) {
        let command = "mode=$(cat /var/lib/ployz-e2e-zfs/mode 2>/dev/null || true); \
                       if [ \"$mode\" != real ]; then exit 0; fi; \
                       docker rm -f $(docker ps -aq --filter label=dev.ployz.namespace=default --filter label=dev.ployz.service=db) >/dev/null 2>&1 || true; \
                       pool=$(cat /var/lib/ployz-e2e-zfs/pool.name 2>/dev/null || true); \
                       if [ -n \"$pool\" ]; then zpool destroy \"$pool\" >/dev/null 2>&1 || true; fi; \
                       loopdev=$(cat /var/lib/ployz-e2e-zfs/pool.loop 2>/dev/null || true); \
                       if [ -n \"$loopdev\" ]; then losetup -d \"$loopdev\" >/dev/null 2>&1 || true; fi; \
                       rm -f /var/lib/ployz-e2e-zfs/pool.img /var/lib/ployz-e2e-zfs/pool.loop";
        for node in &self.nodes {
            if let Err(error) = self.ssh_run(node, command) {
                self.log_progress(&format!(
                    "real zfs cleanup command failed on {}: {error}",
                    node.name
                ));
            }
        }
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
                container_logs.combined(),
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

        if self.scenario == Scenario::DeploySmoke {
            for container_name in [
                self.challtestsrv_container_name(),
                self.pebble_container_name(),
            ] {
                let container_logs =
                    docker_outer_raw(["logs", container_name.as_str()]).unwrap_or_default();
                fs::write(
                    logs_dir.join(format!("{container_name}.log")),
                    container_logs.combined(),
                )
                .map_err(|error| {
                    Error::Io(format!("write container log '{container_name}': {error}"))
                })?;
            }
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
        self.log_progress(&format!("mesh_init node={node_name} network={network}"));
        self.ssh_expect_ok_name(node_name, &format!("ployzd mesh init {network}"))?;
        self.log_progress(&format!("mesh_init complete node={node_name}"));
        Ok(())
    }

    pub(crate) fn mesh_start(&self, node_name: &str, network: &str) -> Result<()> {
        self.log_progress(&format!("mesh_start node={node_name} network={network}"));
        self.ssh_expect_ok_name(node_name, &format!("ployzd mesh start {network}"))?;
        self.log_progress(&format!("mesh_start complete node={node_name}"));
        Ok(())
    }

    pub(crate) fn mesh_stop(&self, node_name: &str, force: bool) -> Result<()> {
        self.log_progress(&format!("mesh_stop node={node_name} force={force}"));
        let command = if force {
            "ployzd mesh stop --force"
        } else {
            "ployzd mesh stop"
        };
        self.ssh_expect_ok_name(node_name, command)?;
        self.log_progress(&format!("mesh_stop complete node={node_name}"));
        Ok(())
    }

    pub(crate) fn wait_mesh_ready_name(&self, node_name: &str) -> Result<()> {
        self.log_progress(&format!("wait_mesh_ready start node={node_name}"));
        self.wait_mesh_ready_default(self.node(node_name)?)
    }

    pub(crate) fn wait_mesh_standby_name(&self, node_name: &str) -> Result<()> {
        let node = self.node(node_name)?;
        self.log_progress(&format!("wait_mesh_standby start node={node_name}"));
        wait_until(READY_WAIT_TIMEOUT, || {
            let Ok(output) = self.ssh_run(node, "ployzd --plain mesh ready --json") else {
                return Ok(false);
            };
            if !output.status.success() {
                return Ok(false);
            }
            let payload = parse_ready_payload(output.stdout.trim())?;
            Ok(payload.ready && !payload.workload_subnet_present)
        })
        .map_err(|error| {
            Error::Message(format!(
                "mesh did not become standby-ready on {}: {error}",
                node.name
            ))
        })?;
        self.log_progress(&format!("wait_mesh_standby complete node={node_name}"));
        Ok(())
    }

    pub(crate) fn wait_mesh_absent_name(&self, node_name: &str) -> Result<()> {
        let node = self.node(node_name)?;
        self.log_progress(&format!("wait_mesh_absent start node={node_name}"));
        wait_until(READY_WAIT_TIMEOUT, || {
            let Ok(output) = self.ssh_run(node, "ployzd --plain mesh ready --json") else {
                return Ok(false);
            };
            if output.status.success() {
                return Ok(false);
            }
            let combined = output.combined();
            Ok(combined.contains("NO_RUNNING_NETWORK") || combined.contains("no mesh running"))
        })
        .map_err(|error| {
            Error::Message(format!(
                "mesh did not become absent on {}: {error}",
                node.name
            ))
        })?;
        self.log_progress(&format!("wait_mesh_absent complete node={node_name}"));
        Ok(())
    }

    pub(crate) fn wait_network_dir_absent_name(
        &self,
        node_name: &str,
        network: &str,
    ) -> Result<()> {
        let node = self.node(node_name)?;
        self.log_progress(&format!(
            "wait_network_dir_absent start node={node_name} network={network}"
        ));
        wait_until(READY_WAIT_TIMEOUT, || {
            let output = self.ssh_run(
                node,
                &format!("test ! -d /var/lib/ployz/networks/{network}"),
            )?;
            Ok(output.status.success())
        })
        .map_err(|error| {
            Error::Message(format!(
                "network directory did not disappear on {} for {}: {error}",
                node.name, network
            ))
        })?;
        self.log_progress(&format!(
            "wait_network_dir_absent complete node={node_name} network={network}"
        ));
        Ok(())
    }

    pub(crate) fn machine_add(&self, controller_name: &str, target_name: &str) -> Result<()> {
        self.machine_add_many(controller_name, &[target_name])
    }

    pub(crate) fn machine_add_many(
        &self,
        controller_name: &str,
        target_names: &[&str],
    ) -> Result<()> {
        self.log_progress(&format!(
            "machine_add controller={controller_name} targets={}",
            target_names.join(",")
        ));
        let controller = self.node(controller_name)?;
        let command = self.machine_add_command(target_names)?;
        self.ssh_expect_ok(controller, &command)?;
        self.log_progress(&format!(
            "machine_add complete controller={controller_name}"
        ));
        Ok(())
    }

    pub(crate) fn machine_activate(&self, controller_name: &str, target_name: &str) -> Result<()> {
        self.log_progress(&format!(
            "machine_activate controller={controller_name} target={target_name}"
        ));
        self.ssh_expect_ok_name(
            controller_name,
            &format!("ployzd machine activate {target_name}"),
        )?;
        self.log_progress(&format!("machine_activate complete target={target_name}"));
        Ok(())
    }

    pub(crate) fn machine_drain(&self, controller_name: &str, target_name: &str) -> Result<()> {
        self.log_progress(&format!(
            "machine_drain controller={controller_name} target={target_name}"
        ));
        self.ssh_expect_ok_name(
            controller_name,
            &format!("ployzd machine drain {target_name}"),
        )?;
        self.log_progress(&format!("machine_drain complete target={target_name}"));
        Ok(())
    }

    pub(crate) fn machine_standby(
        &self,
        controller_name: &str,
        target_name: &str,
        force: bool,
    ) -> Result<()> {
        self.log_progress(&format!(
            "machine_standby controller={controller_name} target={target_name} force={force}"
        ));
        let command = if force {
            format!("ployzd machine standby {target_name} --force")
        } else {
            format!("ployzd machine standby {target_name}")
        };
        self.ssh_expect_ok_name(controller_name, &command)?;
        self.log_progress(&format!("machine_standby complete target={target_name}"));
        Ok(())
    }

    pub(crate) fn machine_rm(
        &self,
        controller_name: &str,
        target_name: &str,
        force: bool,
    ) -> Result<()> {
        let command = if force {
            format!("ployzd machine rm {target_name} --force")
        } else {
            format!("ployzd machine rm {target_name}")
        };
        self.ssh_expect_ok_name(controller_name, &command)?;
        Ok(())
    }

    pub(crate) fn mesh_destroy(&self, node_name: &str, network: &str) -> Result<()> {
        self.ssh_expect_ok_name(node_name, &format!("ployzd mesh destroy {network}"))?;
        Ok(())
    }

    pub(crate) fn hard_reboot_node(&self, node_name: &str) -> Result<()> {
        let node = self.node(node_name)?.clone();
        self.log_progress(&format!("hard_reboot start node={node_name}"));
        docker_outer(["kill", node.container_name.as_str()])?;
        docker_outer(["start", node.container_name.as_str()])?;
        self.wait_for_ssh(&node)?;
        self.wait_for_daemon(&node)?;
        self.log_progress(&format!("hard_reboot complete node={node_name}"));
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

    pub(crate) fn wait_mesh_ready_default(&self, node: &Node) -> Result<()> {
        self.wait_mesh_ready(node, READY_WAIT_TIMEOUT)
    }

    pub(crate) fn wait_machine_rows(
        &self,
        node_name: &str,
        expected_rows: &[MachineExpectation<'_>],
    ) -> Result<()> {
        let node = self.node(node_name)?;
        let expected_count = expected_rows.len();
        let expected_labels = expected_rows
            .iter()
            .map(|expected| {
                let subnet = match expected.subnet {
                    SubnetExpectation::Present => "subnet=present",
                    SubnetExpectation::Absent => "subnet=absent",
                };
                format!("{}:{}:{subnet}", expected.id, expected.lifecycle)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut last_snapshot: Option<Vec<MachineRow>> = None;
        let mut consecutive_matches: u8 = 0;
        self.log_progress(&format!(
            "wait_machine_rows start node={node_name} expected=[{expected_labels}]"
        ));

        wait_until(STATE_WAIT_TIMEOUT, || {
            let Ok(output) = self.ssh_run(node, "ployzd --json machine ls") else {
                return Ok(false);
            };
            if !output.status.success() {
                return Ok(false);
            }

            let snapshot = machine_rows(&output.stdout)?;
            if snapshot.len() != expected_count {
                consecutive_matches = 0;
                last_snapshot = None;
                return Ok(false);
            }
            if !expected_rows
                .iter()
                .all(|expected| snapshot.iter().any(|row| row.matches(*expected)))
            {
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
        })?;
        self.log_progress(&format!("wait_machine_rows complete node={node_name}"));
        Ok(())
    }

    pub(crate) fn assert_doctor_roles(
        &self,
        node_name: &str,
        local_role: &str,
        peer_roles: &[(&str, &str)],
    ) -> Result<()> {
        use crate::support::{DaemonJsonPayload, parse_daemon_json_response};
        let output = self.ssh_expect_ok_name(node_name, "ployzd --json doctor")?;
        let response = parse_daemon_json_response(&output.stdout)?;
        if !response.ok {
            return Err(Error::Message(format!(
                "doctor on {node_name} returned non-ok response: {}",
                response.message
            )));
        }
        let Some(DaemonJsonPayload::Doctor(payload)) = response.payload else {
            return Err(Error::Message(format!(
                "doctor on {node_name} missing doctor payload"
            )));
        };
        if payload.local.machine_role != local_role {
            return Err(Error::Message(format!(
                "doctor on {node_name} reports local machine_role='{}', expected '{}'",
                payload.local.machine_role, local_role
            )));
        }
        for (peer_id, expected_role) in peer_roles {
            let Some(peer) = payload.peers.iter().find(|p| p.machine_id == *peer_id) else {
                return Err(Error::Message(format!(
                    "doctor on {node_name} has no peer row for '{peer_id}'"
                )));
            };
            if peer.machine_role != *expected_role {
                return Err(Error::Message(format!(
                    "doctor on {node_name} reports peer '{peer_id}' machine_role='{}', expected '{expected_role}'",
                    peer.machine_role
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn assert_unique_machine_subnets(&self, node_name: &str) -> Result<()> {
        let output = self.ssh_expect_ok_name(node_name, "ployzd --json machine ls")?;
        let mut seen: BTreeMap<String, String> = BTreeMap::new();

        for prefix in machine_rows(&output.stdout)? {
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

    pub(crate) fn partition_groups(&self, left: &[&str], right: &[&str]) -> Result<()> {
        self.log_progress(&format!(
            "partition_groups start left={} right={}",
            left.join(","),
            right.join(",")
        ));
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

        self.log_progress("partition_groups complete");
        Ok(())
    }

    pub(crate) fn clear_partition_rules(&self) -> Result<()> {
        self.log_progress("clear_partition_rules start");
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

        self.log_progress("clear_partition_rules complete");
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

    pub(crate) fn start_pebble_for_http01(&self, node_name: &str) -> Result<()> {
        let node = self.node(node_name)?;
        self.log_progress("start_pebble_for_http01 start");
        let challtestsrv_name = self.challtestsrv_container_name();
        let pebble_name = self.pebble_container_name();
        let _ = docker_outer(["rm", "-f", challtestsrv_name.as_str()]);
        let _ = docker_outer(["rm", "-f", pebble_name.as_str()]);

        let challtestsrv_args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            challtestsrv_name.clone(),
            "--network".to_string(),
            self.outer_network.clone(),
            PEBBLE_CHALLTESTSRV_IMAGE.to_string(),
            "-defaultIPv6".to_string(),
            String::new(),
            "-management".to_string(),
            ":8055".to_string(),
            "-dnsserver".to_string(),
            ":8053".to_string(),
            "-http01".to_string(),
            ":5002".to_string(),
            "-tlsalpn01".to_string(),
            ":5001".to_string(),
            "-https01".to_string(),
            String::new(),
            "-doh".to_string(),
            String::new(),
        ];
        let challtestsrv_arg_refs: Vec<&str> =
            challtestsrv_args.iter().map(String::as_str).collect();
        run_command_expect_ok("docker", &challtestsrv_arg_refs)?;

        let challtestsrv_ip = self.container_outer_ip(&challtestsrv_name)?;
        self.ssh_expect_ok(
            node,
            &format!(
                "curl -fsS -X POST -d '{{\"ip\":\"{}\"}}' http://{}:8055/set-default-ipv4",
                node.outer_ip, challtestsrv_ip
            ),
        )?;

        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| Error::Message("failed to resolve repo root".into()))?
            .to_path_buf();
        let pebble_mount = format!(
            "{}:/e2e-pebble:ro",
            repo_root.join("packaging/e2e/pebble").to_string_lossy()
        );
        let pebble_args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            pebble_name.clone(),
            "--network".to_string(),
            self.outer_network.clone(),
            "-v".to_string(),
            pebble_mount,
            "-e".to_string(),
            "PEBBLE_VA_NOSLEEP=1".to_string(),
            "-e".to_string(),
            "PEBBLE_WFE_NONCEREJECT=0".to_string(),
            PEBBLE_IMAGE.to_string(),
            "-config".to_string(),
            "/e2e-pebble/pebble-config.json".to_string(),
            "-dnsserver".to_string(),
            format!("{challtestsrv_ip}:8053"),
            "-strict=false".to_string(),
        ];
        let pebble_arg_refs: Vec<&str> = pebble_args.iter().map(String::as_str).collect();
        run_command_expect_ok("docker", &pebble_arg_refs)?;

        wait_until(PEBBLE_WAIT_TIMEOUT, || {
            let output = self.ssh_run(
                node,
                &format!("curl -kfsS https://{}:14000/dir >/dev/null", pebble_name),
            )?;
            Ok(output.status.success())
        })?;
        self.log_progress("start_pebble_for_http01 complete");
        Ok(())
    }

    pub(crate) fn ssh_expect_ok_name(
        &self,
        node_name: &str,
        script: &str,
    ) -> Result<CommandOutput> {
        self.ssh_expect_ok(self.node(node_name)?, script)
    }

    pub(crate) fn ssh_run_concurrent(
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
        for (node_name, handle) in commands.iter().map(|(node_name, _)| node_name).zip(handles) {
            let output = handle.join().map_err(|_| {
                Error::Message(format!(
                    "concurrent ssh command panicked on node '{node_name}'"
                ))
            })??;
            outputs.push(output);
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
        self.log_progress("build payload manifest start");
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| Error::Message("failed to resolve repo root".into()))?
            .to_path_buf();
        let artifacts_dir = self
            .payload_dir
            .parent()
            .ok_or_else(|| Error::Message("payload dir has no parent".into()))?;
        let _lock = acquire_payload_lock(artifacts_dir)?;
        let stamp = payload_stamp(&repo_root, &self.image_platform, E2E_PAYLOAD_BUILD_PROFILE)?;
        let stamp_path = self.payload_dir.join(PAYLOAD_STAMP_FILE);
        if let Ok(existing) = fs::read_to_string(&stamp_path)
            && existing.trim() == stamp
        {
            self.log_progress("build payload manifest complete");
            return Ok(());
        }

        if self.payload_dir.exists() {
            fs::remove_dir_all(&self.payload_dir).map_err(|error| {
                Error::Io(format!(
                    "remove stale payload dir '{}': {error}",
                    self.payload_dir.display()
                ))
            })?;
        }
        let script = repo_root.join("scripts/build-install-payload.sh");
        let output = run_command_expect_ok(
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
        if let Some(line) = output
            .stdout
            .lines()
            .find(|line| line.starts_with("payload cache hit"))
        {
            self.log_progress(line);
        }
        fs::write(&stamp_path, stamp).map_err(|error| {
            Error::Io(format!(
                "write payload stamp '{}': {error}",
                stamp_path.display()
            ))
        })?;
        self.log_progress("build payload manifest complete");
        Ok(())
    }

    fn start_nodes(&mut self, names: &[&str]) -> Result<()> {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| Error::Message("failed to resolve repo root".into()))?
            .to_path_buf();
        let mounts = node_start_mounts(&repo_root);

        for name in names {
            self.start_node(name, &mounts)?;
        }

        self.wait_for_started_nodes()
    }

    fn start_node(&mut self, name: &str, mounts: &NodeStartMounts) -> Result<()> {
        let ssh_port = pick_free_port()?;
        let container_name = format!("ployz-e2e-{}-{name}", self.scenario.as_str());
        let _ = docker_outer(["rm", "-f", container_name.as_str()]);

        let args = self.node_run_args(name, &container_name, ssh_port, mounts);
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

        self.nodes.push(Node {
            name: name.to_string(),
            container_name,
            ssh_port,
            outer_ip,
        });
        self.write_metadata()
    }

    fn wait_for_started_nodes(&self) -> Result<()> {
        for node in &self.nodes {
            self.wait_for_ssh(node)?;
        }
        for node in &self.nodes {
            self.wait_for_daemon(node)?;
        }

        Ok(())
    }

    fn node_run_args(
        &self,
        name: &str,
        container_name: &str,
        ssh_port: u16,
        mounts: &NodeStartMounts,
    ) -> Vec<String> {
        let key_mount = format!(
            "{}:/e2e-keys:ro",
            self.private_key_path
                .parent()
                .map_or_else(|| self.root_dir.join("keys"), Path::to_path_buf)
                .to_string_lossy()
        );
        let payload_mount = format!("{}:/e2e-payload:ro", self.payload_dir.to_string_lossy());
        let mut args = self.node_run_base_args(name, container_name, ssh_port);

        args.push("-v".to_string());
        args.push(key_mount);
        args.push("-v".to_string());
        args.push(payload_mount);
        args.push("-v".to_string());
        args.push(mounts.dind.clone());
        args.push("-v".to_string());
        args.push(mounts.entrypoint.clone());
        args.push("-v".to_string());
        args.push(mounts.preloaded_images.clone());
        args.push("-v".to_string());
        args.push(mounts.pebble.clone());
        args.push(self.image.clone());
        args
    }

    fn node_run_base_args(&self, name: &str, container_name: &str, ssh_port: u16) -> Vec<String> {
        let ssh_mapping = format!("{ssh_port}:22");
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
            container_name.to_string(),
            "--hostname".to_string(),
            name.to_string(),
            "--network".to_string(),
            self.outer_network.clone(),
            "-p".to_string(),
            ssh_mapping,
            "-e".to_string(),
            format!("PLOYZ_E2E_SSH_AUTHORIZED_KEY={}", self.public_key),
            "-e".to_string(),
            format!("PLOYZ_E2E_IMAGE={}", self.image),
            "-e".to_string(),
            format!("PLOYZ_E2E_IMAGE_ID={}", self.image_id),
            "-e".to_string(),
            format!("PLOYZ_E2E_SCENARIO={}", self.scenario.as_str()),
            "-e".to_string(),
            format!("PLOYZ_E2E_RUNTIME={}", self.scenario.runtime()),
            "-e".to_string(),
            format!("PLOYZ_E2E_NODE={name}"),
            "-e".to_string(),
            format!("PLOYZ_PEER_CONTROL_TARGET={name}"),
            "-e".to_string(),
            run_id,
        ];

        if self.scenario == Scenario::DeploySmoke {
            args.push("-e".to_string());
            args.push(format!(
                "PLOYZ_ACME_DIRECTORY_URL=https://{}:14000/dir",
                self.pebble_container_name()
            ));
            args.push("-e".to_string());
            args.push("PLOYZ_ACME_ROOT_CA_PATH=/e2e-pebble/pebble.minica.pem".to_string());
            args.push("-e".to_string());
            args.push("PLOYZ_GATEWAY_HTTPS_LISTEN_ADDR=0.0.0.0:443".to_string());
        }

        if self.zfs_mode != ZfsMode::Off {
            args.push("-e".to_string());
            args.push(format!("PLOYZ_E2E_ZFS_MODE={}", self.zfs_mode.as_str()));
            if let Ok(pool) = std::env::var("PLOYZ_E2E_ZFS_POOL") {
                args.push("-e".to_string());
                args.push(format!("PLOYZ_E2E_ZFS_POOL={pool}"));
            }
            if let Ok(pool_size) = std::env::var("PLOYZ_E2E_ZFS_POOL_SIZE") {
                args.push("-e".to_string());
                args.push(format!("PLOYZ_E2E_ZFS_POOL_SIZE={pool_size}"));
            }
        }

        args
    }

    pub(crate) fn pebble_container_name(&self) -> String {
        format!("ployz-e2e-{}-pebble", self.scenario.as_str())
    }

    fn challtestsrv_container_name(&self) -> String {
        format!("ployz-e2e-{}-challtestsrv", self.scenario.as_str())
    }

    fn container_outer_ip(&self, container_name: &str) -> Result<String> {
        let output = docker_outer([
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            container_name,
        ])?;
        let ip = output.stdout.trim().to_string();
        if ip.is_empty() {
            return Err(Error::Message(format!(
                "container '{container_name}' did not receive an outer network IP"
            )));
        }
        Ok(ip)
    }

    fn wait_for_ssh(&self, node: &Node) -> Result<()> {
        self.log_progress(&format!("wait_for_ssh start node={}", node.name));
        wait_until(SSH_WAIT_TIMEOUT, || match self.ssh_run(node, "true") {
            Ok(output) => Ok(output.status.success()),
            Err(_) => Ok(false),
        })
        .map_err(|error| {
            Error::Message(format!(
                "ssh did not become ready on {}: {error}",
                node.name
            ))
        })?;
        self.log_progress(&format!("wait_for_ssh complete node={}", node.name));
        Ok(())
    }

    fn wait_for_daemon(&self, node: &Node) -> Result<()> {
        self.log_progress(&format!("wait_for_daemon start node={}", node.name));
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
        })?;
        self.log_progress(&format!("wait_for_daemon complete node={}", node.name));
        Ok(())
    }

    fn wait_mesh_ready(&self, node: &Node, timeout: Duration) -> Result<()> {
        self.log_progress(&format!(
            "wait_mesh_ready polling node={} timeout={}s",
            node.name,
            timeout.as_secs()
        ));
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
        })?;
        self.log_progress(&format!("wait_mesh_ready complete node={}", node.name));
        Ok(())
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

fn payload_stamp(repo_root: &Path, target_platform: &str, build_profile: &str) -> Result<String> {
    let script = repo_root.join("scripts/payload-stamp.sh");
    let output = run_command_expect_ok(
        "bash",
        &[
            script.to_string_lossy().as_ref(),
            repo_root.to_string_lossy().as_ref(),
            target_platform,
            build_profile,
        ],
    )?;
    Ok(output.stdout.trim().to_string())
}

struct PayloadLock {
    path: PathBuf,
}

impl Drop for PayloadLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn acquire_payload_lock(artifacts_dir: &Path) -> Result<PayloadLock> {
    let lock_path = artifacts_dir.join(PAYLOAD_LOCK_FILE);
    let deadline = std::time::Instant::now() + PAYLOAD_LOCK_TIMEOUT;

    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => return Ok(PayloadLock { path: lock_path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::time::Instant::now() >= deadline {
                    return Err(Error::Io(format!(
                        "timed out waiting for payload lock '{}'",
                        lock_path.display()
                    )));
                }
                thread::sleep(PAYLOAD_LOCK_POLL);
            }
            Err(error) => {
                return Err(Error::Io(format!(
                    "create payload lock '{}': {error}",
                    lock_path.display()
                )));
            }
        }
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
    lifecycle: String,
    subnet: String,
}

impl MachineRow {
    fn matches(&self, expected: MachineExpectation<'_>) -> bool {
        if self.id != expected.id || self.lifecycle != expected.lifecycle {
            return false;
        }
        match expected.subnet {
            SubnetExpectation::Present => self.subnet != "—",
            SubnetExpectation::Absent => self.subnet == "—",
        }
    }
}

fn machine_rows(machine_ls: &str) -> Result<Vec<MachineRow>> {
    let response = parse_daemon_json_response(machine_ls)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "machine list failed [{}]: {}",
            response.code, response.message
        )));
    }
    let Some(DaemonJsonPayload::MachineList(payload)) = response.payload else {
        return Err(Error::Message(String::from(
            "machine list response missing machine-list payload",
        )));
    };
    let mut rows: Vec<_> = payload
        .rows
        .into_iter()
        .map(|row| MachineRow {
            id: row.id,
            lifecycle: row.lifecycle,
            subnet: row.subnet.unwrap_or_else(|| String::from("—")),
        })
        .collect();
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_rows_parse_machine_list_payload() {
        let rows = machine_rows(
            r#"{
  "ok": true,
  "code": "OK",
  "message": "peer table",
  "payload": {
    "kind": "machine-list",
    "rows": [
      {
        "id": "peer",
        "lifecycle": "standby",
        "overlay_ip": "fd00::2",
        "created_at": 123,
        "subnet": null
      }
    ]
  }
}"#,
        )
        .expect("machine rows");
        let [row] = rows.as_slice() else {
            panic!("expected one row");
        };
        assert_eq!(row.id, "peer");
        assert_eq!(row.lifecycle, "standby");
        assert_eq!(row.subnet, "—");
    }
}
