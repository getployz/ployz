//! Versioned release manifest parsing for Host Runner-owned installs.

use ployz_core::build::railpack_pins;
use ployz_core::install::{
    AbsoluteInstallPath, ExactPloyzVersion, FirstMachineInstallArtifacts, InstallArtifactSource,
    InstallArtifactSpec, InstallArtifactVersion, InstallSha256Digest, NatsServerInstallSpec,
    ReleasePlatformFailure,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleasePlatform {
    LinuxAmd64,
    LinuxArm64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReleaseManifestError {
    #[error("release manifest platform is invalid: {failure:?}")]
    Platform { failure: ReleasePlatformFailure },
    #[error("{message}")]
    Invalid { message: String },
}

impl From<String> for ReleaseManifestError {
    fn from(message: String) -> Self {
        Self::Invalid { message }
    }
}

impl ReleasePlatform {
    pub fn from_target(os: &str, arch: &str) -> Result<Self, String> {
        match (os, arch) {
            ("linux", "x86_64") => Ok(Self::LinuxAmd64),
            ("linux", "aarch64") => Ok(Self::LinuxArm64),
            _ => Err(format!("unsupported release platform {os}/{arch}")),
        }
    }

    #[must_use]
    pub const fn manifest_slug(self) -> &'static str {
        match self {
            Self::LinuxAmd64 => "linux-amd64",
            Self::LinuxArm64 => "linux-arm64",
        }
    }

    fn from_manifest_slug(value: &str) -> Result<Self, ReleaseManifestError> {
        match value {
            "linux-amd64" => Ok(Self::LinuxAmd64),
            "linux-arm64" => Ok(Self::LinuxArm64),
            _ => Err(ReleaseManifestError::Platform {
                failure: ReleasePlatformFailure::Unsupported {
                    platform: value.to_owned(),
                },
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseManifest {
    platform: ReleasePlatform,
    version: ExactPloyzVersion,
    ployzd_url: String,
    ployzd_sha256: String,
    ebpf_tc_url: String,
    ebpf_tc_sha256: String,
    ebpf_ctl_url: String,
    ebpf_ctl_sha256: String,
    railpack: RailpackManifestEntry,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct RailpackManifestEntry {
    version: String,
    url: String,
    sha256: String,
    install_path: String,
}

impl ReleaseManifest {
    pub fn parse(contents: &str) -> Result<Self, ReleaseManifestError> {
        let platform = manifest_value(contents, "PLOYZ_RELEASE_PLATFORM").map_err(|_| {
            ReleaseManifestError::Platform {
                failure: ReleasePlatformFailure::Missing,
            }
        })?;
        Ok(Self {
            platform: ReleasePlatform::from_manifest_slug(&platform)?,
            version: ExactPloyzVersion::try_new(manifest_value(contents, "PLOYZ_VERSION")?)
                .map_err(|error| {
                    invalid_manifest(format!(
                        "release manifest PLOYZ_VERSION is invalid: {error}"
                    ))
                })?,
            ployzd_url: manifest_value(contents, "PLOYZD_URL")?,
            ployzd_sha256: manifest_value(contents, "PLOYZD_SHA256")?,
            ebpf_tc_url: manifest_value(contents, "PLOYZ_EBPF_TC_URL")?,
            ebpf_tc_sha256: manifest_value(contents, "PLOYZ_EBPF_TC_SHA256")?,
            ebpf_ctl_url: manifest_value(contents, "PLOYZ_EBPF_CTL_URL")?,
            ebpf_ctl_sha256: manifest_value(contents, "PLOYZ_EBPF_CTL_SHA256")?,
            railpack: railpack_entry(contents).map_err(invalid_manifest)?,
            nats_server: nats_server_entry(contents).map_err(invalid_manifest)?,
        })
    }

    #[must_use]
    pub const fn platform(&self) -> ReleasePlatform {
        self.platform
    }

    #[must_use]
    pub fn ployz_version(&self) -> &str {
        self.version.as_str()
    }

    #[must_use]
    pub const fn version(&self) -> &ExactPloyzVersion {
        &self.version
    }

    pub fn install_artifacts_for(
        &self,
        local_platform: ReleasePlatform,
    ) -> Result<FirstMachineInstallArtifacts, String> {
        self.require_platform(local_platform)?;
        self.install_artifacts()
    }

    pub fn nats_server_install_spec_for(
        &self,
        local_platform: ReleasePlatform,
    ) -> Result<Option<NatsServerInstallSpec>, String> {
        self.require_platform(local_platform)?;
        self.nats_server_install_spec()
    }

    pub fn install_artifacts(&self) -> Result<FirstMachineInstallArtifacts, String> {
        Ok(FirstMachineInstallArtifacts {
            ployzd: artifact_spec(
                self.ployz_version(),
                &self.ployzd_url,
                &self.ployzd_sha256,
                "/usr/local/bin/ployzd",
            )?,
            ebpf_bytecode: artifact_spec(
                self.ployz_version(),
                &self.ebpf_tc_url,
                &self.ebpf_tc_sha256,
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            )?,
            ebpf_ctl: artifact_spec(
                self.ployz_version(),
                &self.ebpf_ctl_url,
                &self.ebpf_ctl_sha256,
                "/usr/local/bin/ployz-ebpf-ctl",
            )?,
            railpack: artifact_spec(
                &self.railpack.version,
                &self.railpack.url,
                &self.railpack.sha256,
                &self.railpack.install_path,
            )?,
            nats_server: self.nats_server_install_spec()?,
        })
    }

    fn require_platform(&self, local_platform: ReleasePlatform) -> Result<(), String> {
        if self.platform == local_platform {
            return Ok(());
        }
        Err(format!(
            "release manifest platform {} does not match host platform {}",
            self.platform.manifest_slug(),
            local_platform.manifest_slug()
        ))
    }

    fn nats_server_install_spec(&self) -> Result<Option<NatsServerInstallSpec>, String> {
        self.nats_server
            .as_ref()
            .map(|entry| {
                Ok(NatsServerInstallSpec {
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
            .transpose()
    }
}

fn railpack_entry(contents: &str) -> Result<RailpackManifestEntry, String> {
    let pins = railpack_pins().map_err(|error| error.to_string())?;
    let version = manifest_value(contents, "PLOYZ_RAILPACK_VERSION")?;
    if version != pins.version() {
        return Err(format!(
            "release manifest has unsupported PLOYZ_RAILPACK_VERSION={version}; expected {}",
            pins.version()
        ));
    }
    Ok(RailpackManifestEntry {
        version,
        url: manifest_value(contents, "PLOYZ_RAILPACK_URL")?,
        sha256: manifest_value(contents, "PLOYZ_RAILPACK_SHA256")?,
        install_path: pins.install_path().to_owned(),
    })
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
    let platform = ReleasePlatform::from_target(std::env::consts::OS, std::env::consts::ARCH)
        .map_or("unsupported", ReleasePlatform::manifest_slug);
    release_manifest_url_for_platform(version, platform)
}

#[must_use]
pub fn release_manifest_url_for_platform(
    version: &ExactPloyzVersion,
    platform: impl AsRef<str>,
) -> String {
    format!(
        "https://github.com/getployz/ployz/releases/download/{}/ployz-release-{}.env",
        version.tag(),
        platform.as_ref()
    )
}

fn invalid_manifest(message: impl Into<String>) -> ReleaseManifestError {
    ReleaseManifestError::Invalid {
        message: message.into(),
    }
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
    ReleasePlatform::from_target(std::env::consts::OS, std::env::consts::ARCH)
        .map_or("unsupported", ReleasePlatform::manifest_slug)
}

fn manifest_value(contents: &str, key: &str) -> Result<String, String> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("release manifest is missing {key}"))
}

/// Reads the manifest text behind a release manifest URL. Published release
/// manifests live behind https and go through the https-only cloud client;
/// a `file://` URL names manifest material already present on the machine
/// (local installs, e2e formations) and is read directly from disk.
pub(crate) fn read_release_manifest_text(url: &str) -> Result<String, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read release manifest {url}: {error}"));
    }
    crate::cloud_client::get_text_url(url)
        .map_err(|error| format!("failed to download release manifest {url}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        ExactPloyzVersion, ReleaseManifest, ReleaseManifestError, ReleasePlatform,
        ReleasePlatformFailure, persisted_release_manifest_url, read_release_manifest_text,
        release_manifest_url,
    };

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn manifest_without_nats(platform: Option<&str>) -> String {
        let platform = platform
            .map(|platform| format!("PLOYZ_RELEASE_PLATFORM={platform}\n"))
            .unwrap_or_default();
        format!(
            "{platform}PLOYZ_VERSION=0.1.0\n\
             PLOYZD_URL=https://example.test/ployzd\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=https://example.test/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=https://example.test/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n\
             PLOYZ_RAILPACK_VERSION=v0.31.0\n\
             PLOYZ_RAILPACK_URL=https://example.test/railpack\n\
             PLOYZ_RAILPACK_SHA256={SHA}\n"
        )
    }

    #[test]
    fn release_platform_derives_linux_amd64_from_rust_target_names() {
        assert_eq!(
            ReleasePlatform::from_target("linux", "x86_64"),
            Ok(ReleasePlatform::LinuxAmd64)
        );
    }

    #[test]
    fn release_platform_derives_linux_arm64_from_rust_target_names() {
        assert_eq!(
            ReleasePlatform::from_target("linux", "aarch64"),
            Ok(ReleasePlatform::LinuxArm64)
        );
    }

    #[test]
    fn release_platform_rejects_unknown_architecture() {
        assert_eq!(
            ReleasePlatform::from_target("linux", "riscv64").expect_err("arch is unsupported"),
            "unsupported release platform linux/riscv64"
        );
    }

    #[test]
    fn release_platform_rejects_unknown_operating_system() {
        assert_eq!(
            ReleasePlatform::from_target("windows", "x86_64").expect_err("os is unsupported"),
            "unsupported release platform windows/x86_64"
        );
    }

    #[test]
    fn release_platform_uses_canonical_manifest_slug() {
        assert_eq!(ReleasePlatform::LinuxAmd64.manifest_slug(), "linux-amd64");
    }

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
    fn release_manifest_builds_host_runner_artifacts() {
        let manifest = ReleaseManifest::parse(&format!(
            "PLOYZ_RELEASE_PLATFORM=linux-amd64\n\
             PLOYZ_VERSION=0.1.0\n\
             PLOYZD_URL=https://example.test/ployzd\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=https://example.test/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=https://example.test/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n\
             PLOYZ_RAILPACK_VERSION=v0.31.0\n\
             PLOYZ_RAILPACK_URL=https://example.test/railpack\n\
             PLOYZ_RAILPACK_SHA256={SHA}\n\
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
        assert_eq!(artifacts.railpack.version.as_str(), "v0.31.0");
        assert_eq!(
            artifacts.railpack.install_path.as_str(),
            "/usr/local/lib/ployz/railpack/v0.31.0/railpack"
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
    fn release_manifest_rejects_artifacts_for_another_local_platform() {
        let manifest = ReleaseManifest::parse(&manifest_without_nats(Some("linux-amd64")))
            .expect("manifest parses");

        let error = manifest
            .install_artifacts_for(ReleasePlatform::LinuxArm64)
            .expect_err("another platform must fail before install planning");

        assert_eq!(
            error,
            "release manifest platform linux-amd64 does not match host platform linux-arm64"
        );
    }

    #[test]
    fn release_manifest_requires_release_platform() {
        let error = ReleaseManifest::parse(&manifest_without_nats(None))
            .expect_err("platform is required release identity");

        assert_eq!(
            error,
            ReleaseManifestError::Platform {
                failure: ReleasePlatformFailure::Missing,
            }
        );
    }

    #[test]
    fn release_manifest_rejects_unknown_release_platform() {
        let error = ReleaseManifest::parse(&manifest_without_nats(Some("linux-riscv64")))
            .expect_err("platform is unsupported");

        assert_eq!(
            error,
            ReleaseManifestError::Platform {
                failure: ReleasePlatformFailure::Unsupported {
                    platform: "linux-riscv64".to_owned(),
                },
            }
        );
    }

    #[test]
    fn release_manifest_exposes_typed_platform() {
        let manifest = ReleaseManifest::parse(&manifest_without_nats(Some("linux-arm64")))
            .expect("manifest parses");

        assert_eq!(manifest.platform(), ReleasePlatform::LinuxArm64);
    }

    #[test]
    fn release_manifest_exposes_ployz_release_version() {
        let manifest = ReleaseManifest::parse(&manifest_without_nats(Some("linux-amd64")))
            .expect("manifest parses");

        assert_eq!(manifest.ployz_version(), "0.1.0");
    }

    #[test]
    fn release_manifest_rejects_mutable_ployz_release_version() {
        let manifest = manifest_without_nats(Some("linux-amd64"))
            .replace("PLOYZ_VERSION=0.1.0", "PLOYZ_VERSION=alpha");

        let error = ReleaseManifest::parse(&manifest).expect_err("release must be immutable");

        assert_eq!(
            error.to_string(),
            "release manifest PLOYZ_VERSION is invalid: release version must be exact, got mutable \"alpha\""
        );
    }

    #[test]
    fn release_manifest_requires_the_complete_railpack_tuple() {
        let missing = ReleaseManifest::parse(&format!(
            "PLOYZ_RELEASE_PLATFORM=linux-amd64\n\
             PLOYZ_VERSION=0.1.0\n\
             PLOYZD_URL=https://example.test/ployzd\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=https://example.test/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=https://example.test/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n"
        ))
        .expect_err("Railpack is required release material");
        assert!(missing.to_string().contains("PLOYZ_RAILPACK_VERSION"));

        let partial = ReleaseManifest::parse(&format!(
            "PLOYZ_RELEASE_PLATFORM=linux-amd64\n\
             PLOYZ_VERSION=0.1.0\n\
             PLOYZD_URL=https://example.test/ployzd\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=https://example.test/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=https://example.test/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n\
             PLOYZ_RAILPACK_VERSION=v0.31.0\n"
        ))
        .expect_err("partial Railpack tuple is rejected");
        assert!(partial.to_string().contains("PLOYZ_RAILPACK_URL"));
    }

    #[test]
    fn release_manifest_without_nats_server_omits_the_artifact() {
        let manifest = ReleaseManifest::parse(&manifest_without_nats(Some("linux-amd64")))
            .expect("manifest parses");
        let artifacts = manifest.install_artifacts().expect("artifacts build");

        assert!(artifacts.nats_server.is_none());
    }

    #[test]
    fn release_manifest_with_partial_nats_server_entry_is_rejected() {
        let error = ReleaseManifest::parse(&format!(
            "PLOYZ_RELEASE_PLATFORM=linux-amd64\n\
             PLOYZ_VERSION=0.1.0\n\
             PLOYZD_URL=https://example.test/ployzd\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=https://example.test/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=https://example.test/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n\
             PLOYZ_RAILPACK_VERSION=v0.31.0\n\
             PLOYZ_RAILPACK_URL=https://example.test/railpack\n\
             PLOYZ_RAILPACK_SHA256={SHA}\n\
             PLOYZ_NATS_SERVER_VERSION=2.14.2\n"
        ))
        .expect_err("partial entry is rejected");

        assert!(error.to_string().contains("partial nats-server entry"));
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

    #[test]
    fn file_url_manifest_is_read_from_disk() {
        let path = std::env::temp_dir().join(format!(
            "ployz-release-{}.env",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after unix epoch")
                .as_nanos()
        ));
        std::fs::write(&path, "PLOYZ_VERSION=local\n").expect("manifest can be written");

        let url = format!("file://{}", path.display());
        assert_eq!(
            read_release_manifest_text(&url).expect("file manifest loads"),
            "PLOYZ_VERSION=local\n"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn missing_file_url_manifest_reports_read_failure() {
        let error = read_release_manifest_text("file:///nonexistent/ployz-release.env")
            .expect_err("missing manifest file fails");

        assert!(error.starts_with("failed to read release manifest file://"));
    }

    #[test]
    fn non_https_remote_manifest_url_is_rejected() {
        let error = read_release_manifest_text("http://releases.invalid/ployz-release.env")
            .expect_err("plain http manifest URL fails");

        assert!(error.starts_with("failed to download release manifest http://"));
    }
}
