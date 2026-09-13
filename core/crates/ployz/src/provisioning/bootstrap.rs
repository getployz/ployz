use std::{
    env,
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
    directory: TempDir,
    daemon: PathBuf,
    archive: PathBuf,
    checksums: PathBuf,
    local_source: bool,
}

impl Bootstrap {
    /// Acquire, checksum, and extract the bootstrap for a Linux architecture.
    pub(super) async fn acquire(architecture: &str) -> Result<Self, ProvisionError> {
        let archive_name = daemon_archive(architecture)?;
        let directory = tempfile::Builder::new()
            .prefix("ployz-bootstrap-")
            .tempdir()
            .map_err(|source| ProvisionError::BootstrapIo {
                stage: "create bootstrap directory",
                source,
            })?;
        let archive = directory.path().join(archive_name);
        let checksums = directory.path().join("checksums.txt");
        let (archive_bytes, checksum_bytes, local_source) =
            if let Some(source) = env::var_os("PLOYZ_RELEASE_DIR") {
                let source = PathBuf::from(source);
                let archive_bytes =
                    read_file(&source.join(archive_name), "copy bootstrap daemon archive")?;
                let checksum_bytes = read_file(
                    &source.join("checksums.txt"),
                    "copy bootstrap release checksums",
                )?;
                (archive_bytes, checksum_bytes, true)
            } else {
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
                let archive_bytes = download(
                    &client,
                    &format!("{base}/{archive_name}"),
                    "download bootstrap daemon archive",
                )
                .await?;
                let checksum_bytes = download(
                    &client,
                    &format!("{base}/checksums.txt"),
                    "download bootstrap release checksums",
                )
                .await?;
                (archive_bytes, checksum_bytes, false)
            };

        verify_checksum(&archive_bytes, &checksum_bytes, archive_name)?;
        write_file(&archive, &archive_bytes, "stage bootstrap daemon archive")?;
        write_file(
            &checksums,
            &checksum_bytes,
            "stage bootstrap release checksums",
        )?;
        let daemon = directory.path().join("ployzd");
        let mut extract = Command::new("tar");
        extract
            .arg("-xzf")
            .arg(&archive)
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
            directory,
            daemon,
            archive,
            checksums,
            local_source,
        })
    }

    pub(super) fn daemon(&self) -> &Path {
        &self.daemon
    }

    /// Return the local release directory only when it describes the selected target.
    pub(super) fn release_dir(&self, version: &str) -> Option<&Path> {
        (self.local_source && version == env!("CARGO_PKG_VERSION")).then(|| self.directory.path())
    }

    pub(super) fn release_files(&self, version: &str) -> Option<[&Path; 2]> {
        self.release_dir(version)
            .map(|_| [self.archive.as_path(), self.checksums.as_path()])
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

fn read_file(path: &Path, stage: &'static str) -> Result<Vec<u8>, ProvisionError> {
    fs::read(path).map_err(|source| ProvisionError::BootstrapIo { stage, source })
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

fn write_file(path: &Path, bytes: &[u8], stage: &'static str) -> Result<(), ProvisionError> {
    fs::write(path, bytes).map_err(|source| ProvisionError::BootstrapIo { stage, source })
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

fn daemon_archive(architecture: &str) -> Result<&'static str, ProvisionError> {
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
