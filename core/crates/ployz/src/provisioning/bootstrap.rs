use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};

use ployz_core::{
    MachineVersion, RELEASE_PUBLIC_KEY, RELEASE_SIGNATURE_ASSET, verify_release_checksums,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::time::timeout;

use super::ProvisionError;

const RELEASE_REPOSITORY: &str = "https://github.com/getployz/ployz";
/// Passes GitHub's release assets through over IPv6, for a server that can't connect to github.com.
const RELEASE_FALLBACK: &str = "https://ployz.sh";
pub(super) const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

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
        let asset = |file: &str| {
            let path = format!("releases/download/v{}/{file}", env!("CARGO_PKG_VERSION"));
            (
                format!("{RELEASE_REPOSITORY}/{path}"),
                format!("{RELEASE_FALLBACK}/{path}"),
            )
        };
        let (url, fallback) = asset(archive_name);
        let archive = download(
            &client,
            &url,
            Some(&fallback),
            "download bootstrap daemon archive",
        )
        .await?;
        let (url, fallback) = asset("checksums.txt");
        let checksums = download(
            &client,
            &url,
            Some(&fallback),
            "download bootstrap release checksums",
        )
        .await?;
        let (url, fallback) = asset(RELEASE_SIGNATURE_ASSET);
        let signature = download_if_present(
            &client,
            &url,
            Some(&fallback),
            "download bootstrap release checksums signature",
        )
        .await?;
        Self::extract(
            RELEASE_PUBLIC_KEY,
            archive_name,
            &archive,
            &checksums,
            signature.as_deref(),
        )
        .await
    }

    /// Build a bootstrap from a local release directory, verified like a download.
    #[cfg(test)]
    pub(super) async fn from_release_dir(
        release: &Path,
        architecture: &str,
    ) -> Result<Self, ProvisionError> {
        let archive_name = daemon_archive(architecture)?;
        let read_if_present = |name: &str| match fs::read(release.join(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ProvisionError::BootstrapIo {
                stage: "read local bootstrap release",
                source,
            }),
        };
        let read = |name: &str| {
            fs::read(release.join(name)).map_err(|source| ProvisionError::BootstrapIo {
                stage: "read local bootstrap release",
                source,
            })
        };
        Self::extract(
            &ployz_testkit::release_key::release_public_key(),
            archive_name,
            &read(archive_name)?,
            &read("checksums.txt")?,
            read_if_present(RELEASE_SIGNATURE_ASSET)?.as_deref(),
        )
        .await
    }

    async fn extract(
        public_key: &str,
        archive_name: &str,
        archive: &[u8],
        checksums: &[u8],
        signature: Option<&[u8]>,
    ) -> Result<Self, ProvisionError> {
        verify_signature(public_key, checksums, signature)?;
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
        verify_command_version("preflight bootstrap daemon", command, VERSION_TIMEOUT).await
    }
}

pub(super) async fn verify_command_version(
    stage: &'static str,
    command: Command,
    budget: Duration,
) -> Result<(), ProvisionError> {
    let output = command_output(command, budget)
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

/// Requests `url`, or `fallback` when `url`'s host can't be resolved or connected.
async fn send(
    client: &reqwest::Client,
    url: &str,
    fallback: Option<&str>,
) -> reqwest::Result<reqwest::Response> {
    match (client.get(url).send().await, fallback) {
        (Err(error), Some(fallback)) if error.is_connect() => client.get(fallback).send().await,
        (sent, _) => sent,
    }
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    fallback: Option<&str>,
    stage: &'static str,
) -> Result<Vec<u8>, ProvisionError> {
    send(client, url, fallback)
        .await
        .map_err(|source| ProvisionError::BootstrapDownload { stage, source })?
        .error_for_status()
        .map_err(|source| ProvisionError::BootstrapDownload { stage, source })?
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|source| ProvisionError::BootstrapDownload { stage, source })
}

/// Download `url`, or `None` when the release does not publish it.
async fn download_if_present(
    client: &reqwest::Client,
    url: &str,
    fallback: Option<&str>,
    stage: &'static str,
) -> Result<Option<Vec<u8>>, ProvisionError> {
    let response = send(client, url, fallback)
        .await
        .map_err(|source| ProvisionError::BootstrapDownload { stage, source })?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    response
        .error_for_status()
        .map_err(|source| ProvisionError::BootstrapDownload { stage, source })?
        .bytes()
        .await
        .map(|bytes| Some(bytes.to_vec()))
        .map_err(|source| ProvisionError::BootstrapDownload { stage, source })
}

fn verify_signature(
    public_key: &str,
    checksums: &[u8],
    signature: Option<&[u8]>,
) -> Result<(), ProvisionError> {
    let version = MachineVersion::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| ProvisionError::BootstrapVerification(error.to_string()))?;
    let signature = signature.ok_or_else(|| ProvisionError::BootstrapUnsigned(version.clone()))?;
    let signature = std::str::from_utf8(signature).map_err(|error| {
        ProvisionError::BootstrapVerification(format!(
            "{RELEASE_SIGNATURE_ASSET} is not UTF-8: {error}"
        ))
    })?;
    verify_release_checksums(public_key, checksums, signature, &version)
        .map_err(ProvisionError::BootstrapSignature)
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

    async fn serve_status(status: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            let response =
                format!("HTTP/1.1 {status}\r\ncontent-length: 4\r\nconnection: close\r\n\r\nbody");
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}/checksums.txt.minisig")
    }

    #[tokio::test]
    async fn missing_signature_reads_as_absent_and_other_failures_stay_errors() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        let url = serve_status("404 Not Found").await;
        let absent = download_if_present(&client, &url, None, "signature")
            .await
            .unwrap();
        assert_eq!(absent, None);

        let url = serve_status("200 OK").await;
        let present = download_if_present(&client, &url, None, "signature")
            .await
            .unwrap();
        assert_eq!(present.as_deref(), Some(b"body".as_slice()));

        let url = serve_status("500 Internal Server Error").await;
        let failed = download_if_present(&client, &url, None, "signature").await;
        assert!(matches!(
            failed,
            Err(ProvisionError::BootstrapDownload {
                stage: "signature",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn an_unreachable_release_host_falls_back_and_an_answering_one_does_not() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        // Nothing listens on a port whose listener was dropped, so connecting is refused, as
        // connecting to github.com is on a server without IPv4.
        let refused = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}/checksums.txt", listener.local_addr().unwrap())
        };

        let fallback = serve_status("200 OK").await;
        let fetched = download(&client, &refused, Some(&fallback), "checksums")
            .await
            .unwrap();
        assert_eq!(fetched, b"body");

        let failing = serve_status("500 Internal Server Error").await;
        let fallback = serve_status("200 OK").await;
        let failed = download(&client, &failing, Some(&fallback), "checksums").await;
        assert!(matches!(
            failed,
            Err(ProvisionError::BootstrapDownload {
                stage: "checksums",
                ..
            })
        ));
    }
}
