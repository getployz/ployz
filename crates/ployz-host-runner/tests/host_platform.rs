use ployz_host_runner::execution::host_platform::{
    DockerInstall, HostPackageFamily, SupervisorKind, detect_host_platform,
};

#[test]
fn supported_distributions_select_host_capabilities() {
    let cases = [
        (
            "arch",
            HostPackageFamily::Arch,
            SupervisorKind::Systemd,
            DockerInstall::ArchPackages,
        ),
        (
            "ubuntu",
            HostPackageFamily::Debian,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        (
            "debian",
            HostPackageFamily::Debian,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        (
            "raspbian",
            HostPackageFamily::Debian,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        (
            "centos",
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        (
            "fedora",
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        (
            "rhel",
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        (
            "ol",
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::GetDocker,
        ),
        (
            "rocky",
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::RhelRepositoryFile,
        ),
        (
            "sles",
            HostPackageFamily::Suse,
            SupervisorKind::Systemd,
            DockerInstall::SusePackages,
        ),
        (
            "opensuse-leap",
            HostPackageFamily::Suse,
            SupervisorKind::Systemd,
            DockerInstall::SusePackages,
        ),
        (
            "opensuse-tumbleweed",
            HostPackageFamily::Suse,
            SupervisorKind::Systemd,
            DockerInstall::SusePackages,
        ),
        (
            "almalinux",
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::CentosRepositoryFile,
        ),
        (
            "amzn",
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::AmazonPackages,
        ),
        (
            "alpine",
            HostPackageFamily::Alpine,
            SupervisorKind::OpenRc,
            DockerInstall::AlpinePackages,
        ),
        (
            "postmarketos",
            HostPackageFamily::Alpine,
            SupervisorKind::OpenRc,
            DockerInstall::AlpinePackages,
        ),
        (
            "tencentos",
            HostPackageFamily::Rpm,
            SupervisorKind::Systemd,
            DockerInstall::CentosRepositoryFile,
        ),
    ];

    for (id, package_family, supervisor, docker_install) in cases {
        let profile = detect_host_platform(&format!("ID=\"{id}\"\nVERSION_ID=\"9.4\"\n"))
            .expect("supported distribution is detected");

        assert_eq!(
            (
                profile.package_family(),
                profile.supervisor(),
                profile.docker_install()
            ),
            (package_family, supervisor, docker_install),
            "wrong host capabilities for {id}",
        );
    }
}

#[test]
fn distribution_aliases_normalize_to_their_base_profile() {
    let cases = [
        ("manjaro", "arch"),
        ("manjaro-arm", "arch"),
        ("endeavouros", "arch"),
        ("cachyos", "arch"),
        ("fedora-asahi-remix", "fedora"),
        ("pop", "ubuntu"),
        ("linuxmint", "ubuntu"),
        ("zorin", "ubuntu"),
    ];

    for (alias, canonical) in cases {
        let alias_profile =
            detect_host_platform(&format!("ID={alias}\n")).expect("supported alias is detected");
        let canonical_profile = detect_host_platform(&format!("ID={canonical}\n"))
            .expect("canonical distribution is detected");

        assert_eq!(
            alias_profile, canonical_profile,
            "{alias} did not normalize to {canonical}",
        );
    }
}
