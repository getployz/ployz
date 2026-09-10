//! Release selection, acquisition, verification, and activation.

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

#[cfg(not(test))]
use std::os::unix::fs::chown;

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{process::Command, time::timeout};

use super::{Error, InstallPaths, daemon_archive, run_command};
use ployz_core::{MachineUpgradeStage, MachineVersion};

const RELEASE_REPOSITORY: &str = "https://github.com/getployz/ployz2";
const CHANNEL_URL: &str = "https://ployz.sh";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(test))]
const VERSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const VERSION_COMMAND_TIMEOUT: Duration = Duration::from_millis(100);

/// A trusted release target selected at the CLI boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseRequest {
    /// Resolve the stable channel once for this installation attempt.
    Stable,
    /// Resolve the beta channel once for this installation attempt.
    Beta,
    /// Install this exact published daemon version.
    Exact(MachineVersion),
}

impl FromStr for ReleaseRequest {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.strip_prefix('v').unwrap_or(value);
        match value {
            "" | "latest" | "stable" => Ok(Self::Stable),
            "beta" => Ok(Self::Beta),
            "nightly" => Err(Error::Nightly),
            value => MachineVersion::parse(value.to_owned())
                .map(Self::Exact)
                .map_err(|_| Error::InvalidVersion {
                    value: value.to_owned(),
                }),
        }
    }
}

/// A release source that is fixed by the local installer invocation.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseSource {
    /// Ployz's fixed, trusted published release and channel endpoints.
    Published,
    /// An operator-provided local release directory used only by offline qualification.
    Local(PathBuf),
}

impl ReleaseSource {
    fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }

    async fn channel(&self, name: &str) -> Result<String, Error> {
        let bytes = match self {
            Self::Published => {
                fetch(&format!("{CHANNEL_URL}/{name}"), "resolve release channel").await?
            }
            Self::Local(directory) => {
                fs::read(directory.join(name)).map_err(|source| Error::Io {
                    stage: "read local release channel",
                    source,
                })?
            }
        };
        String::from_utf8(bytes).map_err(|error| {
            Error::ReleaseSelection(format!("{name} channel is not UTF-8: {error}"))
        })
    }

    async fn release_file(
        &self,
        target: &MachineVersion,
        file: &str,
        stage: &'static str,
    ) -> Result<Vec<u8>, Error> {
        match self {
            Self::Published => fetch(&release_url(target, file), stage).await,
            Self::Local(directory) => fs::read(directory.join(file)).map_err(|source| Error::Io {
                stage: "read local release artifact",
                source,
            }),
        }
    }
}

pub(super) async fn resolve_release(
    request: &ReleaseRequest,
    source: &ReleaseSource,
) -> Result<MachineVersion, Error> {
    match request {
        ReleaseRequest::Stable => {
            let value = source.channel("stable").await?;
            parse_channel_version(value.trim())
        }
        ReleaseRequest::Beta => {
            let value = source.channel("beta").await?;
            parse_channel_version(value.trim())
        }
        ReleaseRequest::Exact(version) => Ok(version.clone()),
    }
}

fn parse_channel_version(value: &str) -> Result<MachineVersion, Error> {
    MachineVersion::parse(value.to_owned()).map_err(|_| Error::InvalidVersion {
        value: value.to_owned(),
    })
}

pub(super) async fn fetch(url: &str, stage: &str) -> Result<Vec<u8>, Error> {
    let client = reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(DOWNLOAD_TIMEOUT)
        .read_timeout(DOWNLOAD_TIMEOUT)
        .user_agent("ployzd-installer")
        .build()
        .map_err(|error| Error::ReleaseSelection(format!("build download client: {error}")))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| Error::ReleaseSelection(format!("{stage}: {error}")))?
        .error_for_status()
        .map_err(|error| Error::ReleaseSelection(format!("{stage}: {error}")))?;
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| Error::ReleaseSelection(format!("{stage}: {error}")))
}

fn release_url(target: &MachineVersion, file: &str) -> String {
    format!("{RELEASE_REPOSITORY}/releases/download/v{target}/{file}")
}

pub(super) async fn install_binaries(
    source: &ReleaseSource,
    paths: &InstallPaths,
    target: &MachineVersion,
    progress: &mut impl FnMut(MachineUpgradeStage) -> Result<(), Error>,
) -> Result<bool, Error> {
    let installed = installed_release(&paths.bin_dir.join("ployzd")).await?;
    let helper = installed_release(&paths.bin_dir.join("ployzd-tailcat"))
        .await
        .ok()
        .flatten();
    let replace = replacement_required(source, installed.as_ref(), helper.as_ref(), target);
    if !replace {
        println!(
            "ployzd {} retained",
            installed.expect("a skipped replacement has an installed release")
        );
        return Ok(false);
    }

    progress(MachineUpgradeStage::Acquiring)?;
    let archive = daemon_archive()?;
    let stage = staging_directory(&paths.bin_dir)?;
    let archive_path = stage.path().join(archive);
    let checksum = release_checksum(source, target, archive).await?;
    let archive_bytes = source
        .release_file(target, archive, "download daemon archive")
        .await?;
    progress(MachineUpgradeStage::Verifying)?;
    verify_checksum(&archive_bytes, archive, &checksum)?;
    write_private(&archive_path, &archive_bytes, "stage daemon archive")?;
    extract_archive(&archive_path, stage.path())?;
    let daemon = stage.path().join("ployzd");
    let uninstall = stage.path().join("ployz-uninstall");
    let helper = stage.path().join("ployz-tailcat");
    verify_executable(&daemon, target).await?;
    verify_executable(&helper, target).await?;
    verify_uninstall(&uninstall)?;
    sync_staged_files(&daemon, &uninstall, &helper, stage.path())?;
    progress(MachineUpgradeStage::Activating)?;
    activate(&daemon, &uninstall, &helper, paths)?;
    Ok(true)
}

