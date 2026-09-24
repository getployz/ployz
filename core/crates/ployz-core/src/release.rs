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

    #[test]
    fn machine_versions_order_by_release_not_by_string() {
        let version = |value: &str| MachineVersion::parse(value).unwrap();
        assert!(version("1.2.10") > version("1.2.3"));
        assert!(version("1.2.3") > version("1.2.3-beta.9"));
        assert!(version("1.2.3-beta.10") > version("1.2.3-beta.2"));
        assert!(version("1.2.3-beta.2").is_prerelease());
        assert!(!version("1.2.3").is_prerelease());
    }
}
