use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use super::assess::{assess, read_facts};
use super::host::install_docker;
use super::os_release::OsRelease;
use super::storage::{
    self, BuildFailure, OPENZFS_BUILD_DEPENDENCIES, OPENZFS_SHA256, ZfsRoute,
    debian_contrib_source, debian_kernel_flavour_step, download_openzfs, enable_debian_contrib,
    kernel_devel, make_openzfs_rpms, require_kernel_devel, uname, zfs_installed, zfs_preflight,
};
use super::{
    DEFAULT_RUN_DIR, Error, InstallPaths, apt_get, command_exists, refuse, require_root, run_apt,
    run_command, run_host,
};

const STAGE: &str = "prepare the host";

/// The Debian packages a DKMS build of `zfs-dkms` needs beyond the toolchain `dkms` pulls in.
const DEBIAN_ZFS_DKMS_DEPENDENCIES: [&str; 4] = ["file", "libc6-dev", "lsb-release", "perl"];

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub(super) struct PrepState {
    pub(super) docker: Timed<DockerStep>,
    pub(super) zfs: Timed<ZfsStep>,
    pub(super) added_packages: Vec<String>,
    pub(super) zfs_build: Option<ZfsBuild>,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub(super) struct Timed<S> {
    #[serde(flatten)]
    pub(super) step: S,
    pub(super) started_at: Option<String>,
    pub(super) finished_at: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub(super) enum DockerStep {
    #[default]
    Installing,
    Ready,
    Failed {
        message: String,
    },
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub(super) enum ZfsStep {
    #[default]
    Installing,
    Ready,
    Failed {
        message: String,
    },
    Skipped {
        reason: String,
    },
    Off,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "route", rename_all = "lowercase")]
pub(super) enum ZfsBuild {
    Ubuntu {
        downloaded: Vec<String>,
    },
    Dkms {
        module: String,
        version: String,
        source_dir: PathBuf,
    },
}

/// Runs preparation on this host.
///
/// # Errors
///
/// Refuses a caller that isn't root, or reports a step that failed.
pub async fn prepare(skip_zfs: bool) -> Result<(), Error> {
    require_root()?;
    let paths = InstallPaths::system(crate::machine::DEFAULT_DATA_DIR, DEFAULT_RUN_DIR);
    prepare_at(&paths, skip_zfs, async || docker_step(&paths).await).await
}

/// Removes what the ZFS step built, for a host that enrolls without ZFS.
///
/// # Errors
///
/// Refuses a caller that isn't root, or fails to remove the module, its source or the packages
/// the step added.
pub fn discard_zfs_build() -> Result<(), Error> {
    require_root()?;
    discard_at(&InstallPaths::system(
        crate::machine::DEFAULT_DATA_DIR,
        DEFAULT_RUN_DIR,
    ))
}

pub(super) fn prepare_dir(paths: &InstallPaths) -> PathBuf {
    paths.data_dir.join(crate::machine::PREPARE_DIR_NAME)
}

pub(super) fn state_path(paths: &InstallPaths) -> PathBuf {
    prepare_dir(paths).join("state.json")
}

pub(super) async fn prepare_at(
    paths: &InstallPaths,
    skip_zfs: bool,
    docker: impl AsyncFnOnce() -> Result<(), String>,
) -> Result<(), Error> {
    let Some(_lock) = lock(paths, Wait::No)? else {
        println!("Host preparation is already running");
        return Ok(());
    };
    let previous = load(paths);
    let mut job = Job {
        path: state_path(paths),
        state: PrepState {
            added_packages: previous.added_packages,
            zfs_build: previous.zfs_build,
            ..PrepState::default()
        },
    };
    job.begin_docker()?;
    let docker = match docker().await {
        Ok(()) => DockerStep::Ready,
        Err(message) => DockerStep::Failed { message },
    };
    job.finish_docker(docker)?;
    job.begin_zfs()?;
    let zfs = if skip_zfs {
        ZfsStep::Off
    } else {
        zfs_step(paths, &mut job)
    };
    job.finish_zfs(zfs)?;
    let summary = format!(
        "Docker {}, ZFS {}",
        docker_summary(&job.state.docker.step),
        zfs_summary(&job.state.zfs.step)
    );
    let failed = matches!(job.state.docker.step, DockerStep::Failed { .. })
        || matches!(job.state.zfs.step, ZfsStep::Failed { .. });
    if failed {
        Err(refuse(STAGE, summary))
    } else {
        println!("Host prepared: {summary}");
        Ok(())
    }
}

fn docker_summary(step: &DockerStep) -> String {
    match step {
        DockerStep::Installing => "installing".into(),
        DockerStep::Ready => "ready".into(),
        DockerStep::Failed { message } => format!("failed ({message})"),
    }
}

fn zfs_summary(step: &ZfsStep) -> String {
    match step {
        ZfsStep::Installing => "installing".into(),
        ZfsStep::Ready => "ready".into(),
        ZfsStep::Failed { message } => format!("failed ({message})"),
        ZfsStep::Skipped { reason } => format!("skipped ({reason})"),
        ZfsStep::Off => "off".into(),
    }
}

async fn docker_step(paths: &InstallPaths) -> Result<(), String> {
    let message = |error: Error| ployz_core::error_chain::inline(&error);
    let facts = read_facts(paths).map_err(message)?;
    if let Some(blocker) = assess(&facts).blockers.into_iter().next() {
        return Err(blocker.message);
    }
    install_docker(paths).await.map_err(message)
}

pub(super) fn load(paths: &InstallPaths) -> PrepState {
    let path = state_path(paths);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            eprintln!("ignoring unreadable {}: {error}", path.display());
            PrepState::default()
        }),
        Err(_) => PrepState::default(),
    }
}