fn replacement_required(
    source: &ReleaseSource,
    installed: Option<&MachineVersion>,
    helper: Option<&MachineVersion>,
    target: &MachineVersion,
) -> bool {
    source.is_local() || installed != Some(target) || helper != Some(target)
}

pub(super) async fn installed_release(path: &Path) -> Result<Option<MachineVersion>, Error> {
    if !path.is_file() {
        return Ok(None);
    }
    let output = version_command(path, "inspect installed daemon").await?;
    if !output.status.success() {
        return Err(Error::Verification(format!(
            "installed daemon version check exited with {}",
            output.status
        )));
    }
    MachineVersion::parse(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .map(Some)
        .map_err(|_| Error::Verification("installed daemon reported an invalid version".into()))
}

async fn release_checksum(
    source: &ReleaseSource,
    target: &MachineVersion,
    archive: &str,
) -> Result<String, Error> {
    let checksums = source
        .release_file(target, "checksums.txt", "download release checksums")
        .await?;
    checksum_for(&checksums, archive)
        .ok_or_else(|| Error::Verification(format!("checksums.txt has no hash for {archive}")))
}

pub(super) fn checksum_for(checksums: &[u8], archive: &str) -> Option<String> {
    let checksums = std::str::from_utf8(checksums).ok()?;
    checksums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hash = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        (name == archive && valid_checksum(hash)).then(|| hash.to_ascii_lowercase())
    })
}

fn valid_checksum(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn verify_checksum(bytes: &[u8], archive: &str, expected: &str) -> Result<(), Error> {
    let actual = hex::encode(Sha256::digest(bytes));
    if actual == expected {
        Ok(())
    } else {
        Err(Error::Verification(format!(
            "{} checksum was {actual}, expected {expected}",
            Path::new(archive)
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("daemon archive")
        )))
    }
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<(), Error> {
    let mut command = std::process::Command::new("tar");
    command
        // Release archives are produced by a different build user. The daemon must not inherit
        // that user's numeric ownership when root installs it on a Machine.
        .args(["--no-same-owner", "-xzf"])
        .arg(archive)
        .arg("-C")
        .arg(destination);
    run_command("extract staged daemon archive", &mut command)?;
    Ok(())
}

async fn verify_executable(path: &Path, target: &MachineVersion) -> Result<(), Error> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|source| Error::Io {
        stage: "mark staged daemon executable",
        source,
    })?;
    let output = version_command(path, "preflight staged daemon").await?;
    if !output.status.success() {
        return Err(Error::Verification(format!(
            "staged daemon version check exited with {}",
            output.status
        )));
    }
    let observed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if observed == target.as_str() {
        Ok(())
    } else {
        Err(Error::Verification(format!(
            "staged daemon reported version {observed:?}, expected {target}"
        )))
    }
}

async fn version_command(path: &Path, stage: &str) -> Result<std::process::Output, Error> {
    let mut command = Command::new(path);
    command.arg("version").kill_on_drop(true);
    match timeout(VERSION_COMMAND_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(Error::Command {
            stage: stage.into(),
            message: error.to_string(),
        }),
        Err(_) => Err(Error::Command {
            stage: stage.into(),
            message: format!("timed out after {}ms", VERSION_COMMAND_TIMEOUT.as_millis()),
        }),
    }
}

fn verify_uninstall(path: &Path) -> Result<(), Error> {
    let metadata = path.metadata().map_err(|source| Error::Io {
        stage: "preflight staged uninstall command",
        source,
    })?;
    if metadata.is_file() && metadata.len() != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|source| Error::Io {
            stage: "mark staged uninstall command executable",
            source,
        })
    } else {
        Err(Error::Verification(
            "daemon archive does not contain a usable ployz-uninstall command".into(),
        ))
    }
}

fn sync_staged_files(
    daemon: &Path,
    uninstall: &Path,
    helper: &Path,
    staging: &Path,
) -> Result<(), Error> {
    for path in [daemon, uninstall, helper] {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|source| Error::Io {
                stage: "persist verified staged release",
                source,
            })?;
    }
    File::open(staging)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| Error::Io {
            stage: "persist verified staging directory",
            source,
        })
}

