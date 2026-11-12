use ployz_core::probe_document::OsRelease;

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

    pub(super) fn package_manager(self) -> &'static str {
        match self {
            Self::Ubuntu | Self::Debian { .. } => "apt-get",
            Self::AmazonLinux => "dnf",
        }
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
            (ubuntu.prepare_minutes(), ubuntu.package_manager()),
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
            (trixie.prepare_minutes(), trixie.package_manager()),
            (10, "apt-get")
        );
        let amazon = ZfsRoute::for_os(&os("amzn", "Amazon Linux", Some("2023"))).unwrap();
        assert!(matches!(amazon, ZfsRoute::AmazonLinux));
        assert_eq!(
            (amazon.prepare_minutes(), amazon.package_manager()),
            (20, "dnf")
        );
        assert!(ZfsRoute::for_os(&os("debian", "Debian", Some("11"))).is_none());
    }
}
