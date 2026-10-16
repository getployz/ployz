//! Optional ZFS preparation for a fresh Machine.

use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
};

use ployz_core::StorageChoice;

use super::assess::{
    Filesystem, Virtualization, ZFS_SMOKE_BYTES, read_secure_boot_variable, read_virtualization,
    root_reserve_blocker, secure_boot_blocker, virtualization_blocker, zfs_os_route,
};
use super::os_release::OsRelease;
use super::{Error, InstallPaths, command_exists, refuse, run_apt, run_command, run_host};
use super::{
    host::write_file_atomically,
    release::{staging_directory, verify_checksum, write_private},
};

const POSIX_STAT_BLOCK_BYTES: u64 = 512;

pub(super) fn prepare_storage(storage: StorageChoice, paths: &InstallPaths) -> Result<(), Error> {
    match storage {
        StorageChoice::None => Ok(()),
        StorageChoice::Zfs => prepare_zfs(paths),
    }
}

pub(super) const STAGE: &str = "prepare ZFS storage";

/// How the ZFS kernel module becomes loadable on the Machine's distro.
#[derive(Clone, Copy)]
pub(super) enum ZfsRoute {
    /// Canonical's prebuilt module package for the running kernel.
    Ubuntu,
    /// Debian's `zfs-dkms` from `contrib`, built on the Machine and rebuilt by DKMS for new
    /// kernels. `keyring` is the path the release's own images name: apt refuses a source whose
    /// Signed-By differs from another entry for the same suite.
    Debian {
        codename: &'static str,
        keyring: &'static str,
    },
    /// The pinned [`OPENZFS_VERSION`] built on the Machine into DKMS and userspace RPMs.
    AmazonLinux,
}

impl ZfsRoute {
    pub(super) fn for_os(os: &OsRelease) -> Option<Self> {
        match (os.id.as_str(), os.version_id.as_deref()) {
            ("ubuntu", _) => Some(Self::Ubuntu),
            ("debian", Some("12")) => Some(Self::Debian {
                codename: "bookworm",
                keyring: "debian-archive-keyring.gpg",
            }),
            ("debian", Some("13")) => Some(Self::Debian {
                codename: "trixie",
                keyring: "debian-archive-keyring.pgp",
            }),
            (_, Some("2023")) if os.is_amazon_linux() => Some(Self::AmazonLinux),
            _ => None,
        }
    }

    /// Whether the Machine builds the module itself, unsigned, rather than installing Ubuntu's.
    pub(super) fn builds_module(self) -> bool {
        !matches!(self, Self::Ubuntu)
    }

    pub(super) fn prepare_minutes(self) -> u32 {
        match self {
            Self::Ubuntu => 1,
            Self::Debian { .. } => 10,
            Self::AmazonLinux => 20,
        }
    }

    pub(super) fn package_manager(self) -> &'static str {
        match self {
            Self::Ubuntu | Self::Debian { .. } => "apt-get",
            Self::AmazonLinux => "dnf",
        }
    }

    fn install(self, paths: &InstallPaths, os: &OsRelease, kernel: &str) -> Result<(), Error> {
        super::prepare::install_waiting_for_preparation(
            paths,
            self.package_manager(),
            || match self {
                Self::Ubuntu => install_zfs_packages(kernel),
                Self::Debian { codename, keyring } => {
                    prepare_debian_zfs(&paths.apt_dir, os, codename, keyring, kernel)
                }
                Self::AmazonLinux => {
                    prepare_amazon_zfs(&paths.modules_load_dir, os, kernel, OPENZFS_SHA256)
                }
            },
        )
    }
}

fn prepare_zfs(paths: &InstallPaths) -> Result<(), Error> {
    let (os, route) = zfs_preflight(paths)?;
    let kernel = uname("-r", "read running kernel")?;
    // A module this Machine built earlier stays; Ubuntu's install checks its own package.
    let install = !(route.builds_module() && zfs_installed(&kernel));
    // Before the host-root reserve, which reads the real disk, so tests can reach this refusal.
    if install && matches!(route, ZfsRoute::AmazonLinux) {
        require_kernel_devel(&os, &kernel)?;
    }
    require_host_root_reserve()?;
    let cap = zfs_arc_max()?;
    if install {
        route.install(paths, &os, &kernel)?;
    }
    // Written only once ZFS is in place, so every refusal above leaves the host untouched.
    persist_zfs_arc_max(paths, cap)?;
    run_host("load ZFS kernel module", "modprobe", ["zfs"])?;
    set_and_verify_zfs_arc_max(cap)?;
    validate_zfs()?;
    println!("ZFS storage preparation validated; no Machine Pool was created");
    Ok(())
}

