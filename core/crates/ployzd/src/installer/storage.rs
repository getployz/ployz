//! Optional ZFS preparation for a fresh Machine.

use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
};

use ployz_core::StorageChoice;

use super::{Error, InstallPaths, command_exists, run_apt, run_command, run_host};
use super::{
    host::write_file_atomically,
    release::{staging_directory, verify_checksum, write_private},
};

const ZFS_SMOKE_BYTES: u64 = 128 * 1024 * 1024;
const POSIX_STAT_BLOCK_BYTES: u64 = 512;

pub(super) fn prepare_storage(storage: StorageChoice, paths: &InstallPaths) -> Result<(), Error> {
    match storage {
        StorageChoice::None => Ok(()),
        StorageChoice::Zfs => prepare_zfs(paths),
    }
}

/// Distros named in the refusal; each has a [`ZfsRoute`].
const SUPPORTED_DISTROS: &str = "Ubuntu LTS, Debian 12–13 or Amazon Linux 2023";

/// How the ZFS kernel module becomes loadable on the Machine's distro.
#[derive(Clone, Copy)]
enum ZfsRoute {
    /// Canonical's prebuilt module package for the running kernel.
    Ubuntu,
    /// Debian's `zfs-dkms` from `contrib`, built on the Machine and rebuilt by DKMS for new kernels.
    Debian,
    /// The pinned [`OPENZFS`] release built on the Machine into DKMS and userspace RPMs.
    AmazonLinux,
}

impl ZfsRoute {
    fn for_os(os: &OsRelease) -> Result<Self, Error> {
        match (os.id.as_str(), os.version_id.as_str()) {
            ("ubuntu", _) => Ok(Self::Ubuntu),
            ("debian", "12" | "13") => Ok(Self::Debian),
            ("amzn", "2023") => Ok(Self::AmazonLinux),
            _ => Err(Error::Command {
                stage: "prepare ZFS storage".into(),
                message: format!(
                    "Managed volumes need {SUPPORTED_DISTROS}; this Server runs {}. Use one of those, or add `--storage none`.",
                    os.display()
                ),
            }),
        }
    }
}

fn prepare_zfs(paths: &InstallPaths) -> Result<(), Error> {
    let os = OsRelease::read(&paths.os_release)?;
    let route = ZfsRoute::for_os(&os)?;
    // Unsigned modules this Machine builds can't load under Secure Boot; refuse before installing.
    if matches!(route, ZfsRoute::Debian | ZfsRoute::AmazonLinux) && secure_boot_enabled(paths)? {
        return Err(Error::Command {
            stage: "prepare ZFS storage".into(),
            message: format!(
                "Secure Boot is on, so this Server can't load the ZFS module Ployz builds for {}. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
                os.name
            ),
        });
    }
    let container = container_virtualization();
    if container == "openvz" || (Path::new("/proc/vz").is_dir() && !Path::new("/proc/bc").is_dir())
    {
        return Err(Error::Command {
            stage: "prepare ZFS storage".into(),
            message: "OpenVZ does not allow this Machine to load the host ZFS kernel module".into(),
        });
    }
    if container == "lxc" && lxc_is_unprivileged()? {
        return Err(Error::Command {
            stage: "prepare ZFS storage".into(),
            message:
                "Unprivileged LXC does not allow this Machine to load the host ZFS kernel module"
                    .into(),
        });
    }
    let package_manager = match route {
        ZfsRoute::Ubuntu | ZfsRoute::Debian => "apt-get",
        ZfsRoute::AmazonLinux => "dnf",
    };
    if !command_exists(package_manager) {
        return Err(Error::Command {
            stage: "prepare ZFS storage".into(),
            message: format!(
                "{} {package_manager} is required for ZFS storage preparation",
                os.name
            ),
        });
    }
    let kernel = uname("-r", "read running kernel")?;
    require_host_root_reserve(ZFS_SMOKE_BYTES)?;
    let cap = zfs_arc_max()?;
    persist_zfs_arc_max(paths, cap)?;
    match route {
        ZfsRoute::Ubuntu => install_zfs_packages(&kernel)?,
        ZfsRoute::Debian => prepare_debian_zfs(paths, &os, &kernel)?,
        ZfsRoute::AmazonLinux => prepare_amazon_zfs(&os, &kernel, &OPENZFS)?,
    }
    run_host("load ZFS kernel module", "modprobe", ["zfs"])?;
    set_and_verify_zfs_arc_max(cap)?;
    validate_zfs()?;
    println!("ZFS storage preparation validated; no Machine Pool was created");
    Ok(())
}

