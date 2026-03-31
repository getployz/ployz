use crate::error::{Error, Result};
use crate::runner::scenario_run::ScenarioRun;
use crate::support::{docker_outer, run_command_expect_ok};
use std::fs;
use std::path::Path;

pub(crate) const SSH_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
pub(crate) const DAEMON_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
pub(crate) const READY_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
pub(crate) const STATE_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
pub(crate) const CONTAINER_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
pub(crate) const PARTITION_INPUT_CHAIN: &str = "PLOYZ_E2E_PARTITION_INPUT";
pub(crate) const PARTITION_OUTPUT_CHAIN: &str = "PLOYZ_E2E_PARTITION_OUTPUT";
pub(crate) const E2E_PAYLOAD_BUILD_PROFILE: &str = "debug";
pub(crate) const CORROSION_LOG_PATH_ENV: &str = "PLOYZ_CORROSION_LOG_PATH";
pub(crate) const CORROSION_RUST_LOG_ENV: &str = "PLOYZ_CORROSION_RUST_LOG";

#[derive(Debug)]
pub(crate) struct ImageMetadata {
    pub(crate) id: String,
    pub(crate) platform: String,
}

pub(crate) fn image_metadata(image: &str) -> Result<ImageMetadata> {
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

impl ScenarioRun {
    pub(crate) fn create_outer_network(&self) -> Result<()> {
        let _ = docker_outer(["network", "rm", self.outer_network.as_str()]);
        docker_outer(["network", "create", self.outer_network.as_str()])?;
        Ok(())
    }

    pub(crate) fn ensure_payload(&self) -> Result<()> {
        if self.payload_dir.exists() {
            fs::remove_dir_all(&self.payload_dir).map_err(|error| {
                Error::Io(format!(
                    "remove stale payload dir '{}': {error}",
                    self.payload_dir.display()
                ))
            })?;
        }
        let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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

    pub(crate) fn generate_ssh_keypair(&mut self) -> Result<()> {
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
}