pub(super) fn zfs_preflight(paths: &InstallPaths) -> Result<(OsRelease, ZfsRoute), Error> {
    let refusal = |reason: super::assess::Blocker| refuse(STAGE, reason.message);
    let os = OsRelease::read(&paths.os_release)?;
    let route = zfs_os_route(&os).map_err(refusal)?;
    if route.builds_module() {
        let secure_boot = read_secure_boot_variable(&paths.secure_boot)?;
        if let Some(reason) = secure_boot_blocker(route, &os, Some(secure_boot)) {
            return Err(refusal(reason));
        }
    }
    let (virt, unread) = read_virtualization(&paths.proc_dir);
    // Install never refused a Machine whose virtualization it couldn't detect.
    if let (Virtualization::Lxc { privileged: None }, Some(error)) = (&virt, unread) {
        return Err(error);
    }
    if let Some(reason) = virtualization_blocker(&virt) {
        return Err(refusal(reason));
    }
    let package_manager = route.package_manager();
    if !command_exists(package_manager) {
        return Err(refuse(
            STAGE,
            format!(
                "{} {package_manager} is required for ZFS storage preparation",
                os.name
            ),
        ));
    }
    Ok((os, route))
}

/// A build step that failed: its name, as the refusal shows it, and the cause for the log.
pub(super) struct BuildFailure {
    pub(super) step: &'static str,
    pub(super) cause: Error,
}

/// Names `step` as the failure of whatever error it wraps.
fn failed(step: &'static str) -> impl FnOnce(Error) -> BuildFailure {
    move |cause| BuildFailure { step, cause }
}

/// Runs one build step's command under the step's name.
fn run_step(step: &'static str, command: &mut Command) -> Result<(), BuildFailure> {
    run_command(step, command).map(drop).map_err(failed(step))
}

/// Runs `build`, then checks `kernel` has ZFS.
///
/// # Errors
///
/// Names the OS, kernel and failing step, and points at `--storage none`.
fn build_zfs_module(
    os: &OsRelease,
    kernel: &str,
    build: impl FnOnce() -> Result<(), BuildFailure>,
) -> Result<(), Error> {
    eprintln!(
        "WARNING: {} doesn't include ZFS, so Ployz is building it for kernel {kernel}. This can take up to 20 minutes on small Servers. Ubuntu includes ZFS and sets up in seconds.",
        os.display()
    );
    build()
        .and_then(|()| {
            if zfs_installed(kernel) {
                Ok(())
            } else {
                Err(failed("the ZFS module check")(Error::Verification(
                    format!("no ZFS module or tools for kernel {kernel}"),
                )))
            }
        })
        .map_err(|BuildFailure { step, cause }| {
            // The drafted message has no room for the cause; keep it in the install log.
            eprintln!("{cause}");
            refuse(STAGE, format!(
                "Couldn't build ZFS for kernel {kernel} on {}: {step} failed. Add `--storage none` to start without managed volumes.",
                os.display()
            ))
        })
}

/// Whether `kernel` has a ZFS module and the userspace tools that drive it.
pub(super) fn zfs_installed(kernel: &str) -> bool {
    command_exists("zpool")
        && command_exists("zfs")
        && Command::new("modinfo")
            .args(["-k", kernel, "zfs"])
            .output()
            .is_ok_and(|output| output.status.success())
}