/// The os-release fields that pick a [`ZfsRoute`] and name the OS in a refusal.
pub(super) struct OsRelease {
    id: String,
    name: String,
    version_id: String,
}

impl OsRelease {
    pub(super) fn read(path: &Path) -> Result<Self, Error> {
        let value = fs::read_to_string(path).map_err(|source| Error::Io {
            stage: "identify Linux distribution for ZFS storage preparation",
            source,
        })?;
        let field = |key: &str| {
            value
                .lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .map(|field| field.trim().trim_matches(['"', '\'']).to_owned())
                .filter(|field| !field.is_empty())
        };
        let Some(id) = field("ID") else {
            return Err(Error::Command {
                stage: "prepare ZFS storage".into(),
                message: "Could not identify the Linux distribution for ZFS storage preparation"
                    .into(),
            });
        };
        Ok(Self {
            name: field("NAME").unwrap_or_else(|| id.clone()),
            version_id: field("VERSION_ID").unwrap_or_default(),
            id,
        })
    }

    /// `NAME VERSION_ID`, e.g. "Amazon Linux 2023"; rolling releases have no version.
    fn display(&self) -> String {
        format!("{} {}", self.name, self.version_id)
            .trim_end()
            .to_owned()
    }
}

/// Firmware Secure Boot state; a Machine without EFI variables boots with it off.
pub(super) fn secure_boot_enabled(paths: &InstallPaths) -> Result<bool, Error> {
    match fs::read(&paths.secure_boot) {
        // The variable is 4 attribute bytes followed by one data byte; 1 means on.
        Ok(variable) => Ok(variable.last() == Some(&1)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(Error::Io {
            stage: "read Secure Boot state",
            source,
        }),
    }
}

/// A failed step of a route that builds ZFS, named for the user, and its cause.
type BuildFailure = (&'static str, Error);

/// Runs `build` unless `kernel` already has ZFS, then checks the module exists.
///
/// # Errors
///
/// Names the OS, kernel and failing step, and points at `--storage none`.
fn build_zfs_module(
    os: &OsRelease,
    kernel: &str,
    build: impl FnOnce() -> Result<(), BuildFailure>,
) -> Result<(), Error> {
    if zfs_installed(kernel) {
        return Ok(());
    }
    println!("Building ZFS for kernel {kernel}. This takes a few minutes.");
    build()
        .and_then(|()| {
            if zfs_installed(kernel) {
                Ok(())
            } else {
                Err((
                    "the ZFS module check",
                    Error::Verification(format!("no ZFS module or tools for kernel {kernel}")),
                ))
            }
        })
        .map_err(|(step, cause)| {
            // The drafted message has no room for the cause; keep it in the install log.
            eprintln!("{cause}");
            Error::Command {
                stage: "prepare ZFS storage".into(),
                message: format!(
                    "Couldn't build ZFS for kernel {kernel} on {}: {step} failed. Add `--storage none` to start without managed volumes.",
                    os.display()
                ),
            }
        })
}

/// Whether `kernel` has a ZFS module and the userspace tools that drive it.
fn zfs_installed(kernel: &str) -> bool {
    command_exists("zpool")
        && command_exists("zfs")
        && Command::new("modinfo")
            .args(["-k", kernel, "zfs"])
            .output()
            .is_ok_and(|output| output.status.success())
}

/// Installs Debian's `zfs-dkms`, whose install builds the module for every kernel with headers.
pub(super) fn prepare_debian_zfs(
    paths: &InstallPaths,
    os: &OsRelease,
    kernel: &str,
) -> Result<(), Error> {
    build_zfs_module(os, kernel, || {
        let Some(flavour) = debian_kernel_flavour(kernel) else {
            return Err((
                "reading the kernel flavour",
                Error::Verification(format!("kernel {kernel} names no Debian flavour")),
            ));
        };
        enable_debian_contrib(&paths.apt_dir).map_err(|error| ("turning on contrib", error))?;
        run_apt("refresh Debian packages for ZFS", ["update", "-qq"], None)
            .map_err(|error| ("apt-get update", error))?;
        // The flavour's meta package pulls headers for future kernels, so DKMS can rebuild.
        run_apt(
            "install kernel headers",
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
        .map_err(|error| ("installing kernel headers", error))?;
        // run_apt's noninteractive frontend and closed stdin keep the CDDL debconf note from blocking.
        run_apt(
            "install ZFS packages",
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
        .map_err(|error| ("installing zfs-dkms", error))?;
        Ok(())
    })
}

/// The OpenZFS release Amazon Linux builds. Its kernel range (4.18–7.2) covers AL2023's 6.1
/// and 6.12; bumping it is a deliberate change, checksum and library packages included.
pub(super) const OPENZFS: OpenZfsRelease<'static> = OpenZfsRelease {
    version: "2.4.4",
    sha256: "2a3c70d55a37cc71618a95a60e81ad66530201eb118d37741dc92efcf848c8b1",
    packages: &[
        "zfs-dkms",
        "zfs",
        "libzfs7",
        "libzpool7",
        "libnvpair3",
        "libuutil3",
    ],
};

/// A pinned OpenZFS source release and the RPMs of it a Machine installs.
pub(super) struct OpenZfsRelease<'pin> {
    pub(super) version: &'pin str,
    /// SHA-256 of the release tarball, lowercase hex.
    pub(super) sha256: &'pin str,
    /// The DKMS module, the userspace tools and the libraries they link.
    pub(super) packages: &'pin [&'pin str],
}

/// Build dependencies of the OpenZFS RPMs, besides the running kernel's `kernel-devel`.
const OPENZFS_BUILD_DEPENDENCIES: [&str; 20] = [
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

/// Builds `release` into DKMS and userspace RPMs and installs them; DKMS builds the module.
pub(super) fn prepare_amazon_zfs(
    os: &OsRelease,
    kernel: &str,
    release: &OpenZfsRelease,
) -> Result<(), Error> {
    // AL2023 names it kernel-devel or kernel6.12-devel; both provide this for their kernel.
    let kernel_devel = format!("kernel-devel-uname-r = {kernel}");
    if !zfs_installed(kernel) {
        let providers = command_stdout(
            "find kernel-devel for the running kernel",
            "dnf",
            ["-q", "repoquery", "--whatprovides", &kernel_devel],
        )?;
        if providers.trim().is_empty() {
            return Err(Error::Command {
                stage: "prepare ZFS storage".into(),
                message: format!(
                    "{} has no kernel-devel package for the running kernel {kernel}, so ZFS can't be built for it. Update the kernel, reboot, and retry, or add `--storage none`.",
                    os.display()
                ),
            });
        }
    }
    build_zfs_module(os, kernel, || {
        let version = release.version;
        let scratch = staging_directory(Path::new("/var/tmp"))
            .map_err(|error| ("staging the OpenZFS build", error))?;
        let tarball = scratch.path().join(format!("zfs-{version}.tar.gz"));
        let mut download = Command::new("curl");
        download
            .args(["--proto", "=https", "--tlsv1.2", "-fsSL", "--retry", "3", "-o"])
            .arg(&tarball)
            .arg(format!(
                "https://github.com/openzfs/zfs/releases/download/zfs-{version}/zfs-{version}.tar.gz"
            ));
        run_command("download OpenZFS", &mut download)
            .map_err(|error| ("downloading OpenZFS", error))?;
        let bytes = fs::read(&tarball).map_err(|source| {
            (
                "downloading OpenZFS",
                Error::Io {
                    stage: "read OpenZFS download",
                    source,
                },
            )
        })?;
        // Checked before anything is installed, so a bad download leaves the Machine untouched.
        verify_checksum(&bytes, &format!("zfs-{version}.tar.gz"), release.sha256)
            .map_err(|error| ("the OpenZFS checksum check", error))?;
        let mut dependencies = Command::new("dnf");
        dependencies
            .args(["install", "-y", &kernel_devel])
            .args(OPENZFS_BUILD_DEPENDENCIES);
        run_command("install ZFS build dependencies", &mut dependencies)
            .map_err(|error| ("installing build dependencies", error))?;
        let mut unpack = Command::new("tar");
        unpack
            .args(["--no-same-owner", "-xzf"])
            .arg(&tarball)
            .arg("-C")
            .arg(scratch.path());
        run_command("unpack OpenZFS", &mut unpack).map_err(|error| ("unpacking OpenZFS", error))?;
        let source = scratch.path().join(format!("zfs-{version}"));
        // The RPM specs configure their own builds; this only prepares `make dist`.
        let mut configure = Command::new("./configure");
        configure.arg("--with-config=user").current_dir(&source);
        run_command("configure OpenZFS", &mut configure)
            .map_err(|error| ("configuring OpenZFS", error))?;
        // AL2023's kernel-devel carries Epoch 1, so zfs-dkms's Fedora-only kernel range pins
        // conflict with every AL2023 kernel. Building it as non-Fedora drops them.
        let mut make = Command::new("make");
        make.args(["rpm-utils", "rpm-dkms", "RPM_DEFINE_DKMS=--undefine=fedora"])
            .current_dir(&source);
        run_command("build ZFS RPMs", &mut make)
            .map_err(|error| ("building the ZFS RPMs", error))?;
        let rpms =
            built_rpms(&source, release).map_err(|error| ("building the ZFS RPMs", error))?;
        // zfs-dkms's install builds and installs the module for the running kernel.
        let mut install = Command::new("dnf");
        install.args(["install", "-y"]).args(rpms);
        run_command("install ZFS RPMs", &mut install)
            .map_err(|error| ("installing the ZFS RPMs", error))?;
        Ok(())
    })
}

/// The binary RPMs of `release.packages` that `make` left in `source`.
fn built_rpms(source: &Path, release: &OpenZfsRelease) -> Result<Vec<PathBuf>, Error> {
    let files: Vec<PathBuf> = fs::read_dir(source)
        .map_err(|source| Error::Io {
            stage: "find built ZFS RPMs",
            source,
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    release
        .packages
        .iter()
        .map(|package| {
            let prefix = format!("{package}-{}-", release.version);
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

/// Turns on `contrib` for Debian-origin apt entries in both source formats, leaving the rest alone.
fn enable_debian_contrib(apt_dir: &Path) -> Result<(), Error> {
    let mut files = vec![apt_dir.join("sources.list")];
    if let Ok(entries) = fs::read_dir(apt_dir.join("sources.list.d")) {
        files.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
    }
    for file in files {
        let edit = match file.extension().and_then(OsStr::to_str) {
            Some("list") => enable_contrib_one_line,
            Some("sources") => enable_contrib_deb822,
            _ => continue,
        };
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(Error::Io {
                    stage: "read apt sources",
                    source,
                });
            }
        };
        let edited = edit(&text);
        if edited != text {
            write_file_atomically(&file, &edited, "turn on Debian contrib")?;
        }
    }
    Ok(())
}

/// A Debian entry that lacks `contrib`: `main` from a `/debian` or `/debian-security` archive.
/// That covers deb.debian.org, provider mirrors and Debian cloud images' `mirror+file` lists.
// ponytail: a path heuristic, not the Release file's Origin; a third-party `/debian … main` repo also gets contrib.
fn lacks_debian_contrib<'src>(
    mut uris: impl Iterator<Item = &'src str>,
    components: &[&str],
) -> bool {
    components.contains(&"main")
        && !components.contains(&"contrib")
        && uris.any(|uri| {
            uri.split('/').any(|segment| {
                matches!(
                    segment.trim_end_matches(".list"),
                    "debian" | "debian-security"
                )
            })
        })
}

/// One-line `deb [options] uri suite component…` entries.
fn enable_contrib_one_line(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            let (entry, comment) = line.split_once('#').unwrap_or((line, ""));
            let words: Vec<&str> = entry.split_whitespace().collect();
            let fields = match words.as_slice() {
                [kind, rest @ ..] if matches!(*kind, "deb" | "deb-src") => rest,
                _ => return line.to_owned(),
            };
            let fields = match fields.first() {
                Some(options) if options.starts_with('[') => fields
                    .iter()
                    .position(|word| word.ends_with(']'))
                    .and_then(|end| fields.get(end + 1..))
                    .unwrap_or_default(),
                _ => fields,
            };
            match fields {
                [uri, _suite, components @ ..]
                    if lacks_debian_contrib(std::iter::once(*uri), components) =>
                {
                    let comment = if line.contains('#') {
                        format!(" #{comment}")
                    } else {
                        String::new()
                    };
                    format!("{} contrib{comment}", entry.trim_end())
                }
                _ => line.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// deb822 paragraphs, each with its own `URIs:` and `Components:` fields.
fn enable_contrib_deb822(text: &str) -> String {
    text.split("\n\n")
        .map(|paragraph| {
            let field = |name: &str| {
                paragraph.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case(name).then_some(value)
                })
            };
            let (Some(uris), Some(components)) = (field("URIs"), field("Components")) else {
                return paragraph.to_owned();
            };
            let components: Vec<&str> = components.split_whitespace().collect();
            if !lacks_debian_contrib(uris.split_whitespace(), &components) {
                return paragraph.to_owned();
            }
            paragraph
                .split('\n')
                .map(|line| match line.split_once(':') {
                    Some((key, _)) if key.eq_ignore_ascii_case("Components") => {
                        format!("{} contrib", line.trim_end())
                    }
                    _ => line.to_owned(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn container_virtualization() -> String {
    Command::new("systemd-detect-virt")
        .arg("--container")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn lxc_is_unprivileged() -> Result<bool, Error> {
    let map = fs::read_to_string("/proc/self/uid_map").map_err(|source| Error::Io {
        stage: "inspect LXC user namespace",
        source,
    })?;
    let mut fields = map.split_whitespace();
    Ok(matches!(
        (fields.next(), fields.next()),
        (Some("0"), Some(outside)) if outside != "0"
    ))
}

fn uname(arg: &str, stage: &str) -> Result<String, Error> {
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

fn require_host_root_reserve(allocation: u64) -> Result<(), Error> {
    let (size, available) =
        crate::host_capacity::filesystem_space(Path::new("/")).map_err(|source| Error::Io {
            stage: "inspect host-root capacity for ZFS storage preparation",
            source,
        })?;
    let reserve = ployz_core::storage_host_reserve(size);
    if available < reserve.saturating_add(allocation) {
        return Err(Error::Command {
            stage: "prepare ZFS storage".into(),
            message: format!(
                "Host root has {available} bytes available; ZFS validation needs {allocation} bytes while preserving the {reserve}-byte host-root reserve"
            ),
        });
    }
    Ok(())
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
        .ok_or_else(|| Error::Command {
            stage: "prepare ZFS storage".into(),
            message: "Could not read total RAM for the ZFS ARC limit".into(),
        })?;
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
        download_error.unwrap_or_else(|| Error::Command {
            stage: "prepare ZFS storage".into(),
            message: format!(
                "Ubuntu has no packaged ZFS module for the running kernel {kernel}; install a supported Ubuntu kernel and retry"
            ),
        })
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
        Err(Error::Command {
            stage: "prepare ZFS storage".into(),
            message: format!(
                "Installed package {package} does not supply the ZFS module for running kernel {kernel}"
            ),
        })
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
    require_host_root_reserve(ZFS_SMOKE_BYTES)?;
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
                directory.display()
            ))
        });
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(Error::Verification(format!("{cleanup} (after: {error})")))
        }
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
