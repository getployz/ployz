use ployz_core::probe_document::{Blocker, BlockerCode, OsRelease, Virtualization};

use super::packages::PackageManager;

/// What ended the choice of a ZFS route: a reason the Machine has none, or a read that failed.
pub(super) enum RouteStop<E> {
    Blocked(Blocker),
    Unread(E),
}

/// The ZFS route for a Machine, or the first thing that stops it, in the installer's order: OS,
/// Secure Boot, then virtualization. The assessment answers from the probe document and the
/// installer from the host, so each passes its own reads. They are lazy: a Machine refused by
/// its OS, or by Secure Boot, never has its virtualization read.
pub(super) fn choose<E>(
    os: &OsRelease,
    secure_boot: impl FnOnce() -> Result<Option<bool>, E>,
    virtualization: impl FnOnce() -> Result<Virtualization, E>,
) -> Result<ZfsRoute, RouteStop<E>> {
    let route = ZfsRoute::for_os(os).ok_or_else(|| RouteStop::Blocked(unsupported_os(os)))?;
    if route.builds_module() {
        let secure_boot = secure_boot().map_err(RouteStop::Unread)?;
        if let Some(reason) = secure_boot_blocker(os, secure_boot) {
            return Err(RouteStop::Blocked(reason));
        }
    }
    let virt = virtualization().map_err(RouteStop::Unread)?;
    match virtualization_blocker(&virt) {
        Some(reason) => Err(RouteStop::Blocked(reason)),
        None => Ok(route),
    }
}

fn unsupported_os(os: &OsRelease) -> Blocker {
    Blocker::new(
        BlockerCode::UnsupportedOs,
        format!(
            "Managed volumes need Ubuntu LTS, Debian 12–13 or Amazon Linux 2023; this Server runs {}. Use one of those, or add `--storage none`.",
            os.display()
        ),
    )
}

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
    /// The pinned `OPENZFS_VERSION` built on the Machine into DKMS and userspace RPMs.
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

    pub(super) fn package_manager(self) -> PackageManager {
        match self {
            Self::Ubuntu | Self::Debian { .. } => PackageManager::Apt,
            Self::AmazonLinux => PackageManager::Dnf,
        }
    }
}

/// Secure Boot stops the routes that build their own, unsigned module.
fn secure_boot_blocker(os: &OsRelease, secure_boot: Option<bool>) -> Option<Blocker> {
    match secure_boot {
        Some(false) => None,
        Some(true) => Some(Blocker::new(
            BlockerCode::SecureBoot,
            format!(
                "Secure Boot is on, so this Server can't load the ZFS module Ployz builds for {}. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
                os.name
            ),
        )),
        None => Some(Blocker::new(
            BlockerCode::SecureBootUnknown,
            format!(
                "Couldn't tell whether Secure Boot is on, and under it this Server can't load the ZFS module Ployz builds for {}. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
                os.name
            ),
        )),
    }
}

fn virtualization_blocker(virt: &Virtualization) -> Option<Blocker> {
    match virt {
        Virtualization::OpenVz => Some(Blocker::new(
            BlockerCode::OpenVz,
            "OpenVZ does not allow this Machine to load the host ZFS kernel module",
        )),
        Virtualization::Lxc {
            privileged: Some(false),
        } => Some(Blocker::new(
            BlockerCode::UnprivilegedLxc,
            "Unprivileged LXC does not allow this Machine to load the host ZFS kernel module",
        )),
        Virtualization::Lxc { privileged: None } => Some(Blocker::new(
            BlockerCode::LxcPrivilegeUnknown,
            "Couldn't read this LXC container's user namespace, so Ployz can't tell whether it may load the host ZFS kernel module",
        )),
        Virtualization::BareMetal
        | Virtualization::Vm { .. }
        | Virtualization::Lxc {
            privileged: Some(true),
        }
        | Virtualization::Container { .. }
        | Virtualization::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(id: &str, name: &str, version_id: Option<&str>) -> OsRelease {
        OsRelease {
            id: id.to_owned(),
            name: name.to_owned(),
            version_id: version_id.map(str::to_owned),
        }
    }

    #[test]
    fn each_supported_distro_has_its_route() {
        let ubuntu = ZfsRoute::for_os(&os("ubuntu", "Ubuntu", Some("24.04"))).unwrap();
        assert!(matches!(ubuntu, ZfsRoute::Ubuntu));
        assert!(!ubuntu.builds_module());
        assert_eq!(
            (ubuntu.prepare_minutes(), ubuntu.package_manager().command()),
            (1, "apt-get")
        );
        let bookworm = ZfsRoute::for_os(&os("debian", "Debian", Some("12"))).unwrap();
        assert!(matches!(
            bookworm,
            ZfsRoute::Debian {
                codename: "bookworm",
                keyring: "debian-archive-keyring.gpg"
            }
        ));
        let trixie = ZfsRoute::for_os(&os("debian", "Debian", Some("13"))).unwrap();
        assert!(matches!(
            trixie,
            ZfsRoute::Debian {
                codename: "trixie",
                keyring: "debian-archive-keyring.pgp"
            }
        ));
        assert!(trixie.builds_module());
        assert_eq!(
            (trixie.prepare_minutes(), trixie.package_manager().command()),
            (10, "apt-get")
        );
        let amazon = ZfsRoute::for_os(&os("amzn", "Amazon Linux", Some("2023"))).unwrap();
        assert!(matches!(amazon, ZfsRoute::AmazonLinux));
        assert_eq!(
            (amazon.prepare_minutes(), amazon.package_manager().command()),
            (20, "dnf")
        );
        assert!(ZfsRoute::for_os(&os("debian", "Debian", Some("11"))).is_none());
    }

    #[test]
    fn a_stop_never_reads_what_comes_after_it() {
        let unread = || -> Result<Virtualization, &'static str> { Err("virtualization read") };
        let debian = os("debian", "Debian", Some("13"));
        let secure_boot_on = || Ok(Some(true));
        // Secure Boot stops a building route before virtualization is read.
        assert!(matches!(
            choose(&debian, secure_boot_on, unread),
            Err(RouteStop::Blocked(blocker)) if blocker.code == BlockerCode::SecureBoot
        ));
        // An unsupported OS reads nothing.
        let arch = os("arch", "Arch Linux", None);
        let nothing = || -> Result<Option<bool>, &'static str> { Err("secure boot read") };
        assert!(matches!(
            choose(&arch, nothing, unread),
            Err(RouteStop::Blocked(blocker)) if blocker.code == BlockerCode::UnsupportedOs
        ));
        // Ubuntu's prebuilt module is not stopped by Secure Boot, so it is never read.
        let ubuntu = os("ubuntu", "Ubuntu", Some("24.04"));
        assert!(matches!(
            choose(&ubuntu, nothing, || Ok(Virtualization::BareMetal)),
            Ok(ZfsRoute::Ubuntu)
        ));
        // A read that fails ends the choice with its error.
        assert!(matches!(
            choose(&ubuntu, nothing, unread),
            Err(RouteStop::Unread("virtualization read"))
        ));
    }
}