enum Wait {
    No,
    Yes,
}

fn lock(paths: &InstallPaths, wait: Wait) -> Result<Option<File>, Error> {
    let stage = "take the host preparation lock";
    let io = |source| Error::Io { stage, source };
    let dir = prepare_dir(paths);
    fs::create_dir_all(&dir).map_err(io)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))
        .map_err(io)?;
    match wait {
        Wait::Yes => fs2::FileExt::lock_exclusive(&file).map_err(io)?,
        Wait::No => match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == fs2::lock_contended_error().kind() => return Ok(None),
            Err(error) => return Err(io(error)),
        },
    }
    Ok(Some(file))
}

pub(super) struct Job {
    path: PathBuf,
    pub(super) state: PrepState,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl Job {
    pub(super) fn save(&self) -> Result<(), Error> {
        let stage = "write the host preparation state";
        let bytes = serde_json::to_vec_pretty(&self.state).map_err(|error| Error::Io {
            stage,
            source: io::Error::other(error),
        })?;
        crate::filesystem::atomic_write(&self.path, &bytes, 0o644)
            .map_err(|source| Error::Io { stage, source })
    }

    fn begin_docker(&mut self) -> Result<(), Error> {
        self.state.docker = Timed {
            step: DockerStep::Installing,
            started_at: Some(now()),
            finished_at: None,
        };
        self.save()
    }

    fn finish_docker(&mut self, step: DockerStep) -> Result<(), Error> {
        self.state.docker.step = step;
        self.state.docker.finished_at = Some(now());
        self.save()
    }

    fn begin_zfs(&mut self) -> Result<(), Error> {
        self.state.zfs = Timed {
            step: ZfsStep::Installing,
            started_at: Some(now()),
            finished_at: None,
        };
        self.save()
    }

    fn finish_zfs(&mut self, step: ZfsStep) -> Result<(), Error> {
        self.state.zfs.step = step;
        self.state.zfs.finished_at = Some(now());
        self.save()
    }

    fn add_packages(&mut self, packages: &[String]) -> Result<(), Error> {
        for package in packages {
            if !self.state.added_packages.contains(package) {
                self.state.added_packages.push(package.clone());
            }
        }
        self.save()
    }

    fn set_build(&mut self, build: ZfsBuild) -> Result<(), Error> {
        self.state.zfs_build = Some(build);
        self.save()
    }
}

fn zfs_step(paths: &InstallPaths, job: &mut Job) -> ZfsStep {
    match build_ahead(paths, job) {
        Ok(step) => step,
        Err(Error::Command { stage, message }) if stage == storage::STAGE => {
            ZfsStep::Skipped { reason: message }
        }
        Err(error) => ZfsStep::Failed {
            message: ployz_core::error_chain::inline(&error),
        },
    }
}

fn build_ahead(paths: &InstallPaths, job: &mut Job) -> Result<ZfsStep, Error> {
    let (os, route) = zfs_preflight(paths)?;
    let kernel = uname("-r", "read running kernel")?;
    if zfs_installed(&kernel) {
        return Ok(ZfsStep::Ready);
    }
    match route {
        ZfsRoute::Ubuntu => download_ubuntu_zfs(job)?,
        ZfsRoute::Debian { codename, keyring } => {
            build_debian_zfs(paths, job, codename, keyring, &kernel)?;
        }
        ZfsRoute::AmazonLinux => build_amazon_zfs(paths, job, &os, &kernel, OPENZFS_SHA256)?,
    }
    Ok(ZfsStep::Ready)
}

/// Fills the apt cache with `zfsutils-linux`; Ubuntu's kernel already ships the module.
fn download_ubuntu_zfs(job: &mut Job) -> Result<(), Error> {
    run_apt("refresh Ubuntu packages for ZFS", ["update", "-qq"], None)?;
    run_apt(
        "download ZFS packages",
        ["install", "--download-only", "-y", "-qq", "zfsutils-linux"],
        None,
    )?;
    job.set_build(ZfsBuild::Ubuntu {
        downloaded: vec!["zfsutils-linux".into()],
    })
}

fn build_debian_zfs(
    paths: &InstallPaths,
    job: &mut Job,
    codename: &str,
    keyring: &str,
    kernel: &str,
) -> Result<(), Error> {
    if dkms_built(kernel) {
        return Ok(());
    }
    let flavour = debian_kernel_flavour_step(kernel).map_err(build_error)?;
    enable_debian_contrib(&paths.apt_dir, codename, keyring).map_err(build_error)?;
    let wanted = [
        format!("linux-headers-{kernel}"),
        format!("linux-headers-{flavour}"),
        "dkms".to_owned(),
    ]
    .into_iter()
    .chain(DEBIAN_ZFS_DKMS_DEPENDENCIES.map(str::to_owned));
    let lacked: Vec<String> = wanted.filter(|package| !deb_installed(package)).collect();
    if !lacked.is_empty() {
        job.add_packages(&lacked)?;
        let mut install = apt_get();
        install
            .args(["install", "-y", "-qq", "--no-install-recommends"])
            .args(&lacked);
        run_command("install ZFS build dependencies", &mut install)?;
    }
    let dir = fresh_build_dir(paths)?;
    run_apt("download zfs-dkms", ["download", "zfs-dkms"], Some(&dir))?;
    let archive = only_file(&dir, |name| {
        name.starts_with("zfs-dkms_") && name.ends_with(".deb")
    })?;
    let root = dir.join("root");
    let mut unpack = Command::new("dpkg-deb");
    unpack.arg("-x").arg(&archive).arg(&root);
    run_command("unpack zfs-dkms", &mut unpack)?;
    dkms_build(paths, job, &root, kernel)
}

fn build_amazon_zfs(
    paths: &InstallPaths,
    job: &mut Job,
    os: &OsRelease,
    kernel: &str,
    sha256: &str,
) -> Result<(), Error> {
    require_kernel_devel(os, kernel)?;
    if dkms_built(kernel) {
        return Ok(());
    }
    let dir = prepare_dir(paths).join("zfs");
    fs::create_dir_all(&dir).map_err(|source| Error::Io {
        stage: "create the ZFS build directory",
        source,
    })?;
    let tarball = download_openzfs(&dir, sha256).map_err(build_error)?;
    let lacked: Vec<String> = std::iter::once(kernel_devel(kernel))
        .chain(OPENZFS_BUILD_DEPENDENCIES.map(str::to_owned))
        .filter(|package| !rpm_provided(package))
        .collect();
    if !lacked.is_empty() {
        job.add_packages(&lacked)?;
        let mut install = Command::new("dnf");
        install.args(["install", "-y"]).args(&lacked);
        run_command("install ZFS build dependencies", &mut install)?;
    }
    let rpms = make_openzfs_rpms(&dir, &tarball).map_err(build_error)?;
    let dkms_rpm = rpms
        .iter()
        .find(|rpm| {
            rpm.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("zfs-dkms-"))
        })
        .ok_or_else(|| Error::Verification("the OpenZFS build made no zfs-dkms RPM".into()))?;
    let root = dir.join("root");
    remove_dir(&root)?;
    fs::create_dir_all(&root).map_err(|source| Error::Io {
        stage: "unpack the zfs-dkms RPM",
        source,
    })?;
    unpack_rpm(dkms_rpm, &root)?;
    dkms_build(paths, job, &root, kernel)
}

