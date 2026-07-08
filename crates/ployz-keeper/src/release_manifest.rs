//! Versioned release manifest parsing for keeper-owned installs.

use std::fmt;

use ployz_core::install::{
    AbsoluteInstallPath, FirstMachineInstallArtifacts, InstallArtifactSource, InstallArtifactSpec,
    InstallArtifactVersion, InstallSha256Digest, NatsServerInstallSpec,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactPloyzVersion(String);

impl ExactPloyzVersion {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ExactPloyzVersionError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ExactPloyzVersionError::Empty);
        }
        if value == "latest" || value == "alpha" || value == "beta" || value == "stable" {
            return Err(ExactPloyzVersionError::Mutable { value });
        }
        if value.contains('*')
            || value.contains('^')
            || value.contains('~')
            || value.contains('<')
            || value.contains('>')
            || value.contains('=')
            || value.contains(',')
        {
            return Err(ExactPloyzVersionError::Range { value });
        }
        if value
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || ".-_".contains(character)))
        {
            return Err(ExactPloyzVersionError::Invalid { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn tag(&self) -> String {
        if self.0.starts_with('v') {
            self.0.clone()
        } else {
            format!("v{}", self.0)
        }
    }
}

impl std::str::FromStr for ExactPloyzVersion {
    type Err = ExactPloyzVersionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExactPloyzVersionError {
    Empty,
    Mutable { value: String },
    Range { value: String },
    Invalid { value: String },
}

impl fmt::Display for ExactPloyzVersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("update version is empty"),
            Self::Mutable { value } => {
                write!(
                    formatter,
                    "update version must be exact, got mutable {value:?}"
                )
            }
            Self::Range { value } => {
                write!(
                    formatter,
                    "update version must be exact, got range {value:?}"
                )
            }
            Self::Invalid { value } => write!(formatter, "update version is invalid: {value:?}"),
        }
    }
}

impl std::error::Error for ExactPloyzVersionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseManifest {
    version: String,
    ployzd_url: String,
    ployzd_sha256: String,
    ebpf_tc_url: String,
    ebpf_tc_sha256: String,
    ebpf_ctl_url: String,
    ebpf_ctl_sha256: String,
    /// Absent when the manifest ships no `nats-server` (a dev substrate
    /// push); installs that found or promote a core reject such a manifest.
    nats_server: Option<NatsServerManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NatsServerManifestEntry {
    version: String,
    url: String,
    sha256: String,
}

impl ReleaseManifest {
    pub fn parse(contents: &str) -> Result<Self, String> {
        Ok(Self {
            version: manifest_value(contents, "PLOYZ_VERSION")?,
            ployzd_url: manifest_value(contents, "PLOYZD_URL")?,
            ployzd_sha256: manifest_value(contents, "PLOYZD_SHA256")?,
            ebpf_tc_url: manifest_value(contents, "PLOYZ_EBPF_TC_URL")?,
            ebpf_tc_sha256: manifest_value(contents, "PLOYZ_EBPF_TC_SHA256")?,
            ebpf_ctl_url: manifest_value(contents, "PLOYZ_EBPF_CTL_URL")?,
            ebpf_ctl_sha256: manifest_value(contents, "PLOYZ_EBPF_CTL_SHA256")?,
            nats_server: nats_server_entry(contents)?,
        })
    }

    pub fn install_artifacts(&self) -> Result<FirstMachineInstallArtifacts, String> {
        Ok(FirstMachineInstallArtifacts {
            ployzd: artifact_spec(
                &self.version,
                &self.ployzd_url,
                &self.ployzd_sha256,
                "/usr/local/bin/ployzd",
            )?,
            ebpf_bytecode: artifact_spec(
                &self.version,
                &self.ebpf_tc_url,
                &self.ebpf_tc_sha256,
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            )?,
            ebpf_ctl: artifact_spec(
                &self.version,
                &self.ebpf_ctl_url,
                &self.ebpf_ctl_sha256,
                "/usr/local/bin/ployz-ebpf-ctl",
            )?,
            nats_server: self
                .nats_server
                .as_ref()
                .map(|entry| {
                    Ok::<_, String>(NatsServerInstallSpec {
                        version: InstallArtifactVersion::try_new(&entry.version)
                            .map_err(|error| error.to_string())?,
                        source: InstallArtifactSource::try_new(&entry.url)
                            .map_err(|error| error.to_string())?,
                        sha256: InstallSha256Digest::try_new(&entry.sha256)
                            .map_err(|error| error.to_string())?,
                        binary: AbsoluteInstallPath::try_new("/usr/local/bin/nats-server")
                            .map_err(|error| error.to_string())?,
                        config: AbsoluteInstallPath::try_new("/etc/nats/nats-server.conf")
                            .map_err(|error| error.to_string())?,
                    })
                })
                .transpose()?,
        })
    }
}

/// A manifest either carries all three `PLOYZ_NATS_SERVER_*` values or none;
/// a partial entry is a broken manifest, not an omitted artifact.
fn nats_server_entry(contents: &str) -> Result<Option<NatsServerManifestEntry>, String> {
    let version = manifest_value(contents, "PLOYZ_NATS_SERVER_VERSION").ok();
    let url = manifest_value(contents, "PLOYZ_NATS_SERVER_URL").ok();
    let sha256 = manifest_value(contents, "PLOYZ_NATS_SERVER_SHA256").ok();
    match (version, url, sha256) {
        (None, None, None) => Ok(None),
        (Some(version), Some(url), Some(sha256)) => Ok(Some(NatsServerManifestEntry {
            version,
            url,
            sha256,
        })),
        _ => Err(
            "release manifest has a partial nats-server entry: PLOYZ_NATS_SERVER_VERSION, \
             PLOYZ_NATS_SERVER_URL, and PLOYZ_NATS_SERVER_SHA256 must all be set or all be absent"
                .to_owned(),
        ),
    }
}

#[must_use]
pub fn release_manifest_url(version: &ExactPloyzVersion) -> String {
    format!(
        "https://github.com/getployz/ployz/releases/download/{}/ployz-release-{}.env",
        version.tag(),
        release_platform()
    )
}

pub fn persisted_release_manifest_url(path: &std::path::Path) -> Result<String, String> {
    let contents = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    manifest_value(&contents, "PLOYZ_RELEASE_MANIFEST_URL")
}

#[must_use]
pub fn default_release_manifest_url() -> String {
    format!(
        "https://github.com/getployz/ployz/releases/download/v{}/ployz-release-{}.env",
        env!("CARGO_PKG_VERSION"),
        release_platform()
    )
}

fn artifact_spec(
    version: &str,
    source: &str,
    sha256: &str,
    install_path: &str,
) -> Result<InstallArtifactSpec, String> {
    Ok(InstallArtifactSpec {
        version: InstallArtifactVersion::try_new(version).map_err(|error| error.to_string())?,
        source: InstallArtifactSource::try_new(source).map_err(|error| error.to_string())?,
        sha256: InstallSha256Digest::try_new(sha256).map_err(|error| error.to_string())?,
        install_path: AbsoluteInstallPath::try_new(install_path)
            .map_err(|error| error.to_string())?,
    })
}

fn release_platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-amd64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "arm") => "linux-arm64",
        _ => "unsupported",
    }
}

