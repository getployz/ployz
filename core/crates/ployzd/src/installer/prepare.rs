use std::{
    fs::{self, File, OpenOptions},
    io,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::{SecondsFormat, Utc};
use nix::fcntl::{FcntlArg, FdFlag};
use ployz_core::host_preparation::{
    Begun, DockerOutcome, PrepState, PrepStep, StateError, ZfsBuild, ZfsOutcome, encode_state,
    read_state,
};
use ployz_core::probe_document::OsRelease;

use super::assess::{assess, read_facts};
use super::host::{install_docker, verify_docker};
use super::os_release;
use super::storage::{
    self, BuildFailure, OPENZFS_BUILD_DEPENDENCIES, OPENZFS_SHA256, debian_contrib_source,
    debian_kernel_flavour_step, download_openzfs, enable_debian_contrib, kernel_devel,
    make_openzfs_rpms, require_kernel_devel, uname, zfs_installed, zfs_preflight,
};
use super::zfs_route::ZfsRoute;
use super::{
    Error, InstallPaths, apt_get, command_exists, refuse, require_root, run_apt, run_command,
    run_host,
};

const STAGE: &str = "prepare the host";

/// The Debian packages a DKMS build of `zfs-dkms` needs beyond the toolchain `dkms` pulls in.
const DEBIAN_ZFS_DKMS_DEPENDENCIES: [&str; 4] = ["file", "libc6-dev", "lsb-release", "perl"];

/// Runs preparation on this host.
///
/// # Errors
///
/// Refuses a caller that isn't root, or reports a step that failed.
pub async fn prepare(skip_zfs: bool) -> Result<(), Error> {
    require_root()?;
    let paths = InstallPaths::production();
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
    discard_at(&InstallPaths::production())
}

pub(super) fn prepare_dir(paths: &InstallPaths) -> PathBuf {
    paths.layout.prepare_dir()
}

pub(super) fn state_path(paths: &InstallPaths) -> PathBuf {
    paths.layout.prepare_state()
}

pub(super) async fn prepare_at(
    paths: &InstallPaths,
    skip_zfs: bool,
    docker: impl AsyncFnOnce() -> Result<(), String>,
) -> Result<(), Error> {
    let Some(held) = lock(paths, Wait::No)? else {
        println!("Host preparation is already running");
        return Ok(());
    };
    mark(&held);
    let previous = load(paths);
    // The lock is free, so a step an earlier run began and left `installing` died with it.
    let interrupted: Vec<String> = [
        (
            "docker",
            matches!(previous.docker, PrepStep::Running { .. }),
        ),
        ("zfs", matches!(previous.zfs, PrepStep::Running { .. })),
    ]
    .into_iter()
    .filter(|(_, unfinished)| *unfinished)
    .map(|(step, _)| step.to_owned())
    .collect();
    for step in &interrupted {
        println!("Redoing the {step} step an earlier run left unfinished");
    }
    let mut job = Job {
        path: state_path(paths),
        state: PrepState {
            added_packages: previous.added_packages,
            zfs_build: previous.zfs_build,
            added_contrib_source: previous.added_contrib_source,
            interrupted,
            ..PrepState::default()
        },
    };
    if debian_family(paths) {
        finish_interrupted_package_installs();
    }
    let begun = job.begin_docker()?;
    let docker = match docker().await {
        Ok(()) => DockerOutcome::Ready,
        Err(message) => DockerOutcome::Failed { message },
    };
    let summary = format!("Docker {}", docker_summary(&docker));
    let failed = matches!(docker, DockerOutcome::Failed { .. });
    job.finish_docker(begun, docker)?;
    let begun = job.begin_zfs()?;
    let zfs = if skip_zfs {
        ZfsOutcome::Off
    } else {
        zfs_step(paths, &mut job)
    };
    let summary = format!("{summary}, ZFS {}", zfs_summary(&zfs));
    let failed = failed || matches!(zfs, ZfsOutcome::Failed { .. });
    job.finish_zfs(begun, zfs)?;
    if failed {
        Err(refuse(STAGE, summary))
    } else {
        println!("Host prepared: {summary}");
        Ok(())
    }
}

fn docker_summary(outcome: &DockerOutcome) -> String {
    match outcome {
        DockerOutcome::Ready => "ready".into(),
        DockerOutcome::Failed { message } => format!("failed ({message})"),
    }
}

fn zfs_summary(outcome: &ZfsOutcome) -> String {
    match outcome {
        ZfsOutcome::Ready => "ready".into(),
        ZfsOutcome::Failed { message } => format!("failed ({message})"),
        ZfsOutcome::Skipped { reason } => format!("skipped ({reason})"),
        ZfsOutcome::Off => "off".into(),
    }
}

async fn docker_step(paths: &InstallPaths) -> Result<(), String> {
    let message = |error: Error| ployz_core::error_chain::inline(&error);
    let facts = read_facts(paths).map_err(message)?;
    if let Some(blocker) = assess(&facts).blockers.into_iter().next() {
        return Err(blocker.message);
    }
    install_docker(paths).await.map_err(message)?;
    // install_docker keeps a Docker that already exists; only asking it proves it answers.
    verify_docker().map_err(message)
}

fn debian_family(paths: &InstallPaths) -> bool {
    os_release::read(&paths.os_release)
        .is_ok_and(|os| matches!(os.id.as_str(), "debian" | "ubuntu"))
}

/// Configures packages a killed run left unpacked, which would stop every apt call after it.
/// Best effort: dpkg won't wait for a lock apt holds, and the apt calls that follow do.
fn finish_interrupted_package_installs() {
    let mut configure = Command::new("dpkg");
    configure
        .args(["--configure", "-a", "--force-confdef", "--force-confold"])
        .env("DEBIAN_FRONTEND", "noninteractive");
    if let Err(error) = run_command("finish interrupted package installs", &mut configure) {
        eprintln!("{}", ployz_core::error_chain::inline(&error));
    }
}

pub(super) fn load(paths: &InstallPaths) -> PrepState {
    let path = state_path(paths);
    match read_state(&path) {
        Ok(state) => state.unwrap_or_default(),
        Err(StateError::Malformed(error)) => {
            eprintln!("ignoring unreadable {}: {error}", path.display());
            PrepState::default()
        }
        Err(StateError::Unreadable(_)) => PrepState::default(),
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
        .open(paths.layout.prepare_lock())
        .map_err(io)?;
    // Children inherit the descriptor, so a DKMS build keeps the lock if this process is killed.
    nix::fcntl::fcntl(file.as_raw_fd(), FcntlArg::F_SETFD(FdFlag::empty()))
        .map_err(|errno| io(errno.into()))?;
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

/// Marks the preparation lock `held` as held by a run, so check-in can tell one runs: a
/// whole-file write lock on the same open file, an open file description lock, which the kernel
/// keeps for exactly as long as the flock, through the build children that inherit it. Best
/// effort: the flock alone still keeps runs from overlapping.
fn mark(held: &File) {
    let whole = nix::libc::flock {
        l_type: nix::libc::F_WRLCK as nix::libc::c_short,
        l_whence: nix::libc::SEEK_SET as nix::libc::c_short,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    };
    if let Err(errno) = nix::fcntl::fcntl(held.as_raw_fd(), FcntlArg::F_OFD_SETLK(&whole)) {
        eprintln!(
            "Couldn't mark the host preparation lock for check-in: {errno}; check-in will judge this run by ployz-prepare alone"
        );
    }
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
        let bytes = encode_state(&self.state).map_err(|error| Error::Io {
            stage,
            source: io::Error::other(error),
        })?;
        crate::filesystem::atomic_write(&self.path, &bytes, 0o644)
            .map_err(|source| Error::Io { stage, source })
    }

    fn begin_docker(&mut self) -> Result<Begun, Error> {
        let begun = Begun::at(now());
        self.state.docker = begun.running();
        self.save()?;
        Ok(begun)
    }

    fn finish_docker(&mut self, begun: Begun, outcome: DockerOutcome) -> Result<(), Error> {
        self.state.docker = begun.finish(outcome, now());
        self.save()
    }

    fn begin_zfs(&mut self) -> Result<Begun, Error> {
        let begun = Begun::at(now());
        self.state.zfs = begun.running();
        self.save()?;
        Ok(begun)
    }

    fn finish_zfs(&mut self, begun: Begun, outcome: ZfsOutcome) -> Result<(), Error> {
        self.state.zfs = begun.finish(outcome, now());
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

fn zfs_step(paths: &InstallPaths, job: &mut Job) -> ZfsOutcome {
    match build_ahead(paths, job) {
        Ok(outcome) => outcome,
        Err(Error::Refused { stage, message }) if stage == storage::STAGE => {
            ZfsOutcome::Skipped { reason: message }
        }
        Err(error) => ZfsOutcome::Failed {
            message: ployz_core::error_chain::inline(&error),
        },
    }
}

fn build_ahead(paths: &InstallPaths, job: &mut Job) -> Result<ZfsOutcome, Error> {
    let (os, route) = zfs_preflight(paths)?;
    let kernel = uname("-r", "read running kernel")?;
    if zfs_installed(&kernel) {
        return Ok(ZfsOutcome::Ready);
    }
    match route {
        ZfsRoute::Ubuntu => download_ubuntu_zfs(job)?,
        ZfsRoute::Debian { codename, keyring } => {
            build_debian_zfs(paths, job, codename, keyring, &kernel)?;
        }
        ZfsRoute::AmazonLinux => build_amazon_zfs(paths, job, &os, &kernel, OPENZFS_SHA256)?,
    }
    Ok(ZfsOutcome::Ready)
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
    if !debian_contrib_source(&paths.apt_dir).exists() {
        job.state.added_contrib_source = true;
        job.save()?;
    }
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
    dkms_build(paths, job, &root, kernel, "apt-get")
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
    let dir = paths.layout.prepare_zfs_dir();
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
    dkms_build(paths, job, &root, kernel, "dnf")
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
    let dir = paths.layout.prepare_zfs_dir();
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

fn dkms_build(
    paths: &InstallPaths,
    job: &mut Job,
    root: &Path,
    kernel: &str,
    package_manager: &str,
) -> Result<(), Error> {
    let unpacked = only_file(&root.join("usr/src"), |name| name.starts_with("zfs-"))?;
    let name = unpacked
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned();
    let version = name.trim_start_matches("zfs-").to_owned();
    let source_dir = paths.usr_src.join(&name);
    let build = ZfsBuild::Dkms {
        module: "zfs".into(),
        version: version.clone(),
        source_dir: source_dir.clone(),
    };
    let registered = dkms_registered(&version);
    let recorded = job.state.zfs_build.as_ref() == Some(&build);
    // A package may have shipped the same version since this Machine recorded it.
    let packaged = match package_ownership(package_manager, &build) {
        Ownership::Packaged => true,
        Ownership::Unpackaged => false,
        // A registration this Machine didn't make is only built, whoever owns it.
        Ownership::Unknown(_) if registered && !recorded => false,
        Ownership::Unknown(cause) => {
            return Err(refuse(
                "build ZFS with DKMS",
                format!(
                    "couldn't tell whether a package owns {}, so preparation left it as it is: {cause}",
                    source_dir.display()
                ),
            ));
        }
    };
    if packaged || (registered && !recorded) {
        if recorded {
            job.state.zfs_build = None;
            job.save()?;
        }
        let built = run_host(
            "build ZFS with DKMS",
            "dkms",
            ["build", "-m", "zfs", "-v", &version, "-k", kernel],
        );
        if packaged
            && !registered
            && let Err(error) = built
        {
            let fix = if package_manager == "dnf" {
                "dnf reinstall zfs-dkms"
            } else {
                "dpkg-reconfigure zfs-dkms"
            };
            return Err(refuse(
                "build ZFS with DKMS",
                format!(
                    "the zfs-dkms package owns {} and DKMS couldn't build it. If DKMS lost its registration, `{fix}` registers it again: {}",
                    source_dir.display(),
                    ployz_core::error_chain::inline(&error)
                ),
            ));
        }
        built?;
        return Ok(());
    }
    job.set_build(build)?;
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
    set_autoinstall(&source_dir.join("dkms.conf"), false)?;
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

fn set_autoinstall(conf: &Path, on: bool) -> Result<(), Error> {
    let stage = "set DKMS autoinstall for ZFS";
    let io = |source| Error::Io { stage, source };
    let value = if on { "yes" } else { "no" };
    let text = fs::read_to_string(conf).map_err(io)?;
    let edited: String = text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("AUTOINSTALL=") {
                format!("AUTOINSTALL=\"{value}\"\n")
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    if edited != text {
        crate::filesystem::atomic_write(conf, edited.as_bytes(), 0o644).map_err(io)?;
    }
    Ok(())
}

pub(super) struct PreparedBuild {
    version: String,
    source_dir: PathBuf,
}

pub(super) fn prepared_dkms_build(paths: &InstallPaths, kernel: &str) -> Option<PreparedBuild> {
    let Some(ZfsBuild::Dkms {
        version,
        source_dir,
        ..
    }) = load(paths).zfs_build
    else {
        return None;
    };
    dkms_status(&["-m", "zfs", "-v", &version, "-k", kernel])
        .lines()
        .any(|line| {
            let line = line.trim_end();
            line.ends_with(": built") || line.ends_with(": installed")
        })
        .then_some(PreparedBuild {
            version,
            source_dir,
        })
}

pub(super) fn install_prepared_build(build: &PreparedBuild, kernel: &str) -> Result<(), Error> {
    set_autoinstall(&build.source_dir.join("dkms.conf"), true)?;
    run_host(
        "install the prepared ZFS build",
        "dkms",
        ["install", "-m", "zfs", "-v", &build.version, "-k", kernel],
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
            output.status.success()
                && dpkg_status_installed(&String::from_utf8_lossy(&output.stdout)) == Some(true)
        })
}

/// Whether a dpkg `${Status}` (`<want> <flag> <state>`) leaves the package's files on the
/// Machine, or `None` when it isn't one. The want (`hold`, `deinstall`, `purge`) is only a wish
/// and the flag only marks a failed operation, so neither removes anything. A half-installed,
/// unpacked or half-configured package counts too, since apt would still remove it along with
/// its dependencies.
fn dpkg_status_installed(status: &str) -> Option<bool> {
    match status.split_whitespace().collect::<Vec<_>>()[..] {
        [_, _, state] => Some(!matches!(state, "not-installed" | "config-files")),
        _ => None,
    }
}

fn rpm_provided(capability: &str) -> bool {
    Command::new("rpm")
        .args(["-q", "--whatprovides", capability])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Whether a package ships a build's source, by the package manager's own answer.
enum Ownership {
    Packaged,
    Unpackaged,
    /// The query failed, so neither answer is safe to act on.
    Unknown(String),
}

fn package_ownership(package_manager: &str, build: &ZfsBuild) -> Ownership {
    match build {
        ZfsBuild::Ubuntu { .. } if deb_installed("zfsutils-linux") => Ownership::Packaged,
        ZfsBuild::Ubuntu { .. } => Ownership::Unpackaged,
        // rpm exits 1 for any failed query, and prints its negative even when the database
        // didn't open. Only that negative for this path, alone, says no package owns it.
        ZfsBuild::Dkms { source_dir, .. } if package_manager == "dnf" => {
            let path = source_dir.display();
            let unowned = [
                format!("file {path} is not owned by any package"),
                format!("error: file {path}: No such file or directory"),
            ];
            path_ownership("rpm", "-qf", source_dir, |text| {
                unowned.iter().any(|line| text.trim_end() == line)
            })
        }
        // dpkg exits 1 only when no package matches; errors exit 2.
        ZfsBuild::Dkms { source_dir, .. } => path_ownership("dpkg", "-S", source_dir, |_| true),
    }
}

fn path_ownership(
    command: &str,
    flag: &str,
    path: &Path,
    unowned: impl Fn(&str) -> bool,
) -> Ownership {
    let query = format!("{command} {flag} {}", path.display());
    let output = match Command::new(command)
        .arg(flag)
        .arg(path)
        .env("LC_ALL", "C")
        .output()
    {
        Ok(output) => output,
        Err(error) => return Ownership::Unknown(format!("`{query}` couldn't run: {error}")),
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    match output.status.code() {
        Some(0) => Ownership::Packaged,
        Some(1) if unowned(&text) => Ownership::Unpackaged,
        _ => Ownership::Unknown(format!("`{query}` {}: {}", output.status, text.trim())),
    }
}

/// Whether `zfs-dkms` is installed, or why the package manager couldn't say.
fn zfs_dkms_installed(package_manager: &str) -> Result<bool, String> {
    let dnf = package_manager == "dnf";
    let (command, args) = if dnf {
        ("rpm", ["-q", "zfs-dkms"].as_slice())
    } else {
        ("dpkg-query", ["-W", "-f=${Status}", "zfs-dkms"].as_slice())
    };
    let query = format!("{command} {}", args.join(" "));
    let output = Command::new(command)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("`{query}` couldn't run: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    match output.status.code() {
        Some(0) if dnf => Ok(true),
        Some(0) if let Some(installed) = dpkg_status_installed(&stdout) => Ok(installed),
        // dpkg-query exits 1 only for a package it doesn't know; errors exit 2.
        Some(1) if !dnf => Ok(false),
        Some(1)
            if stdout.trim_end() == "package zfs-dkms is not installed" && stderr.is_empty() =>
        {
            Ok(false)
        }
        _ => Err(format!(
            "`{query}` {}: {} {}",
            output.status,
            stdout.trim(),
            stderr.trim()
        )),
    }
}

fn remove_build(module: &str, version: &str, source_dir: &Path) -> Result<(), Error> {
    if dkms_registered(version) {
        run_host(
            "remove the ZFS build",
            "dkms",
            ["remove", "-m", module, "-v", version, "--all"],
        )?;
    }
    remove_dir(source_dir)
}

fn installed_by_dkms(build: &ZfsBuild) -> bool {
    matches!(build, ZfsBuild::Dkms { version, .. } if dkms_status(&["-m", "zfs", "-v", version])
        .lines()
        .any(|line| line.trim_end().ends_with(": installed")))
}

fn remove_unowned_build(package_manager: &str, build: Option<ZfsBuild>) -> Option<ZfsBuild> {
    let build = build?;
    // Ployz never removes an installed build, whoever owns it.
    if installed_by_dkms(&build) {
        return None;
    }
    match package_ownership(package_manager, &build) {
        Ownership::Packaged => return None,
        Ownership::Unknown(cause) => {
            eprintln!(
                "Keeping the ZFS build preparation made, since its owner is unknown: {cause}"
            );
            return Some(build);
        }
        Ownership::Unpackaged => {}
    }
    if let ZfsBuild::Dkms {
        module,
        version,
        source_dir,
    } = &build
        && let Err(error) = remove_build(module, version, source_dir)
    {
        eprintln!("Couldn't remove the ZFS {version} build preparation made: {error}");
        return Some(build);
    }
    None
}

pub(super) fn install_waiting_for_preparation(
    paths: &InstallPaths,
    package_manager: &str,
    install: impl FnOnce() -> Result<(), Error>,
) -> Result<(), Error> {
    if !state_path(paths).exists() {
        return install();
    }
    let _lock = match lock(paths, Wait::No)? {
        Some(lock) => lock,
        None => {
            println!("Waiting for host preparation to finish");
            lock(paths, Wait::Yes)?.expect("a waiting lock is always taken")
        }
    };
    install()?;
    take_over_preparation(paths, package_manager);
    Ok(())
}

fn take_over_preparation(paths: &InstallPaths, package_manager: &str) {
    let mut job = Job {
        path: state_path(paths),
        state: load(paths),
    };
    job.state.zfs_build = remove_unowned_build(package_manager, job.state.zfs_build.take());
    job.state.added_packages.clear();
    job.state.added_contrib_source = false;
    if let Err(error) = remove_dir(&paths.layout.prepare_zfs_dir()) {
        eprintln!("Couldn't remove the ZFS build files preparation downloaded: {error}");
    }
    if let Err(error) = job.save() {
        eprintln!("Couldn't record that ZFS is installed from packages: {error}");
    }
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
    let package_manager = os_release::read(&paths.os_release)
        .ok()
        .as_ref()
        .and_then(ZfsRoute::for_os)
        .map_or("apt-get", ZfsRoute::package_manager);
    match job
        .state
        .zfs_build
        .as_ref()
        .map(|build| package_ownership(package_manager, build))
    {
        Some(Ownership::Packaged) => {
            println!("ZFS is installed from packages, so there is no build to discard");
            return Ok(());
        }
        Some(Ownership::Unknown(cause)) => {
            return Err(refuse(
                "discard the ZFS build",
                format!("couldn't tell whether a package owns it, so it stays: {cause}"),
            ));
        }
        Some(Ownership::Unpackaged) | None => {}
    }
    let keep_packages = (!job.state.added_packages.is_empty() || job.state.added_contrib_source)
        && zfs_dkms_installed(package_manager).map_err(|cause| {
            refuse(
                "discard the ZFS build",
                format!(
                    "couldn't tell whether zfs-dkms is installed, so nothing was removed: {cause}"
                ),
            )
        })?;
    if let Some(ZfsBuild::Dkms {
        module,
        version,
        source_dir,
    }) = &job.state.zfs_build
    {
        remove_build(module, version, source_dir)?;
    }
    remove_dir(&paths.layout.prepare_zfs_dir())?;
    if keep_packages {
        println!("Keeping the packages ZFS was built with, which the installed zfs-dkms needs");
        job.state.added_packages.clear();
        job.state.added_contrib_source = false;
    }
    if job.state.added_contrib_source {
        match fs::remove_file(debian_contrib_source(&paths.apt_dir)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err(Error::Io {
                    stage: "remove the contrib source",
                    source: error,
                });
            }
            _ => {}
        }
    }
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
    job.state.added_contrib_source = false;
    job.state.zfs = PrepStep::Finished {
        started_at: None,
        finished_at: now(),
        outcome: ZfsOutcome::Off,
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
