use crate::error::{Error, Result};
use crate::runner::scenario_run::ScenarioRun;
use crate::support::{CommandOutput, docker_outer_raw};
use std::fmt::Write as _;
use std::fs;

impl ScenarioRun {
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

    pub(crate) fn write_metadata(&self) -> Result<()> {
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
                "node={} container={} ssh_port={} rpc_port={} ip={}",
                node.name, node.container_name, node.ssh_port, node.rpc_port, node.outer_ip
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
