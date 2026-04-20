use crate::cli::Scenario;
use crate::error::{Error, Result};
use crate::scenarios;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use super::environment::SharedPayload;
use super::nodes::Node;

#[derive(Debug, Clone)]
pub(crate) struct ScenarioRun {
    pub(crate) scenario: Scenario,
    pub(crate) image: String,
    pub(crate) image_id: String,
    pub(crate) image_platform: String,
    pub(crate) root_dir: PathBuf,
    pub(crate) payload_dir: PathBuf,
    pub(crate) outer_network: String,
    pub(crate) private_key_path: PathBuf,
    pub(crate) public_key_path: PathBuf,
    pub(crate) public_key: String,
    pub(crate) keep_failed: bool,
    pub(crate) nodes: Vec<Node>,
}

#[derive(Clone, Copy)]
pub(crate) enum CleanupReason {
    Success,
    Failure,
}

impl ScenarioRun {
    pub(crate) fn new(
        scenario: Scenario,
        image: &str,
        shared_payload: &SharedPayload,
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

        fs::create_dir_all(&key_dir).map_err(|error| {
            Error::Io(format!("create key dir '{}': {error}", key_dir.display()))
        })?;

        let private_key_path = key_dir.join("id_ed25519");
        let public_key_path = key_dir.join("id_ed25519.pub");
        let run = Self {
            scenario,
            image: image.to_string(),
            image_id: shared_payload.image_id.clone(),
            image_platform: shared_payload.image_platform.clone(),
            root_dir,
            payload_dir: shared_payload.payload_dir.clone(),
            outer_network: format!("ployz-e2e-net-{run_id}"),
            private_key_path,
            public_key_path,
            public_key: String::new(),
            keep_failed,
            nodes: Vec::new(),
        };
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

    pub(crate) fn cleanup(&self, reason: CleanupReason) {
        if matches!(reason, CleanupReason::Failure) && self.keep_failed {
            return;
        }

        for node in &self.nodes {
            let _ = crate::support::docker_outer(["rm", "-f", node.container_name.as_str()]);
        }
        let _ = crate::support::docker_outer(["network", "rm", self.outer_network.as_str()]);
    }

    #[must_use]
    pub(crate) fn scenario(&self) -> Scenario {
        self.scenario
    }
}