fn unpack_rpm(rpm: &Path, root: &Path) -> Result<(), Error> {
    let stage = "unpack the zfs-dkms RPM";
    let io = |source| Error::Io { stage, source };
    let mut archive = Command::new("rpm2cpio")
        .arg(rpm)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(io)?;
    let stdout = archive
        .stdout
        .take()
        .ok_or_else(|| io(io::Error::other("rpm2cpio has no stdout")))?;
    let mut extract = Command::new("cpio");
    extract.args(["-idm", "-D"]).arg(root).stdin(stdout);
    let extracted = run_command(stage, &mut extract);
    let converted = archive.wait().map_err(io)?;
    extracted?;
    if converted.success() {
        Ok(())
    } else {
        Err(refuse(stage, format!("rpm2cpio exited with {converted}")))
    }
}

fn fresh_build_dir(paths: &InstallPaths) -> Result<PathBuf, Error> {
    let dir = prepare_dir(paths).join("zfs");
    remove_dir(&dir)?;
    fs::create_dir_all(&dir).map_err(|source| Error::Io {
        stage: "create the ZFS build directory",
        source,
    })?;
    Ok(dir)
}

fn only_file(dir: &Path, matches: impl Fn(&str) -> bool) -> Result<PathBuf, Error> {
    fs::read_dir(dir)
        .map_err(|source| Error::Io {
            stage: "find the ZFS source",
            source,
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(&matches)
        })
        .ok_or_else(|| Error::Verification(format!("no ZFS source in {}", dir.display())))
}

