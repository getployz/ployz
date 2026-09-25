//! Release selection, acquisition, verification, and activation.

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::Duration,
};

#[cfg(not(test))]
use std::os::unix::fs::chown;

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{process::Command, time::timeout};

use super::{Error, InstallPaths, daemon_archive, run_command};
use ployz_core::{MachineRelease, MachineUpgradeStage, MachineVersion};

const RELEASE_REPOSITORY: &str = "https://github.com/getployz/ployz2";
const CHANNEL_URL: &str = "https://ployz.sh";
/// Channels are scoped to this daemon's release line, so a breaking release never reaches it.
const RELEASE_LINE: &str = concat!("v", env!("CARGO_PKG_VERSION_MAJOR"));
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(test))]
const VERSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const VERSION_COMMAND_TIMEOUT: Duration = Duration::from_millis(100);

/// Where releases come from. Production always uses [`ReleaseSource::Published`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ReleaseSource {
    /// Ployz's fixed, trusted published release and channel endpoints.
    Published,
    /// A local release directory for in-process installer tests.
    #[cfg(test)]
    Local(std::path::PathBuf),
}

impl ReleaseSource {
    async fn channel(&self, name: &str) -> Result<String, Error> {
        let pointer = format!("{RELEASE_LINE}/{name}");
        let bytes = match self {
            Self::Published => {
                fetch(
                    &format!("{CHANNEL_URL}/{pointer}"),
                    "resolve release channel",
                )
                .await?
            }
            #[cfg(test)]
            Self::Local(directory) => {
                fs::read(directory.join(pointer)).map_err(|source| Error::Io {
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
            #[cfg(test)]
            Self::Local(directory) => fs::read(directory.join(file)).map_err(|source| Error::Io {
                stage: "read local release artifact",
                source,
            }),
        }
    }
}

/// Resolve `request` to one exact target. A channel stays on this daemon's release line, both for
/// its pointer and for the `installed` daemon, and never selects a release older than `installed`;
/// only an exact version crosses a line or moves a Machine backwards.
pub(super) async fn resolve_release(
    request: &MachineRelease,
    source: &ReleaseSource,
    installed: Option<&MachineVersion>,
) -> Result<MachineVersion, Error> {
    let (channel, allows_prerelease) = match request {
        MachineRelease::Exact(version) => return Ok(version.clone()),
        MachineRelease::Stable => ("stable", false),
        MachineRelease::Beta => ("beta", true),
    };
    if let Some(installed) = installed
        && !on_release_line(installed)
    {
        return Err(Error::ReleaseSelection(format!(
            "installed daemon {installed} is not on release line {RELEASE_LINE}; \
             install an exact version to cross release lines"
        )));
    }
    let pointer = parse_channel_version(&source.channel(channel).await?)?;
    if !on_release_line(&pointer) {
        return Err(Error::ReleaseSelection(format!(
            "{RELEASE_LINE} {channel} channel points at {pointer} on another release line"
        )));
    }
    if pointer.is_prerelease() && !allows_prerelease {
        return Err(Error::ReleaseSelection(format!(
            "stable channel points at prerelease {pointer}"
        )));
    }
    Ok(installed
        .filter(|installed| **installed > pointer)
        .cloned()
        .unwrap_or(pointer))
}

fn on_release_line(version: &MachineVersion) -> bool {
    format!("v{}", version.major()) == RELEASE_LINE
}

fn parse_channel_version(value: &str) -> Result<MachineVersion, Error> {
    let value = value.trim();
    let value = value.strip_prefix('v').unwrap_or(value);
    MachineVersion::parse(value).map_err(|_| Error::InvalidVersion {
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
    installed: Option<&MachineVersion>,
    target: &MachineVersion,
    progress: &mut impl FnMut(MachineUpgradeStage) -> Result<(), Error>,
) -> Result<bool, Error> {
    if installed == Some(target) {
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
    verify_executable(&daemon, target).await?;
    verify_uninstall(&uninstall)?;
    sync_staged_files(&daemon, &uninstall, stage.path())?;
    progress(MachineUpgradeStage::Activating)?;
    activate(&daemon, &uninstall, paths)?;
    Ok(true)
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
    MachineVersion::parse(String::from_utf8_lossy(&output.stdout).trim())
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
    if observed == target.to_string() {
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

fn sync_staged_files(daemon: &Path, uninstall: &Path, staging: &Path) -> Result<(), Error> {
    for path in [daemon, uninstall] {
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

fn activate(daemon: &Path, uninstall: &Path, paths: &InstallPaths) -> Result<(), Error> {
    let installed = paths.daemon();
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

    #[tokio::test]
    async fn channels_follow_this_line_and_never_downgrade() {
        let version = |rest: &str| format!("{}.{rest}", &RELEASE_LINE[1..]);
        let root = tempfile::tempdir().unwrap();
        let source = ReleaseSource::Local(root.path().to_owned());
        let line = root.path().join(RELEASE_LINE);
        fs::create_dir_all(&line).unwrap();
        // The unscoped pointer belongs to the live installer and may name a newer line.
        fs::write(root.path().join("stable"), "v99.0.0\n").unwrap();
        fs::write(line.join("stable"), format!("v{}\n", version("2.3"))).unwrap();
        fs::write(line.join("beta"), format!("v{}\n", version("3.0-beta.2"))).unwrap();
        let resolve = async |request: &MachineRelease, installed: Option<&str>| {
            let installed = installed.map(|version| MachineVersion::parse(version).unwrap());
            resolve_release(request, &source, installed.as_ref())
                .await
                .map(|version| version.to_string())
        };
        let installed = version("2.10");

        assert_eq!(
            resolve(&MachineRelease::Stable, None).await.unwrap(),
            version("2.3")
        );
        assert_eq!(
            resolve(&MachineRelease::Beta, None).await.unwrap(),
            version("3.0-beta.2")
        );
        assert_eq!(
            resolve(&MachineRelease::Stable, Some(&installed))
                .await
                .unwrap(),
            installed
        );
        assert_eq!(
            resolve(&MachineRelease::Beta, Some(&installed))
                .await
                .unwrap(),
            version("3.0-beta.2")
        );
        let older = MachineRelease::Exact(MachineVersion::parse(version("0.0")).unwrap());
        assert_eq!(
            resolve(&older, Some(&installed)).await.unwrap(),
            version("0.0")
        );

        fs::write(line.join("stable"), format!("v{}\n", version("4.0-beta.1"))).unwrap();
        assert!(matches!(
            resolve(&MachineRelease::Stable, None).await,
            Err(Error::ReleaseSelection(message)) if message.contains("prerelease")
        ));

        // A daemon installed on another line is never crossed through a channel.
        let other_line = format!("{}.0.0", RELEASE_LINE[1..].parse::<u64>().unwrap() + 1);
        assert!(matches!(
            resolve(&MachineRelease::Stable, Some(&other_line)).await,
            Err(Error::ReleaseSelection(message)) if message.contains("not on release line")
        ));

        // A pointer misfiled under this line never moves a Machine onto another line.
        fs::write(line.join("beta"), "v99.0.0\n").unwrap();
        assert!(matches!(
            resolve(&MachineRelease::Beta, None).await,
            Err(Error::ReleaseSelection(message)) if message.contains("another release line")
        ));
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