/// Adds `contrib` in its own source and installs Debian's `zfs-dkms`, whose install builds the
/// module for every kernel with headers.
fn prepare_debian_zfs(
    apt_dir: &Path,
    os: &OsRelease,
    codename: &str,
    keyring: &str,
    kernel: &str,
) -> Result<(), Error> {
    build_zfs_module(os, kernel, || {
        let flavour = debian_kernel_flavour_step(kernel)?;
        enable_debian_contrib(apt_dir, codename, keyring)?;
        // The flavour's meta package pulls headers for future kernels, so DKMS can rebuild.
        let step = "installing kernel headers";
        run_apt(
            step,
            [
                "install",
                "-y",
                "-qq",
                "--no-install-recommends",
                &format!("linux-headers-{kernel}"),
                &format!("linux-headers-{flavour}"),
            ],
            None,
        )
        .map_err(failed(step))?;
        // run_apt's noninteractive frontend and closed stdin keep the CDDL debconf note from blocking.
        let step = "installing zfs-dkms";
        run_apt(
            step,
            [
                "install",
                "-y",
                "-qq",
                "--no-install-recommends",
                "zfs-dkms",
                "zfsutils-linux",
            ],
            None,
        )
        .map_err(failed(step))?;
        Ok(())
    })
}

pub(super) fn debian_contrib_source(apt_dir: &Path) -> PathBuf {
    apt_dir.join("sources.list.d/ployz-contrib.sources")
}

pub(super) fn debian_kernel_flavour_step(kernel: &str) -> Result<&str, BuildFailure> {
    debian_kernel_flavour(kernel).ok_or_else(|| {
        failed("reading the kernel flavour")(Error::Verification(format!(
            "kernel {kernel} names no Debian flavour"
        )))
    })
}

pub(super) fn enable_debian_contrib(
    apt_dir: &Path,
    codename: &str,
    keyring: &str,
) -> Result<(), BuildFailure> {
    write_file_atomically(
        &debian_contrib_source(apt_dir),
        &format!(
            "Types: deb\nURIs: http://deb.debian.org/debian\nSuites: {codename} {codename}-updates\nComponents: contrib\nSigned-By: /usr/share/keyrings/{keyring}\n\nTypes: deb\nURIs: http://security.debian.org/debian-security\nSuites: {codename}-security\nComponents: contrib\nSigned-By: /usr/share/keyrings/{keyring}\n"
        ),
        "turning on contrib",
    )
    .map_err(failed("turning on contrib"))?;
    let step = "apt-get update";
    run_apt(step, ["update", "-qq"], None)
        .map(drop)
        .map_err(failed(step))
}

/// The OpenZFS release Amazon Linux builds. Its kernel range (4.18–7.2) covers AL2023's 6.1
/// and 6.12; bumping it is a deliberate change, checksum and library packages included.
const OPENZFS_VERSION: &str = "2.4.4";
/// SHA-256 of the [`OPENZFS_VERSION`] release tarball, lowercase hex.
pub(super) const OPENZFS_SHA256: &str =
    "2a3c70d55a37cc71618a95a60e81ad66530201eb118d37741dc92efcf848c8b1";
/// The DKMS module, the userspace tools and the libraries they link.
const OPENZFS_PACKAGES: [&str; 6] = [
    "zfs-dkms",
    "zfs",
    "libzfs7",
    "libzpool7",
    "libnvpair3",
    "libuutil3",
];

/// Build dependencies of the OpenZFS RPMs, besides the running kernel's `kernel-devel`.
pub(super) const OPENZFS_BUILD_DEPENDENCIES: [&str; 20] = [
    "dkms",
    "gcc",
    "make",
    "rpm-build",
    "tar",
    "elfutils-libelf-devel",
    "libaio-devel",
    "libattr-devel",
    "libblkid-devel",
    "libffi-devel",
    "libtirpc-devel",
    "libudev-devel",
    "libuuid-devel",
    "ncompress",
    "openssl-devel",
    "python3-cffi",
    "python3-devel",
    "python3-packaging",
    "python3-setuptools",
    "zlib-devel",
];

/// The dnf capability of the headers for `kernel`; AL2023 names its provider kernel-devel or
/// kernel6.12-devel.
pub(super) fn kernel_devel(kernel: &str) -> String {
    format!("kernel-devel-uname-r = {kernel}")
}