pub(super) fn staging_directory(parent: &Path) -> Result<TempDir, Error> {
    fs::create_dir_all(parent).map_err(|source| Error::Io {
        stage: "create daemon installation directory",
        source,
    })?;
    tempfile::Builder::new()
        .prefix(".ployz-stage-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)
        .map_err(|source| Error::Io {
            stage: "create private daemon staging directory",
            source,
        })
}

pub(super) fn write_private(path: &Path, bytes: &[u8], stage: &'static str) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| Error::Io { stage, source })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| Error::Io { stage, source })
}

fn activate(
    daemon: &Path,
    uninstall: &Path,
    helper: &Path,
    paths: &InstallPaths,
) -> Result<(), Error> {
    let installed = paths.bin_dir.join("ployzd");
    let previous = paths.bin_dir.join("ployzd.previous");
    let directory = File::open(&paths.bin_dir).map_err(|source| Error::Io {
        stage: "open daemon installation directory",
        source,
    })?;
    match fs::remove_file(&previous) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::Io {
                stage: "remove previous daemon release",
                source,
            });
        }
    }
    if installed.exists() {
        fs::hard_link(&installed, &previous).map_err(|source| Error::Io {
            stage: "retain previous daemon release",
            source,
        })?;
        root_ownership(&previous, "secure retained previous daemon release")?;
    }
    directory.sync_all().map_err(|source| Error::Io {
        stage: "persist previous daemon release",
        source,
    })?;
    fs::rename(daemon, &installed).map_err(|source| Error::Io {
        stage: "activate verified daemon",
        source,
    })?;
    root_ownership(&installed, "secure activated daemon")?;
    directory.sync_all().map_err(|source| Error::Io {
        stage: "persist daemon activation",
        source,
    })?;
    let installed_helper = paths.bin_dir.join("ployzd-tailcat");
    fs::rename(helper, &installed_helper).map_err(|source| Error::Io {
        stage: "activate Tailcat helper",
        source,
    })?;
    root_ownership(&installed_helper, "secure activated Tailcat helper")?;
    let installed_uninstall = paths.bin_dir.join("ployz-uninstall");
    fs::rename(uninstall, &installed_uninstall).map_err(|source| Error::Io {
        stage: "activate uninstall command",
        source,
    })?;
    root_ownership(&installed_uninstall, "secure activated uninstall command")?;
    directory.sync_all().map_err(|source| Error::Io {
        stage: "persist uninstall command activation",
        source,
    })
}

fn root_ownership(path: &Path, stage: &'static str) -> Result<(), Error> {
    #[cfg(not(test))]
    chown(path, Some(0), Some(0)).map_err(|source| Error::Io { stage, source })?;
    #[cfg(test)]
    let _ = (path, stage);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn published_release_requests_accept_only_trusted_shapes() {
        assert!(matches!("stable".parse(), Ok(ReleaseRequest::Stable)));
        assert!(matches!("beta".parse(), Ok(ReleaseRequest::Beta)));
        assert_eq!(
            "v1.2.3-beta.4".parse::<ReleaseRequest>().unwrap(),
            ReleaseRequest::Exact(MachineVersion::parse("1.2.3-beta.4").unwrap())
        );
        for invalid in ["1.2", "1.2.3-rc.1", "1.2.3-beta.x", "1.2.3+build.1"] {
            assert!(invalid.parse::<ReleaseRequest>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn checksum_parser_requires_the_requested_daemon_archive() {
        let archive = "ployzd_linux_amd64.tar.gz";
        let checksum = "2d20468cbeb9745b56fdf16897f963773baeecf45c4ea2e00a46e8d82da6ee9f";
        assert_eq!(
            checksum_for(format!("{checksum}  {archive}\n").as_bytes(), archive),
            Some(checksum.into())
        );
        assert_eq!(checksum_for(b"bad  other.tar.gz\n", archive), None);
    }

    #[test]
    fn resolved_channels_replace_the_exact_target_and_skip_only_the_same_target() {
        let target = MachineVersion::parse("1.2.3").unwrap();
        for helper in [None, Some(MachineVersion::parse("1.2.2").unwrap())] {
            assert!(replacement_required(
                &ReleaseSource::Published,
                Some(&target),
                helper.as_ref(),
                &target
            ));
        }
        assert!(!replacement_required(
            &ReleaseSource::Published,
            Some(&target),
            Some(&target),
            &target
        ));
        assert!(replacement_required(
            &ReleaseSource::Published,
            Some(&MachineVersion::parse("1.2.4").unwrap()),
            Some(&target),
            &target
        ));
        assert!(replacement_required(
            &ReleaseSource::Local(PathBuf::from("qualification")),
            Some(&target),
            Some(&target),
            &target
        ));
    }

    #[test]
    fn staging_directory_is_private_and_removed_on_drop() {
        let parent = tempfile::tempdir().unwrap();
        let path = {
            let staging = staging_directory(parent.path()).unwrap();
            assert_eq!(
                staging.path().metadata().unwrap().permissions().mode() & 0o777,
                0o700
            );
            staging.path().to_owned()
        };
        assert!(!path.exists());
    }
}