fn manifest_value(contents: &str, key: &str) -> Result<String, String> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("release manifest is missing {key}"))
}

#[cfg(test)]
mod tests {
    use super::{
        ExactPloyzVersion, ReleaseManifest, persisted_release_manifest_url, release_manifest_url,
    };

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn exact_version_builds_github_release_manifest_url() {
        let version = ExactPloyzVersion::try_new("v0.0.2-alpha.16").expect("version is exact");

        assert!(release_manifest_url(&version).contains(
            "https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.16/ployz-release-"
        ));
    }

    #[test]
    fn mutable_versions_are_rejected() {
        assert!(ExactPloyzVersion::try_new("latest").is_err());
        assert!(ExactPloyzVersion::try_new("alpha").is_err());
        assert!(ExactPloyzVersion::try_new("^0.0.2").is_err());
    }

    #[test]
    fn release_manifest_builds_keeper_artifacts() {
        let manifest = ReleaseManifest::parse(&format!(
            "PLOYZ_VERSION=0.1.0\n\
             PLOYZD_URL=https://example.test/ployzd\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=https://example.test/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=https://example.test/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n\
             PLOYZ_NATS_SERVER_VERSION=2.14.2\n\
             PLOYZ_NATS_SERVER_URL=https://example.test/nats-server\n\
             PLOYZ_NATS_SERVER_SHA256={SHA}\n"
        ))
        .expect("manifest parses");
        let artifacts = manifest.install_artifacts().expect("artifacts build");

        assert_eq!(
            artifacts.ployzd.install_path.as_str(),
            "/usr/local/bin/ployzd"
        );
        assert_eq!(
            artifacts
                .nats_server
                .expect("manifest carries nats-server")
                .binary
                .as_str(),
            "/usr/local/bin/nats-server"
        );
    }

    #[test]
    fn release_manifest_without_nats_server_omits_the_artifact() {
        let manifest = ReleaseManifest::parse(&format!(
            "PLOYZ_VERSION=0.1.0\n\
             PLOYZD_URL=https://example.test/ployzd\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=https://example.test/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=https://example.test/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n"
        ))
        .expect("manifest parses");
        let artifacts = manifest.install_artifacts().expect("artifacts build");

        assert!(artifacts.nats_server.is_none());
    }

    #[test]
    fn release_manifest_with_partial_nats_server_entry_is_rejected() {
        let error = ReleaseManifest::parse(&format!(
            "PLOYZ_VERSION=0.1.0\n\
             PLOYZD_URL=https://example.test/ployzd\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=https://example.test/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=https://example.test/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n\
             PLOYZ_NATS_SERVER_VERSION=2.14.2\n"
        ))
        .expect_err("partial entry is rejected");

        assert!(error.contains("partial nats-server entry"));
    }

    #[test]
    fn persisted_release_env_supplies_manifest_url() {
        let path = std::env::temp_dir().join(format!(
            "ployz-release-env-{}-{}.env",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after unix epoch")
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "PLOYZ_RELEASE_MANIFEST_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.7/ployz-release-linux-amd64.env\n",
        )
        .expect("release env can be written");

        assert_eq!(
            persisted_release_manifest_url(&path).expect("manifest URL loads"),
            "https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.7/ployz-release-linux-amd64.env"
        );
        let _ = std::fs::remove_file(path);
    }
}