/// Refuses when no repository offers `kernel-devel` for the running kernel.
pub(super) fn require_kernel_devel(os: &OsRelease, kernel: &str) -> Result<(), Error> {
    let providers = command_stdout(
        "find kernel-devel for the running kernel",
        "dnf",
        ["-q", "repoquery", "--whatprovides", &kernel_devel(kernel)],
    )?;
    if providers.trim().is_empty() {
        return Err(refuse(
            STAGE,
            format!(
                "{} has no kernel-devel package for the running kernel {kernel}, so ZFS can't be built for it. Update the kernel, reboot, and retry, or add `--storage none`.",
                os.display()
            ),
        ));
    }
    Ok(())
}

/// Builds the pinned OpenZFS into DKMS and userspace RPMs and installs them; DKMS builds the
/// module. `sha256` is the tarball's expected checksum.
fn prepare_amazon_zfs(
    modules_load_dir: &Path,
    os: &OsRelease,
    kernel: &str,
    sha256: &str,
) -> Result<(), Error> {
    build_zfs_module(os, kernel, || {
        let scratch = staging_directory(Path::new("/var/tmp"))
            .map_err(failed("staging the OpenZFS build"))?;
        let tarball = download_openzfs(scratch.path(), sha256)?;
        let mut dependencies = Command::new("dnf");
        dependencies
            .args(["install", "-y", &kernel_devel(kernel)])
            .args(OPENZFS_BUILD_DEPENDENCIES);
        run_step("installing build dependencies", &mut dependencies)?;
        let rpms = make_openzfs_rpms(scratch.path(), &tarball)?;
        // zfs-dkms's install builds and installs the module for the running kernel.
        let mut install = Command::new("dnf");
        install.args(["install", "-y"]).args(rpms);
        run_step("installing the ZFS RPMs", &mut install)?;
        // The RPMs enable zfs-import-cache, zfs-mount and zfs.target, but those skip unless the
        // module is loaded, and upstream leaves loading it at boot commented out. Debian's
        // zfs-load-module.service does this there.
        let step = "loading ZFS at boot";
        fs::create_dir_all(modules_load_dir)
            .map_err(|source| Error::Io {
                stage: step,
                source,
            })
            .and_then(|()| {
                write_file_atomically(&modules_load_dir.join("ployz-zfs.conf"), "zfs\n", step)
            })
            .map_err(failed(step))
    })
}

pub(super) fn download_openzfs(dir: &Path, sha256: &str) -> Result<PathBuf, BuildFailure> {
    let version = OPENZFS_VERSION;
    let name = format!("zfs-{version}.tar.gz");
    let tarball = dir.join(&name);
    let cached = fs::read(&tarball)
        .ok()
        .is_some_and(|bytes| verify_checksum(&bytes, &name, sha256).is_ok());
    if cached {
        return Ok(tarball);
    }
    let step = "downloading OpenZFS";
    let mut download = Command::new("curl");
    download
        .args([
            "--proto",
            "=https",
            "--tlsv1.2",
            "-fsSL",
            "--retry",
            "3",
            "-o",
        ])
        .arg(&tarball)
        .arg(format!(
            "https://github.com/openzfs/zfs/releases/download/zfs-{version}/{name}"
        ));
    run_step(step, &mut download)?;
    let bytes = fs::read(&tarball)
        .map_err(|source| Error::Io {
            stage: step,
            source,
        })
        .map_err(failed(step))?;
    // Checked before anything is installed, so a bad download leaves the Machine untouched.
    verify_checksum(&bytes, &name, sha256).map_err(failed("the OpenZFS checksum check"))?;
    Ok(tarball)
}

pub(super) fn make_openzfs_rpms(dir: &Path, tarball: &Path) -> Result<Vec<PathBuf>, BuildFailure> {
    let mut unpack = Command::new("tar");
    unpack
        .args(["--no-same-owner", "-xzf"])
        .arg(tarball)
        .arg("-C")
        .arg(dir);
    run_step("unpacking OpenZFS", &mut unpack)?;
    let source = dir.join(format!("zfs-{OPENZFS_VERSION}"));
    // The RPM specs configure their own builds; this only prepares `make dist`.
    let mut configure = Command::new("./configure");
    configure.arg("--with-config=user").current_dir(&source);
    run_step("configuring OpenZFS", &mut configure)?;
    // AL2023's kernel-devel carries Epoch 1, so zfs-dkms's Fedora-only kernel range pins
    // conflict with every AL2023 kernel. Building it as non-Fedora drops them.
    let step = "building the ZFS RPMs";
    let mut make = Command::new("make");
    make.args(["rpm-utils", "rpm-dkms", "RPM_DEFINE_DKMS=--undefine=fedora"])
        .current_dir(&source);
    run_step(step, &mut make)?;
    built_rpms(&source).map_err(failed(step))
}