fn dkms_build(paths: &InstallPaths, job: &mut Job, root: &Path, kernel: &str) -> Result<(), Error> {
    let unpacked = only_file(&root.join("usr/src"), |name| name.starts_with("zfs-"))?;
    let name = unpacked
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned();
    let version = name.trim_start_matches("zfs-").to_owned();
    let source_dir = paths.usr_src.join(&name);
    let registered = dkms_registered(&version);
    let ours = matches!(
        &job.state.zfs_build,
        Some(ZfsBuild::Dkms { version: built, .. }) if *built == version
    );
    if registered && !ours {
        run_host(
            "build ZFS with DKMS",
            "dkms",
            ["build", "-m", "zfs", "-v", &version, "-k", kernel],
        )?;
        return Ok(());
    }
    job.set_build(ZfsBuild::Dkms {
        module: "zfs".into(),
        version: version.clone(),
        source_dir: source_dir.clone(),
    })?;
    if registered {
        run_host(
            "remove the earlier ZFS build",
            "dkms",
            ["remove", "-m", "zfs", "-v", &version, "--all"],
        )?;
    }
    remove_dir(&source_dir)?;
    let mut copy = Command::new("cp");
    copy.arg("-a").arg(&unpacked).arg(&source_dir);
    run_command("copy the ZFS source", &mut copy)?;
    run_host(
        "register ZFS with DKMS",
        "dkms",
        ["add", "-m", "zfs", "-v", &version],
    )?;
    run_host(
        "build ZFS with DKMS",
        "dkms",
        ["build", "-m", "zfs", "-v", &version, "-k", kernel],
    )?;
    Ok(())
}

