//! Linux host capability detection for machine bootstrap.

/// Host package ecosystem used by machine preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPackageFamily {
    Debian,
    Rpm,
    Arch,
    Alpine,
    Suse,
}

/// Process supervisor available on the detected host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorKind {
    Systemd,
    OpenRc,
}

/// Docker installation facility selected for the detected host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockerInstall {
    GetDocker,
    AlpinePackages,
    ArchPackages,
    SusePackages,
    AmazonPackages,
    RhelRepositoryFile,
    CentosRepositoryFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxDistribution {
    Arch,
    Ubuntu,
    Debian,
    Raspbian,
    Centos,
    Fedora,
    Rhel,
    OracleLinux,
    Rocky,
    Sles,
    OpenSuseLeap,
    OpenSuseTumbleweed,
    AlmaLinux,
    AmazonLinux,
    Alpine,
    PostmarketOs,
    TencentOs,
}

/// Capability profile selected from a Linux `/etc/os-release` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPlatformProfile {
    distribution: LinuxDistribution,
    version_id: Option<String>,
    package_family: HostPackageFamily,
    supervisor: SupervisorKind,
    docker_install: DockerInstall,
}

impl HostPlatformProfile {
    /// Returns the host's reported release version when present.
    #[must_use]
    pub fn version_id(&self) -> Option<&str> {
        self.version_id.as_deref()
    }

    /// Returns the host package ecosystem.
    #[must_use]
    pub const fn package_family(&self) -> HostPackageFamily {
        self.package_family
    }

    pub(crate) const fn requires_iproute_tc_package(&self) -> bool {
        matches!(self.distribution, LinuxDistribution::Fedora)
    }

    /// Returns the host process supervisor.
    #[must_use]
    pub const fn supervisor(&self) -> SupervisorKind {
        self.supervisor
    }

    /// Returns the Docker installation facility for the host.
    #[must_use]
    pub const fn docker_install(&self) -> DockerInstall {
        self.docker_install
    }
}

/// Failure to select a supported host capability profile.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostPlatformDetectionError {
    #[error("/etc/os-release is missing ID")]
    MissingId,
    #[error("host distribution {id} {version_id:?} is unsupported")]
    Unsupported {
        id: String,
        version_id: Option<String>,
    },
}

/// Parses `/etc/os-release` and selects the host capabilities used by Host Runner.
///
/// # Errors
///
/// Returns [`HostPlatformDetectionError::MissingId`] when `ID` is absent or empty,
/// and [`HostPlatformDetectionError::Unsupported`] when the distribution is not in
/// the supported compatibility matrix.
pub fn detect_host_platform(
    os_release: &str,
) -> Result<HostPlatformProfile, HostPlatformDetectionError> {
    let id = os_release_value(os_release, "ID")
        .filter(|id| !id.is_empty())
        .ok_or(HostPlatformDetectionError::MissingId)?
        .to_ascii_lowercase();
    let version_id = os_release_value(os_release, "VERSION_ID");
    let Some(distribution) = normalize_distribution_id(&id) else {
        return Err(HostPlatformDetectionError::Unsupported { id, version_id });
    };
    let profile = match distribution {
        LinuxDistribution::Arch => profile(
            distribution,
            version_id,
            HostPackageFamily::Arch,
            SupervisorKind::Systemd,
            DockerInstall::ArchPackages,
        ),
        LinuxDistribution::Ubuntu => profile(
            distribution,
            version_id,
            HostPackageFamily::Debian,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        LinuxDistribution::Debian => profile(
            distribution,
            version_id,
            HostPackageFamily::Debian,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        LinuxDistribution::Raspbian => profile(
            distribution,
            version_id,
            HostPackageFamily::Debian,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        LinuxDistribution::Centos
        | LinuxDistribution::Fedora
        | LinuxDistribution::Rhel
        | LinuxDistribution::OracleLinux => profile(
            distribution,
            version_id,
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        LinuxDistribution::Rocky => profile(
            distribution,
            version_id,
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::RhelRepositoryFile,
        ),
        LinuxDistribution::Sles
        | LinuxDistribution::OpenSuseLeap
        | LinuxDistribution::OpenSuseTumbleweed => profile(
            distribution,
            version_id,
            HostPackageFamily::Suse,
            SupervisorKind::Systemd,
            DockerInstall::SusePackages,
        ),
        LinuxDistribution::AlmaLinux => profile(
            distribution,
            version_id,
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::CentosRepositoryFile,
        ),
        LinuxDistribution::AmazonLinux => profile(
            distribution,
            version_id,
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::AmazonPackages,
        ),
        LinuxDistribution::Alpine | LinuxDistribution::PostmarketOs => profile(
            distribution,
            version_id,
            HostPackageFamily::Alpine,
            SupervisorKind::OpenRc,
            DockerInstall::AlpinePackages,
        ),
        LinuxDistribution::TencentOs => profile(
            distribution,
            version_id,
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::CentosRepositoryFile,
        ),
    };
    Ok(profile)
}

fn normalize_distribution_id(id: &str) -> Option<LinuxDistribution> {
    let distribution = match id {
        "arch" | "manjaro" | "manjaro-arm" | "endeavouros" | "cachyos" => LinuxDistribution::Arch,
        "ubuntu" | "pop" | "linuxmint" | "zorin" => LinuxDistribution::Ubuntu,
        "debian" => LinuxDistribution::Debian,
        "raspbian" => LinuxDistribution::Raspbian,
        "centos" => LinuxDistribution::Centos,
        "fedora" | "fedora-asahi-remix" => LinuxDistribution::Fedora,
        "rhel" => LinuxDistribution::Rhel,
        "ol" => LinuxDistribution::OracleLinux,
        "rocky" => LinuxDistribution::Rocky,
        "sles" => LinuxDistribution::Sles,
        "opensuse-leap" => LinuxDistribution::OpenSuseLeap,
        "opensuse-tumbleweed" => LinuxDistribution::OpenSuseTumbleweed,
        "almalinux" => LinuxDistribution::AlmaLinux,
        "amzn" => LinuxDistribution::AmazonLinux,
        "alpine" => LinuxDistribution::Alpine,
        "postmarketos" => LinuxDistribution::PostmarketOs,
        "tencentos" => LinuxDistribution::TencentOs,
        _ => return None,
    };
    Some(distribution)
}

const fn profile(
    distribution: LinuxDistribution,
    version_id: Option<String>,
    package_family: HostPackageFamily,
    supervisor: SupervisorKind,
    docker_install: DockerInstall,
) -> HostPlatformProfile {
    HostPlatformProfile {
        distribution,
        version_id,
        package_family,
        supervisor,
        docker_install,
    }
}

fn os_release_value(os_release: &str, key: &str) -> Option<String> {
    os_release.lines().rev().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate.trim() == key).then(|| unquote_os_release_value(value.trim()))
    })
}

fn unquote_os_release_value(value: &str) -> String {
    match value.as_bytes() {
        [b'"', .., b'"'] | [b'\'', .., b'\''] => value[1..value.len() - 1].to_owned(),
        _ => value.to_owned(),
    }
}