/// The binary RPMs of [`OPENZFS_PACKAGES`] that `make` left in `source`.
fn built_rpms(source: &Path) -> Result<Vec<PathBuf>, Error> {
    let files: Vec<PathBuf> = fs::read_dir(source)
        .map_err(|source| Error::Io {
            stage: "find built ZFS RPMs",
            source,
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    OPENZFS_PACKAGES
        .iter()
        .map(|package| {
            let prefix = format!("{package}-{OPENZFS_VERSION}-");
            files
                .iter()
                .find(|path| {
                    path.file_name()
                        .and_then(OsStr::to_str)
                        .is_some_and(|name| {
                            name.starts_with(&prefix)
                                && name.ends_with(".rpm")
                                && !name.ends_with(".src.rpm")
                        })
                })
                .cloned()
                .ok_or_else(|| Error::Verification(format!("no {package} RPM was built")))
        })
        .collect()
}

/// The flavour Debian names header packages after: `6.1.0-28-cloud-amd64` → `cloud-amd64`.
fn debian_kernel_flavour(kernel: &str) -> Option<&str> {
    kernel
        .match_indices('-')
        .map(|(index, _)| &kernel[index + 1..])
        .find(|rest| !rest.starts_with(|c: char| c.is_ascii_digit()))
        .filter(|flavour| !flavour.is_empty())
}

pub(super) fn uname(arg: &str, stage: &str) -> Result<String, Error> {
    let output = run_host(stage, "uname", [arg])?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.is_empty() {
        Err(Error::Command {
            stage: stage.into(),
            message: "returned no value".into(),
        })
    } else {
        Ok(value)
    }
}

fn require_host_root_reserve() -> Result<(), Error> {
    match root_reserve_blocker(Filesystem::read_root()?) {
        Some(blocker) => Err(refuse(STAGE, blocker.message)),
        None => Ok(()),
    }
}

fn zfs_arc_max() -> Result<u64, Error> {
    let memory = fs::read_to_string("/proc/meminfo").map_err(|source| Error::Io {
        stage: "read total RAM for ZFS ARC limit",
        source,
    })?;
    let kib = memory
        .lines()
        .find_map(|line| {
            line.strip_prefix("MemTotal:")
                .and_then(|value| value.split_whitespace().next())
        })
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| refuse(STAGE, "Could not read total RAM for the ZFS ARC limit"))?;
    Ok((kib.saturating_mul(1024) / 4).clamp(256 * 1024 * 1024, 1024 * 1024 * 1024))
}

fn persist_zfs_arc_max(paths: &InstallPaths, cap: u64) -> Result<(), Error> {
    fs::create_dir_all(&paths.modprobe_dir).map_err(|source| Error::Io {
        stage: "create ZFS module configuration directory",
        source,
    })?;
    write_file_atomically(
        &paths.modprobe_dir.join("ployz-zfs.conf"),
        &format!("options zfs zfs_arc_max={cap}\n"),
        "persist ZFS ARC limit",
    )
}

fn package_has_zfs_module(files: &str, kernel: &str) -> bool {
    let marker = format!("/lib/modules/{kernel}/");
    files.lines().any(|line| {
        line.contains(&marker) && (line.ends_with("/zfs.ko") || line.contains("/zfs.ko."))
    })
}

fn installed_package_files(candidate: &str) -> Result<Option<String>, Error> {
    let output = Command::new("dpkg-query")
        .args(["-L", candidate])
        .output()
        .map_err(|source| Error::Io {
            stage: "inspect installed Ubuntu ZFS module package",
            source,
        })?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned()))
}

