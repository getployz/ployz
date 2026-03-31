use crate::error::{Error, Result};
use crate::runner::scenario_run::ScenarioRun;
use crate::support::{
    CommandOutput, docker_outer, pick_free_port, run_command, run_command_expect_ok,
};
use std::fmt::Write as _;
use std::path::Path;
use std::thread;

#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub(crate) name: String,
    pub(crate) container_name: String,
    pub(crate) ssh_port: u16,
    pub(crate) outer_ip: String,
}

impl ScenarioRun {
    pub(crate) fn node(&self, name: &str) -> Result<&Node> {
        self.nodes
            .iter()
            .find(|node| node.name == name)
            .ok_or_else(|| Error::Message(format!("node '{name}' is not running")))
    }

    pub(crate) fn ssh_run_name(&self, node_name: &str, script: &str) -> Result<CommandOutput> {
        self.ssh_run(self.node(node_name)?, script)
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

    pub(crate) fn ssh_run(&self, node: &Node, script: &str) -> Result<CommandOutput> {
        ssh_run_with_key(self.private_key_path.as_path(), node, script)
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

    pub(crate) fn machine_add_command(&self, target_names: &[&str]) -> Result<String> {
        let mut command = String::from("ployzd machine add --identity /e2e-keys/id_ed25519");

        for target_name in target_names {
            let target = self.node(target_name)?;
            let _ = write!(&mut command, " root@{}", target.outer_ip);
        }

        Ok(command)
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

    pub(crate) fn tick_nodes(&self, node_names: &[&str], repeat: u32) -> Result<()> {
        let commands = node_names
            .iter()
            .map(|node_name| (*node_name, format!("ployzd debug tick --repeat {repeat}")))
            .collect::<Vec<_>>();
        self.ssh_expect_ok_concurrent(&commands)?;
        Ok(())
    }

    pub(crate) fn mesh_init(&self, node_name: &str, network: &str) -> Result<()> {
        self.ssh_expect_ok_name(node_name, &format!("ployzd mesh init {network}"))?;
        Ok(())
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
                    "sh -lc 'iptables -N {} 2>/dev/null || true; \
                     iptables -N {} 2>/dev/null || true; \
                     iptables -F {}; \
                     iptables -F {}; \
                     iptables -C INPUT -j {} 2>/dev/null || iptables -I INPUT 1 -j {}; \
                     iptables -C OUTPUT -j {} 2>/dev/null || iptables -I OUTPUT 1 -j {}'",
                    super::environment::PARTITION_INPUT_CHAIN,
                    super::environment::PARTITION_OUTPUT_CHAIN,
                    super::environment::PARTITION_INPUT_CHAIN,
                    super::environment::PARTITION_OUTPUT_CHAIN,
                    super::environment::PARTITION_INPUT_CHAIN,
                    super::environment::PARTITION_INPUT_CHAIN,
                    super::environment::PARTITION_OUTPUT_CHAIN,
                    super::environment::PARTITION_OUTPUT_CHAIN
                ),
            )?;
        }

        Ok(())
    }

    fn install_partition_chains(&self, node: &Node) -> Result<()> {
        self.ssh_expect_ok(
            node,
            &format!(
                "sh -lc 'iptables -N {} 2>/dev/null || true; \
                 iptables -N {} 2>/dev/null || true; \
                 iptables -F {}; \
                 iptables -F {}; \
                 iptables -C INPUT -j {} 2>/dev/null || iptables -I INPUT 1 -j {}; \
                 iptables -C OUTPUT -j {} 2>/dev/null || iptables -I OUTPUT 1 -j {}'",
                super::environment::PARTITION_INPUT_CHAIN,
                super::environment::PARTITION_OUTPUT_CHAIN,
                super::environment::PARTITION_INPUT_CHAIN,
                super::environment::PARTITION_OUTPUT_CHAIN,
                super::environment::PARTITION_INPUT_CHAIN,
                super::environment::PARTITION_INPUT_CHAIN,
                super::environment::PARTITION_OUTPUT_CHAIN,
                super::environment::PARTITION_OUTPUT_CHAIN
            ),
        )?;
        Ok(())
    }

    fn add_partition_drop_rule(&self, node: &Node, peer_outer_ip: &str) -> Result<()> {
        self.ssh_expect_ok(
            node,
            &format!(
                "sh -lc 'iptables -A {} -s {peer_outer_ip} -j DROP; \
                 iptables -A {} -d {peer_outer_ip} -j DROP'",
                super::environment::PARTITION_INPUT_CHAIN,
                super::environment::PARTITION_OUTPUT_CHAIN,
            ),
        )?;
        Ok(())
    }

    pub(crate) fn start_nodes(&mut self, names: &[&str]) -> Result<()> {
        for name in names {
            let ssh_port = pick_free_port()?;
            let container_name = format!("ployz-e2e-{}-{name}", self.scenario.as_str());
            let _ = docker_outer(["rm", "-f", container_name.as_str()]);

            let key_mount = format!(
                "{}:/e2e-keys:ro",
                self.private_key_path
                    .parent()
                    .map_or_else(|| self.root_dir.join("keys"), Path::to_path_buf)
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

            for env_name in [
                super::environment::CORROSION_LOG_PATH_ENV,
                super::environment::CORROSION_RUST_LOG_ENV,
            ] {
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
        crate::support::wait_until(super::environment::SSH_WAIT_TIMEOUT, || {
            match self.ssh_run(node, "true") {
                Ok(output) => Ok(output.status.success()),
                Err(_) => Ok(false),
            }
        })
        .map_err(|error| {
            Error::Message(format!(
                "ssh did not become ready on {}: {error}",
                node.name
            ))
        })
    }

    fn wait_for_daemon(&self, node: &Node) -> Result<()> {
        crate::support::wait_until(super::environment::DAEMON_WAIT_TIMEOUT, || {
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
