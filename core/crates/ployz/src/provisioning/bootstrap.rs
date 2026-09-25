use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::time::timeout;

use super::ProvisionError;

const RELEASE_REPOSITORY: &str = "https://github.com/getployz/ployz2";
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// A verified copy of this CLI release's daemon, owned by a private directory.
pub(super) struct Bootstrap {
    /// Removes the extracted daemon on drop.
    _directory: TempDir,
    daemon: PathBuf,
}

impl Bootstrap {
    /// Download, checksum, and extract this release's bootstrap for a Linux architecture.
    pub(super) async fn acquire(architecture: &str) -> Result<Self, ProvisionError> {
        let archive_name = daemon_archive(architecture)?;
        let base = format!(
            "{RELEASE_REPOSITORY}/releases/download/v{}",
            env!("CARGO_PKG_VERSION")
        );
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .user_agent("ployz-bootstrap")
            .build()
            .map_err(|source| ProvisionError::BootstrapDownload {
                stage: "build bootstrap download client",
                source,
            })?;
        let archive = download(
            &client,
            &format!("{base}/{archive_name}"),
            "download bootstrap daemon archive",
        )
        .await?;
        let checksums = download(
            &client,
            &format!("{base}/checksums.txt"),
            "download bootstrap release checksums",
        )
        .await?;
        Self::extract(archive_name, &archive, &checksums).await
    }

    /// Build a bootstrap from a local release directory, verified like a download.
    #[cfg(test)]
    pub(super) async fn from_release_dir(
        release: &Path,
        architecture: &str,
    ) -> Result<Self, ProvisionError> {
        let archive_name = daemon_archive(architecture)?;
        let read = |name: &str| {
            fs::read(release.join(name)).map_err(|source| ProvisionError::BootstrapIo {
                stage: "read local bootstrap release",
                source,
            })
        };
        Self::extract(archive_name, &read(archive_name)?, &read("checksums.txt")?).await
    }

    async fn extract(
        archive_name: &str,
        archive: &[u8],
        checksums: &[u8],
    ) -> Result<Self, ProvisionError> {
        verify_checksum(archive, checksums, archive_name)?;
        let directory = tempfile::Builder::new()
            .prefix("ployz-bootstrap-")
            .tempdir()
            .map_err(|source| ProvisionError::BootstrapIo {
                stage: "create bootstrap directory",
                source,
            })?;
        let archive_path = directory.path().join(archive_name);
        fs::write(&archive_path, archive).map_err(|source| ProvisionError::BootstrapIo {
            stage: "stage bootstrap daemon archive",
            source,
        })?;
        let daemon = directory.path().join("ployzd");
        let mut extract = Command::new("tar");
        extract
            .arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(directory.path())
            .arg("ployzd");
        command_status("extract bootstrap daemon", extract).await?;
        fs::set_permissions(&daemon, fs::Permissions::from_mode(0o700)).map_err(|source| {
            ProvisionError::BootstrapIo {
                stage: "mark bootstrap daemon executable",
                source,
            }
        })?;

        Ok(Self {
            _directory: directory,
            daemon,
        })
    }

    pub(super) fn daemon(&self) -> &Path {
        &self.daemon
    }

    pub(super) async fn verify_local_version(&self) -> Result<(), ProvisionError> {
        let mut command = Command::new(&self.daemon);
        command.arg("version");
        verify_command_version("preflight bootstrap daemon", command).await
    }
}

pub(super) async fn verify_command_version(
    stage: &'static str,
    command: Command,
) -> Result<(), ProvisionError> {
    let output = command_output(command, VERSION_TIMEOUT)
        .await
        .map_err(|source| ProvisionError::BootstrapIo { stage, source })?;
    if !output.status.success() {
        return Err(ProvisionError::BootstrapCommand {
            stage,
            status: output.status,
        });
    }
    let observed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if observed == env!("CARGO_PKG_VERSION") {
        Ok(())
    } else {
        Err(ProvisionError::BootstrapVerification(format!(
            "bootstrap daemon reported version {observed:?}, expected {}",
            env!("CARGO_PKG_VERSION")
        )))
    }
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    stage: &'static str,
) -> Result<Vec<u8>, ProvisionError> {
    client
        .get(url)
        .send()
        .await
        .map_err(|source| ProvisionError::BootstrapDownload { stage, source })?
        .error_for_status()
        .map_err(|source| ProvisionError::BootstrapDownload { stage, source })?
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|source| ProvisionError::BootstrapDownload { stage, source })
}

fn verify_checksum(
    archive: &[u8],
    checksums: &[u8],
    archive_name: &str,
) -> Result<(), ProvisionError> {
    let checksums = std::str::from_utf8(checksums).map_err(|error| {
        ProvisionError::BootstrapVerification(format!("checksums.txt is not UTF-8: {error}"))
    })?;
    let expected = checksums
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let hash = fields.next()?;
            let name = fields.next()?.trim_start_matches('*');
            (name == archive_name
                && hash.len() == 64
                && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
        })
        .ok_or_else(|| {
            ProvisionError::BootstrapVerification(format!(
                "checksums.txt has no SHA-256 hash for {archive_name}"
            ))
        })?;
    let actual = hex::encode(Sha256::digest(archive));
    if actual == expected {
        Ok(())
    } else {
        Err(ProvisionError::BootstrapVerification(format!(
            "{archive_name} checksum was {actual}, expected {expected}"
        )))
    }
}

async fn command_status(stage: &'static str, command: Command) -> Result<(), ProvisionError> {
    let status = tokio::process::Command::from(command)
        .status()
        .await
        .map_err(|source| ProvisionError::BootstrapIo { stage, source })?;
    if status.success() {
        Ok(())
    } else {
        Err(ProvisionError::BootstrapCommand { stage, status })
    }
}

/// Capture bounded command output and explicitly kill and reap a timed-out child.
async fn command_output(mut command: Command, budget: Duration) -> io::Result<Output> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?));
    let mut child = tokio::process::Command::from(command).spawn()?;
    let status = match timeout(budget, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            if child.try_wait()?.is_none() {
                child.kill().await?;
            }
            let _ = child.wait().await;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed out after {} seconds", budget.as_secs()),
            ));
        }
    };
    Ok(Output {
        status,
        stdout: read_from_start(&mut stdout)?,
        stderr: read_from_start(&mut stderr)?,
    })
}

fn read_from_start(file: &mut File) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn daemon_archive(architecture: &str) -> Result<&'static str, ProvisionError> {
    match architecture {
        "x86_64" => Ok("ployzd_linux_amd64.tar.gz"),
        "aarch64" => Ok("ployzd_linux_arm64.tar.gz"),
        architecture => Err(ProvisionError::UnsupportedArchitecture(
            architecture.to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_output_kills_and_reaps_a_hung_child() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let error = command_output(command, Duration::from_millis(20))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