pub(super) fn install_zfs_packages(kernel: &str) -> Result<(), Error> {
    run_apt("refresh Ubuntu packages for ZFS", ["update", "-qq"], None)?;
    let mut package = None;
    let mut download_error = None;
    for candidate in [
        format!("linux-main-modules-zfs-{kernel}"),
        format!("linux-modules-zfs-{kernel}"),
        format!("linux-modules-{kernel}"),
        format!("linux-modules-extra-{kernel}"),
    ] {
        let mut show = Command::new("apt-cache");
        show.args(["show", &candidate]);
        if !show
            .output()
            .map_err(|source| Error::Io {
                stage: "inspect Ubuntu ZFS module packages",
                source,
            })?
            .status
            .success()
        {
            continue;
        }
        if installed_package_files(&candidate)?
            .is_some_and(|files| package_has_zfs_module(&files, kernel))
        {
            package = Some(candidate);
            break;
        }
        let scratch = staging_directory(Path::new("/var/tmp"))?;
        if let Err(error) = run_apt(
            "download Ubuntu ZFS module package",
            ["download", &candidate],
            Some(scratch.path()),
        ) {
            download_error = Some(error);
            continue;
        }
        let archive = fs::read_dir(scratch.path())
            .map_err(|source| Error::Io {
                stage: "inspect downloaded Ubuntu ZFS module package",
                source,
            })?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension() == Some(OsStr::new("deb")));
        let Some(archive) = archive else {
            return Err(Error::Verification(format!(
                "downloaded Ubuntu ZFS module package {candidate} contained no Debian archive"
            )));
        };
        let mut contents = Command::new("dpkg-deb");
        contents.args(["-c"]).arg(&archive);
        let output = run_command(
            "inspect downloaded Ubuntu ZFS module package",
            &mut contents,
        )?;
        if package_has_zfs_module(&String::from_utf8_lossy(&output.stdout), kernel) {
            package = Some(candidate);
            break;
        }
    }
    let package = package.ok_or_else(|| {
        download_error.unwrap_or_else(|| refuse(STAGE, format!(
                "Ubuntu has no packaged ZFS module for the running kernel {kernel}; install a supported Ubuntu kernel and retry"
            )))
    })?;
    run_apt(
        "install ZFS packages",
        [
            "install",
            "-y",
            "-qq",
            "--no-install-recommends",
            "zfsutils-linux",
            &package,
        ],
        None,
    )?;
    let files = command_stdout(
        "verify installed Ubuntu ZFS module package",
        "dpkg-query",
        ["-L", &package],
    )?;
    if package_has_zfs_module(&files, kernel) {
        Ok(())
    } else {
        Err(refuse(
            STAGE,
            format!(
                "Installed package {package} does not supply the ZFS module for running kernel {kernel}"
            ),
        ))
    }
}

fn command_stdout<const N: usize>(
    stage: &str,
    program: &str,
    args: [&str; N],
) -> Result<String, Error> {
    let output = run_host(stage, program, args)?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn set_and_verify_zfs_arc_max(cap: u64) -> Result<(), Error> {
    let path = Path::new("/sys/module/zfs/parameters/zfs_arc_max");
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| Error::Io {
            stage: "apply ZFS ARC limit",
            source,
        })?;
    file.write_all(cap.to_string().as_bytes())
        .map_err(|source| Error::Io {
            stage: "apply ZFS ARC limit",
            source,
        })?;
    let observed = fs::read_to_string(path).map_err(|source| Error::Io {
        stage: "verify ZFS ARC limit",
        source,
    })?;
    if observed.trim() == cap.to_string() {
        Ok(())
    } else {
        Err(Error::Verification(format!(
            "loaded ZFS module reports zfs_arc_max={} instead of the required {cap}",
            observed.trim()
        )))
    }
}

