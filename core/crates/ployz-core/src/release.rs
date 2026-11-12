//! Release Channels and the exact Machine release versions they resolve to.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ValueError;

/// One exact supported Machine release version, `X.Y.Z` or `X.Y.Z-beta.N`, in release order.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub struct MachineVersion(semver::Version);

impl MachineVersion {
    /// Parse an exact supported release version.
    ///
    /// # Errors
    ///
    /// Returns an error for anything but `X.Y.Z` or `X.Y.Z-beta.N`.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let value = value.as_ref();
        semver::Version::parse(value)
            .ok()
            .filter(|version| {
                version.build.is_empty()
                    && (version.pre.is_empty()
                        || version.pre.strip_prefix("beta.").is_some_and(|number| {
                            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                        }))
            })
            .map(Self)
            .ok_or_else(|| ValueError::new("Machine version", value, "X.Y.Z or X.Y.Z-beta.N"))
    }

    /// The release line: a breaking release starts a new major.
    #[must_use]
    pub fn major(&self) -> u64 {
        self.0.major
    }

    /// Whether this is an `X.Y.Z-beta.N` prerelease.
    #[must_use]
    pub fn is_prerelease(&self) -> bool {
        !self.0.pre.is_empty()
    }
}

impl fmt::Display for MachineVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<String> for MachineVersion {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<MachineVersion> for String {
    fn from(value: MachineVersion) -> Self {
        value.to_string()
    }
}

/// A Release Channel a person can choose for their Cluster.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    Stable,
    Beta,
}

impl ReleaseChannel {
    /// The channel `release` names, or `None` for an exact version.
    #[must_use]
    pub const fn of(release: &MachineRelease) -> Option<Self> {
        match release {
            MachineRelease::Stable => Some(Self::Stable),
            MachineRelease::Beta => Some(Self::Beta),
            MachineRelease::Exact(_) => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

impl From<ReleaseChannel> for MachineRelease {
    fn from(channel: ReleaseChannel) -> Self {
        match channel {
            ReleaseChannel::Stable => Self::Stable,
            ReleaseChannel::Beta => Self::Beta,
        }
    }
}

impl fmt::Display for ReleaseChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A trusted release selector: a Release Channel resolved at install time, or one version.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub enum MachineRelease {
    /// The stable Release Channel of the installing daemon's line.
    Stable,
    /// The beta Release Channel of the installing daemon's line.
    Beta,
    /// This exact published version.
    Exact(MachineVersion),
}

impl MachineRelease {
    /// Parse `stable`, `beta`, or an exact supported version.
    ///
    /// # Errors
    ///
    /// Returns an error for any other channel name or version shape.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        match value.as_ref() {
            "stable" => Ok(Self::Stable),
            "beta" => Ok(Self::Beta),
            value => MachineVersion::parse(value).map(Self::Exact).map_err(|_| {
                ValueError::new(
                    "Machine release",
                    value,
                    "stable, beta, X.Y.Z, or X.Y.Z-beta.N",
                )
            }),
        }
    }
}

impl fmt::Display for MachineRelease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stable => formatter.write_str("stable"),
            Self::Beta => formatter.write_str("beta"),
            Self::Exact(version) => version.fmt(formatter),
        }
    }
}

impl FromStr for MachineRelease {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for MachineRelease {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<MachineRelease> for String {
    fn from(value: MachineRelease) -> Self {
        value.to_string()
    }
}

/// The release asset holding the minisign signature over `checksums.txt`.
pub const RELEASE_SIGNATURE_ASSET: &str = "checksums.txt.minisig";

/// The minisign public key that signs every published Ployz release.
pub const RELEASE_PUBLIC_KEY: &str = include_str!("../release.pub");

const RELEASE_ARCHIVES: &str = include_str!("../release-archives.txt");

/// One archive a release publishes: the product it carries, the platform it runs on and the
/// binaries inside. `arch` is the archive vocabulary (`amd64`, `arm64`), not Rust's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseArchive {
    pub product: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub name: &'static str,
    pub binaries: &'static str,
}

/// Every archive a release publishes, in manifest order.
pub fn release_archives() -> impl Iterator<Item = ReleaseArchive> {
    RELEASE_ARCHIVES
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let fields: Vec<&'static str> = line.split_whitespace().collect();
            let [product, os, arch, name, binaries] = fields[..] else {
                return None;
            };
            Some(ReleaseArchive {
                product,
                os,
                arch,
                name,
                binaries,
            })
        })
}

/// The CLI archive for an `os` (`linux`, `macos`) and `arch` (`amd64`, `arm64`).
#[must_use]
pub fn cli_archive(os: &str, arch: &str) -> Option<&'static str> {
    archive_for("ployz", os, arch)
}