fn build_error(BuildFailure { step, cause }: BuildFailure) -> Error {
    Error::Command {
        stage: step.into(),
        message: ployz_core::error_chain::inline(&cause),
    }
}

fn dkms_built(kernel: &str) -> bool {
    command_exists("dkms")
        && dkms_status(&["-m", "zfs", "-k", kernel])
            .lines()
            .any(|line| {
                let line = line.trim_end();
                line.ends_with(": built") || line.ends_with(": installed")
            })
}

fn dkms_registered(version: &str) -> bool {
    command_exists("dkms") && !dkms_status(&["-m", "zfs", "-v", version]).trim().is_empty()
}

fn dkms_status(args: &[&str]) -> String {
    Command::new("dkms")
        .arg("status")
        .args(args)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
}

fn deb_installed(package: &str) -> bool {
    Command::new("dpkg-query")
        .args(["-W", "-f=${Status}", package])
        .output()
        .is_ok_and(|output| {
            output.status.success() && output.stdout.as_slice() == b"install ok installed"
        })
}

fn rpm_provided(capability: &str) -> bool {
    Command::new("rpm")
        .args(["-q", "--whatprovides", capability])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn remove_dir(dir: &Path) -> Result<(), Error> {
    match fs::remove_dir_all(dir) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(Error::Io {
            stage: "remove the ZFS build",
            source: error,
        }),
        _ => Ok(()),
    }
}

pub(super) fn discard_at(paths: &InstallPaths) -> Result<(), Error> {
    let _lock = lock(paths, Wait::Yes)?;
    let mut job = Job {
        path: state_path(paths),
        state: load(paths),
    };
    if let Some(ZfsBuild::Dkms {
        module,
        version,
        source_dir,
    }) = &job.state.zfs_build
    {
        if dkms_registered(version) {
            run_host(
                "remove the ZFS build",
                "dkms",
                ["remove", "-m", module, "-v", version, "--all"],
            )?;
        }
        remove_dir(source_dir)?;
    }
    remove_dir(&prepare_dir(paths).join("zfs"))?;
    match fs::remove_file(debian_contrib_source(&paths.apt_dir)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(Error::Io {
                stage: "remove the contrib source",
                source: error,
            });
        }
        _ => {}
    }
    let package_manager = OsRelease::read(&paths.os_release)
        .ok()
        .as_ref()
        .and_then(ZfsRoute::for_os)
        .map_or("apt-get", ZfsRoute::package_manager);
    if !job.state.added_packages.is_empty() {
        let mut remove = if package_manager == "dnf" {
            let mut dnf = Command::new("dnf");
            dnf.args(["remove", "-y"]);
            dnf
        } else {
            let mut apt = apt_get();
            apt.args(["remove", "-y", "-qq"]);
            apt
        };
        remove.args(&job.state.added_packages);
        run_command("remove ZFS build dependencies", &mut remove)?;
    }
    if matches!(job.state.zfs_build, Some(ZfsBuild::Ubuntu { .. })) {
        run_apt("clear downloaded ZFS packages", ["clean"], None)?;
    }
    job.state.added_packages.clear();
    job.state.zfs_build = None;
    job.state.zfs = Timed {
        step: ZfsStep::Off,
        started_at: None,
        finished_at: Some(now()),
    };
    if job.path.exists() {
        job.save()?;
    }
    println!("Discarded the ZFS build");
    Ok(())
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;