fn validate_zfs() -> Result<(), Error> {
    require_host_root_reserve()?;
    let stage = staging_directory(Path::new("/var/tmp"))?;
    let backing = stage.path().join("backing");
    write_private(&backing, b"", "create ZFS smoke backing file")?;
    let mut allocate = Command::new("fallocate");
    allocate
        .args(["-l", &ZFS_SMOKE_BYTES.to_string()])
        .arg(&backing);
    run_command("preallocate ZFS smoke backing file", &mut allocate)?;
    let metadata = backing.metadata().map_err(|source| Error::Io {
        stage: "inspect ZFS smoke backing file",
        source,
    })?;
    if metadata.blocks().saturating_mul(POSIX_STAT_BLOCK_BYTES) < ZFS_SMOKE_BYTES {
        return Err(Error::Verification(format!(
            "ZFS smoke backing file {} is sparse",
            backing.display()
        )));
    }
    let root = fs::metadata("/").map_err(|source| Error::Io {
        stage: "inspect host-root filesystem",
        source,
    })?;
    if metadata.dev() != root.dev() {
        return Err(Error::Verification(format!(
            "ZFS smoke backing file {} is not on the host root filesystem",
            backing.display()
        )));
    }
    validate_zfs_pool(stage)
}

fn validate_zfs_pool(stage: tempfile::TempDir) -> Result<(), Error> {
    let pool = format!(
        "ployz-smoke-{}",
        stage
            .path()
            .file_name()
            .expect("temporary directory name")
            .to_string_lossy()
    );
    // ZFS may own this backing file as soon as creation is attempted. A persistent path
    // cannot unlink it on error or unwind; only positively verified cleanup may delete it.
    let directory = stage.keep();
    let backing = directory.join("backing");
    let mut pool_created = false;
    let result = (|| {
        let mut create = Command::new("zpool");
        create
            .args(["create", "-f", "-m", "none", "-o", "cachefile=none", &pool])
            .arg(&backing);
        run_command("create temporary ZFS smoke Pool", &mut create)?;
        pool_created = true;
        run_host(
            "query temporary ZFS smoke Pool",
            "zpool",
            ["list", "-Hp", "-o", "name,size,alloc,free", &pool],
        )?;
        run_host(
            "query temporary ZFS smoke dataset",
            "zfs",
            ["list", "-Hp", "-o", "name,mountpoint", &pool],
        )?;
        Ok(())
    })();
    let cleanup = cleanup_zfs_smoke(&pool, &backing, pool_created)
        .and_then(|()| {
            fs::remove_dir(&directory).map_err(|source| Error::Io {
                stage: "remove temporary ZFS smoke directory",
                source,
            })
        })
        .map_err(|error| {
            Error::Verification(format!(
                "{error}; ZFS smoke backing directory retained at {} for inspection",
                directory.display(),
                error = ployz_core::error_chain::inline(&error),
            ))
        });
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(Error::Verification(format!(
            "{cleanup} (after: {error})",
            cleanup = ployz_core::error_chain::inline(&cleanup),
            error = ployz_core::error_chain::inline(&error),
        ))),
    }
}

#[cfg(test)]
mod tests;

fn cleanup_zfs_smoke(pool: &str, backing: &Path, pool_created: bool) -> Result<(), Error> {
    let existing_pool = if pool_created {
        true
    } else {
        command_stdout(
            "inspect temporary ZFS smoke Pool after failed creation",
            "zpool",
            ["list", "-H", "-o", "name"],
        )?
        .lines()
        .any(|name| name == pool)
    };
    if existing_pool {
        let mut destroy = Command::new("zpool");
        destroy.args(["destroy", "-f", pool]);
        run_command("destroy temporary ZFS smoke Pool", &mut destroy)?;
    }
    let pools = command_stdout(
        "verify temporary ZFS smoke Pool cleanup",
        "zpool",
        ["list", "-H", "-o", "name"],
    )?;
    if pools.lines().any(|name| name == pool) {
        return Err(Error::Verification(format!(
            "Temporary ZFS smoke Pool {pool} remains after destroy"
        )));
    }
    let datasets = command_stdout(
        "verify temporary ZFS smoke dataset cleanup",
        "zfs",
        ["list", "-H", "-o", "name"],
    )?;
    if datasets.lines().any(|name| {
        name == pool
            || name
                .strip_prefix(pool)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }) {
        return Err(Error::Verification(format!(
            "Temporary ZFS smoke dataset {pool} remains after destroy"
        )));
    }
    fs::remove_file(backing).map_err(|source| Error::Io {
        stage: "remove temporary ZFS smoke backing file",
        source,
    })
}
