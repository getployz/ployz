//! Versioned release manifest parsing for Host Runner-owned installs.

use std::time::Duration;

use ployz_core::install::{
    AbsoluteInstallPath, ExactPloyzVersion, InstallArtifactSource, InstallArtifactSpec,
    InstallArtifactVersion, InstallSha256Digest, ReleasePlatformFailure,
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
    corrosion_embedded_version: String,
    corrosion_url: String,
    corrosion_sha256: String,
    corrosion_schema_url: String,
    corrosion_schema_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifacts {
    pub ployzd: InstallArtifactSpec,
    pub ebpf_bytecode: InstallArtifactSpec,
    pub ebpf_ctl: InstallArtifactSpec,
    pub corrosion: InstallArtifactSpec,
    pub corrosion_schema: InstallArtifactSpec,
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
            corrosion_embedded_version: manifest_value(
                contents,
                "PLOYZ_CORROSION_EMBEDDED_VERSION",
            )?,
            corrosion_url: manifest_value(contents, "PLOYZ_CORROSION_URL")?,
            corrosion_sha256: manifest_value(contents, "PLOYZ_CORROSION_SHA256")?,
            corrosion_schema_url: manifest_value(contents, "PLOYZ_CORROSION_SCHEMA_URL")?,
            corrosion_schema_sha256: manifest_value(contents, "PLOYZ_CORROSION_SCHEMA_SHA256")?,
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

    #[must_use]
    pub fn corrosion_embedded_version(&self) -> &str {
        &self.corrosion_embedded_version
    }

    pub fn install_artifacts_for(
        &self,
        local_platform: ReleasePlatform,
    ) -> Result<ReleaseArtifacts, String> {
        self.require_platform(local_platform)?;
        self.install_artifacts()
    }

    pub fn install_artifacts(&self) -> Result<ReleaseArtifacts, String> {
        Ok(ReleaseArtifacts {
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
            corrosion: artifact_spec(
                &self.corrosion_embedded_version,
                &self.corrosion_url,
                &self.corrosion_sha256,
                "/usr/local/bin/corrosion",
            )?,
            corrosion_schema: artifact_spec(
                "v1",
                &self.corrosion_schema_url,
                &self.corrosion_schema_sha256,
                "/usr/local/lib/ployz/corrosion-schema-v1.sql",
            )?,
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

/// Reads the manifest text behind a release manifest URL.
pub fn read_release_manifest_text(url: &str) -> Result<String, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read release manifest {url}: {error}"));
    }
    let parsed = url::Url::parse(url)
        .map_err(|error| format!("failed to download release manifest {url}: {error}"))?;
    if parsed.scheme() != "https" || parsed.host().is_none() {
        return Err(format!(
            "failed to download release manifest {url}: URL must use https"
        ));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .timeout_connect(Some(Duration::from_secs(5)))
        .build()
        .into();
    agent
        .get(url)
        .call()
        .and_then(|mut response| response.body_mut().read_to_string())
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

    fn manifest(platform: Option<&str>) -> String {
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
             PLOYZ_CORROSION_EMBEDDED_VERSION=corrosion 0.2.0-beta.0\n\
             PLOYZ_CORROSION_URL=https://example.test/corrosion\n\
             PLOYZ_CORROSION_SHA256={SHA}\n\
             PLOYZ_CORROSION_SCHEMA_URL=https://example.test/corrosion-schema-v1.sql\n\
             PLOYZ_CORROSION_SCHEMA_SHA256={SHA}\n"
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
        let manifest =
            ReleaseManifest::parse(&manifest(Some("linux-amd64"))).expect("manifest parses");
        let artifacts = manifest.install_artifacts().expect("artifacts build");

        assert_eq!(
            artifacts.ployzd.install_path.as_str(),
            "/usr/local/bin/ployzd"
        );
        assert_eq!(
            artifacts.corrosion.install_path.as_str(),
            "/usr/local/bin/corrosion"
        );
        assert_eq!(
            artifacts.corrosion.version.as_str(),
            "corrosion 0.2.0-beta.0"
        );
        assert_eq!(
            artifacts.corrosion_schema.install_path.as_str(),
            "/usr/local/lib/ployz/corrosion-schema-v1.sql"
        );
        assert_eq!(artifacts.corrosion_schema.version.as_str(), "v1");
    }

    #[test]
    fn release_manifest_rejects_artifacts_for_another_local_platform() {
        let manifest =
            ReleaseManifest::parse(&manifest(Some("linux-amd64"))).expect("manifest parses");

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
        let error = ReleaseManifest::parse(&manifest(None))
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
        let error = ReleaseManifest::parse(&manifest(Some("linux-riscv64")))
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
        let manifest =
            ReleaseManifest::parse(&manifest(Some("linux-arm64"))).expect("manifest parses");

        assert_eq!(manifest.platform(), ReleasePlatform::LinuxArm64);
    }

    #[test]
    fn release_manifest_exposes_ployz_release_version() {
        let manifest =
            ReleaseManifest::parse(&manifest(Some("linux-amd64"))).expect("manifest parses");

        assert_eq!(manifest.ployz_version(), "0.1.0");
    }

    #[test]
    fn release_manifest_exposes_corrosion_embedded_version() {
        let manifest =
            ReleaseManifest::parse(&manifest(Some("linux-amd64"))).expect("manifest parses");

        assert_eq!(
            manifest.corrosion_embedded_version(),
            "corrosion 0.2.0-beta.0"
        );
    }

    #[test]
    fn release_manifest_rejects_mutable_ployz_release_version() {
        let manifest =
            manifest(Some("linux-amd64")).replace("PLOYZ_VERSION=0.1.0", "PLOYZ_VERSION=alpha");

        let error = ReleaseManifest::parse(&manifest).expect_err("release must be immutable");

        assert_eq!(
            error.to_string(),
            "release manifest PLOYZ_VERSION is invalid: release version must be exact, got mutable \"alpha\""
        );
    }

    #[test]
    fn release_manifest_requires_the_complete_corrosion_tuple() {
        let incomplete = manifest(Some("linux-amd64"))
            .replace(&format!("PLOYZ_CORROSION_SCHEMA_SHA256={SHA}\n"), "");

        let error = ReleaseManifest::parse(&incomplete)
            .expect_err("Corrosion binary, schema, and embedded version are one release tuple");

        assert!(error.to_string().contains("PLOYZ_CORROSION_SCHEMA_SHA256"));
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