/// The Linux daemon archive for a Rust `std::env::consts::ARCH`.
#[must_use]
pub fn daemon_archive(rust_arch: &str) -> Option<&'static str> {
    let arch = match rust_arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return None,
    };
    archive_for("ployzd", "linux", arch)
}

fn archive_for(product: &str, os: &str, arch: &str) -> Option<&'static str> {
    release_archives()
        .find(|archive| archive.product == product && archive.os == os && archive.arch == arch)
        .map(|archive| archive.name)
}

/// Why a release's `checksums.txt` is not trusted.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseSignatureError {
    #[error("the release public key is malformed")]
    PublicKey(#[source] minisign_verify::Error),
    #[error("{RELEASE_SIGNATURE_ASSET} is malformed")]
    Malformed(#[source] minisign_verify::Error),
    #[error("checksums.txt is not signed by the Ployz release key")]
    Invalid(#[source] minisign_verify::Error),
    #[error("{RELEASE_SIGNATURE_ASSET} signs {found:?}, not {expected:?}")]
    WrongRelease { expected: String, found: String },
}

/// The trusted comment a release signature carries, binding it to one version.
#[must_use]
pub fn release_signature_comment(version: &MachineVersion) -> String {
    format!("ployz v{version}")
}

/// Check that `signature` is the release key's signature over `checksums` for `version`.
///
/// # Errors
///
/// Returns an error unless the signature verifies under `public_key` and names `version`.
pub fn verify_release_checksums(
    public_key: &str,
    checksums: &[u8],
    signature: &str,
    version: &MachineVersion,
) -> Result<(), ReleaseSignatureError> {
    let public_key =
        minisign_verify::PublicKey::decode(public_key).map_err(ReleaseSignatureError::PublicKey)?;
    let signature =
        minisign_verify::Signature::decode(signature).map_err(ReleaseSignatureError::Malformed)?;
    public_key
        .verify(checksums, &signature, true)
        .map_err(ReleaseSignatureError::Invalid)?;
    let expected = release_signature_comment(version);
    if signature.trusted_comment() != expected {
        return Err(ReleaseSignatureError::WrongRelease {
            expected,
            found: signature.trusted_comment().to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_release_accepts_only_channels_and_supported_exact_versions() {
        for release in ["stable", "beta", "1.2.3", "1.2.3-beta.4"] {
            assert_eq!(MachineRelease::parse(release).unwrap().to_string(), release);
        }
        assert_eq!(
            MachineRelease::parse("1.2.3-beta.4").unwrap(),
            MachineRelease::Exact(MachineVersion::parse("1.2.3-beta.4").unwrap())
        );
        for release in [
            "latest",
            "nightly",
            "v1.2.3",
            "1.2",
            "1.2.3-alpha.1",
            "1.2.3+build",
            "1.2.3-beta.01",
            "https://example.test/release",
        ] {
            assert!(MachineRelease::parse(release).is_err(), "{release}");
            assert!(MachineVersion::parse(release).is_err(), "{release}");
        }
        assert!(MachineVersion::parse("stable").is_err());
    }

    // Installed daemons and the installer download these names, so they are pinned verbatim.
    #[test]
    fn release_archives_hold_todays_asset_names_and_binaries() {
        let rows: Vec<_> = release_archives()
            .map(|archive| {
                (
                    archive.product,
                    archive.os,
                    archive.arch,
                    archive.name,
                    archive.binaries,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "ployz",
                    "linux",
                    "amd64",
                    "ployz_linux_amd64.tar.gz",
                    "ployz"
                ),
                (
                    "ployz",
                    "linux",
                    "arm64",
                    "ployz_linux_arm64.tar.gz",
                    "ployz"
                ),
                (
                    "ployz",
                    "macos",
                    "amd64",
                    "ployz_macos_amd64.tar.gz",
                    "ployz"
                ),
                (
                    "ployz",
                    "macos",
                    "arm64",
                    "ployz_macos_arm64.tar.gz",
                    "ployz"
                ),
                (
                    "ployzd",
                    "linux",
                    "amd64",
                    "ployzd_linux_amd64.tar.gz",
                    "ployzd,ployz-uninstall"
                ),
                (
                    "ployzd",
                    "linux",
                    "arm64",
                    "ployzd_linux_arm64.tar.gz",
                    "ployzd,ployz-uninstall"
                ),
            ]
        );
    }

    #[test]
    fn every_manifest_line_is_a_comment_or_a_complete_row() {
        let rows = RELEASE_ARCHIVES
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
            .count();
        assert_eq!(rows, release_archives().count());
    }

    #[test]
    fn goreleaser_builds_exactly_the_manifest_platforms() {
        let goreleaser = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.goreleaser.yaml"
        ))
        .unwrap();
        let built: std::collections::BTreeSet<_> = goreleaser
            .lines()
            .filter_map(|line| line.trim().strip_prefix("- id: "))
            .filter(|id| id.contains("-linux-") || id.contains("-darwin-"))
            .collect();
        let listed: std::collections::BTreeSet<_> = release_archives()
            .map(|archive| {
                let os = if archive.os == "macos" {
                    "darwin"
                } else {
                    archive.os
                };
                format!("{}-{os}-{}", archive.product, archive.arch)
            })
            .collect();
        assert_eq!(
            built,
            listed.iter().map(String::as_str).collect(),
            ".goreleaser.yaml builds and release-archives.txt must name the same platforms"
        );
    }

    #[test]
    fn daemon_archive_maps_rust_architectures_to_todays_names() {
        assert_eq!(daemon_archive("x86_64"), Some("ployzd_linux_amd64.tar.gz"));
        assert_eq!(daemon_archive("aarch64"), Some("ployzd_linux_arm64.tar.gz"));
        assert_eq!(daemon_archive("riscv64"), None);
        assert_eq!(
            cli_archive("macos", "arm64"),
            Some("ployz_macos_arm64.tar.gz")
        );
        assert_eq!(cli_archive("darwin", "arm64"), None);
    }

    #[test]
    fn machine_versions_order_by_release_not_by_string() {
        let version = |value: &str| MachineVersion::parse(value).unwrap();
        assert!(version("1.2.10") > version("1.2.3"));
        assert!(version("1.2.3") > version("1.2.3-beta.9"));
        assert!(version("1.2.3-beta.10") > version("1.2.3-beta.2"));
        assert!(version("1.2.3-beta.2").is_prerelease());
        assert!(!version("1.2.3").is_prerelease());
    }

    // Made by the minisign CLI (`minisign -S -l -t 'ployz v1.2.3'`) with a throwaway key.
    const FIXTURE_PUBLIC_KEY: &str = "untrusted comment: minisign public key 4EF612BBAA6B01F2
RWTyAWuquxL2TvYSk50JZQ30mZ2/YYwz00ggW14QxQM94i5aDywcXNR2
";
    const FIXTURE_CHECKSUMS: &[u8] = b"abc123  ployzd_linux_amd64.tar.gz\n";
    const FIXTURE_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RWTyAWuquxL2ToNJ47RLhCHv8sy7TawgNcfgIWlgimjbT3VtnS0+yU22FRKtYpk1uV9kRwpu4Qgin/hzCNiSNzHInyIPjn/njAE=
trusted comment: ployz v1.2.3
Hba87bYoMElhZcSMvr9IyS97pXf3Yvxg/rSb6GG+EvrTyWrk3Hdln9ZDRRhuuFPYdcpUPVLABCKlZkMaBju9AA==
";

    fn verify_fixture(
        public_key: &str,
        checksums: &[u8],
        signature: &str,
        version: &str,
    ) -> Result<(), ReleaseSignatureError> {
        verify_release_checksums(
            public_key,
            checksums,
            signature,
            &MachineVersion::parse(version).unwrap(),
        )
    }

    #[test]
    fn release_signature_from_minisign_verifies() {
        verify_fixture(
            FIXTURE_PUBLIC_KEY,
            FIXTURE_CHECKSUMS,
            FIXTURE_SIGNATURE,
            "1.2.3",
        )
        .unwrap();
    }

    #[test]
    fn release_signature_refuses_tampered_checksums_signature_or_version() {
        let mut checksums = FIXTURE_CHECKSUMS.to_vec();
        *checksums.first_mut().unwrap() ^= 1;
        assert!(matches!(
            verify_fixture(FIXTURE_PUBLIC_KEY, &checksums, FIXTURE_SIGNATURE, "1.2.3"),
            Err(ReleaseSignatureError::Invalid(_))
        ));
        let signature = FIXTURE_SIGNATURE.replace("ployz v1.2.3", "ployz v1.2.4");
        assert!(matches!(
            verify_fixture(FIXTURE_PUBLIC_KEY, FIXTURE_CHECKSUMS, &signature, "1.2.4"),
            Err(ReleaseSignatureError::Invalid(_))
        ));
        assert!(matches!(
            verify_fixture(
                FIXTURE_PUBLIC_KEY,
                FIXTURE_CHECKSUMS,
                FIXTURE_SIGNATURE,
                "1.2.4"
            ),
            Err(ReleaseSignatureError::WrongRelease { .. })
        ));
        assert!(matches!(
            verify_fixture(
                RELEASE_PUBLIC_KEY,
                FIXTURE_CHECKSUMS,
                FIXTURE_SIGNATURE,
                "1.2.3"
            ),
            Err(ReleaseSignatureError::Invalid(_))
        ));
        assert!(matches!(
            verify_fixture(
                FIXTURE_PUBLIC_KEY,
                FIXTURE_CHECKSUMS,
                "not a signature",
                "1.2.3"
            ),
            Err(ReleaseSignatureError::Malformed(_))
        ));
    }
}
