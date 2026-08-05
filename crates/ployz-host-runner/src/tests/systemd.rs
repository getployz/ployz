use super::support;

use std::path::PathBuf;

use crate::execution::{ArtifactKind, ArtifactTarget};
use crate::execution::{
    PloyzdRole, PloyzdRoleEnvironmentFile, PloyzdRoleUnit, SupervisorUnitFileError, role_unit_name,
};
use support::artifacts::{
    artifact_source as source, artifact_version as version, ployzd_artifact,
    sha256_digest as digest,
};

#[test]
fn role_environment_file_requires_plain_systemd_token_path() {
    assert_eq!(
        PloyzdRoleEnvironmentFile::new(PathBuf::from("/etc/ployz role/ployzd.env")),
        Err(SupervisorUnitFileError::UnsupportedEnvironmentFilePath {
            value: PathBuf::from("/etc/ployz role/ployzd.env"),
        })
    );
    assert_eq!(
        PloyzdRoleEnvironmentFile::new(PathBuf::from("/etc/ployz/%role.env")),
        Err(SupervisorUnitFileError::UnsupportedEnvironmentFilePath {
            value: PathBuf::from("/etc/ployz/%role.env"),
        })
    );
}

#[test]
fn role_units_render_the_supervised_ployzd_commands() {
    let api = PloyzdRole::Api;

    assert_eq!(role_unit_name(&api), "ployzd-api.service");

    let api_unit =
        PloyzdRoleUnit::new(api, &ployzd_artifact(), &role_env()).expect("API unit is valid");
    assert_eq!(api_unit.unit_name(), "ployzd-api.service");
    assert_eq!(
        api_unit.render(),
        "[Unit]\nDescription=Ployz api\nAfter=network-online.target docker.service sys-fs-bpf.mount\nWants=network-online.target docker.service\n\n[Service]\nType=exec\nEnvironmentFile=/etc/ployz/ployzd.env\nExecStart=/usr/local/bin/ployzd api\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
}

#[test]
fn ployzd_role_units_limit_systemd_stop_to_ten_seconds() {
    for role in [
        PloyzdRole::Keeper,
        PloyzdRole::Api,
        PloyzdRole::Gateway,
        PloyzdRole::Dns,
    ] {
        let rendered = PloyzdRoleUnit::new(role, &ployzd_artifact(), &role_env())
            .expect("role unit is valid")
            .render();

        assert!(rendered.contains("\nTimeoutStopSec=10s\n"), "{rendered}");
    }
}

#[test]
fn dns_unit_runs_as_a_dynamic_user_with_only_the_port_53_capability() {
    let rendered = PloyzdRoleUnit::new(PloyzdRole::Dns, &ployzd_artifact(), &role_env())
        .expect("DNS unit is valid")
        .render();

    for directive in [
        "DynamicUser=yes",
        "User=ployz-dns",
        "AmbientCapabilities=CAP_NET_BIND_SERVICE",
        "CapabilityBoundingSet=CAP_NET_BIND_SERVICE",
        "NoNewPrivileges=yes",
    ] {
        assert!(
            rendered.lines().any(|line| line == directive),
            "missing {directive:?} in {rendered:?}"
        );
    }
    assert_eq!(rendered.matches("CAP_NET_BIND_SERVICE").count(), 2);
    assert!(
        rendered.contains(
            "After=network-online.target docker.service ployz-corrosion.service ployzd-api.service\nWants=network-online.target docker.service ployz-corrosion.service ployzd-api.service\n"
        ),
        "{rendered}"
    );
}

#[test]
fn role_units_quote_paths_that_need_systemd_escaping() {
    let spaced_path_artifact = ArtifactTarget::new(
        ArtifactKind::Ployzd,
        version("0.1.0"),
        source("https://example.invalid/ployzd"),
        digest(PLOYZD_DIGEST),
        PathBuf::from("/opt/Ployz Tools/ployzd"),
    )
    .expect("valid artifact install path");
    let percent_path_artifact = ArtifactTarget::new(
        ArtifactKind::Ployzd,
        version("0.1.0"),
        source("https://example.invalid/ployzd"),
        digest(PLOYZD_DIGEST),
        PathBuf::from("/opt/ployz%tools/ployzd"),
    )
    .expect("valid artifact install path");
    let dollar_path_artifact = ArtifactTarget::new(
        ArtifactKind::Ployzd,
        version("0.1.0"),
        source("https://example.invalid/ployzd"),
        digest(PLOYZD_DIGEST),
        PathBuf::from("/opt/ployz$tools/ployzd"),
    )
    .expect("valid artifact install path");

    assert_eq!(
        PloyzdRoleUnit::new(PloyzdRole::Gateway, &spaced_path_artifact, &role_env())
            .expect("spaced path can be quoted")
            .render(),
        "[Unit]\nDescription=Ployz gateway\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=exec\nEnvironmentFile=/etc/ployz/ployzd.env\nExecStart=\"/opt/Ployz Tools/ployzd\" gateway\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    assert_eq!(
        PloyzdRoleUnit::new(PloyzdRole::Gateway, &percent_path_artifact, &role_env())
            .expect("percent path can be escaped")
            .render(),
        "[Unit]\nDescription=Ployz gateway\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=exec\nEnvironmentFile=/etc/ployz/ployzd.env\nExecStart=/opt/ployz%%tools/ployzd gateway\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    assert_eq!(
        PloyzdRoleUnit::new(PloyzdRole::Gateway, &dollar_path_artifact, &role_env())
            .expect("dollar path can be escaped")
            .render(),
        "[Unit]\nDescription=Ployz gateway\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=exec\nEnvironmentFile=/etc/ployz/ployzd.env\nExecStart=\"/opt/ployz$$tools/ployzd\" gateway\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
}

fn role_env() -> PloyzdRoleEnvironmentFile {
    PloyzdRoleEnvironmentFile::new(PathBuf::from("/etc/ployz/ployzd.env"))
        .expect("valid role environment path")
}

const PLOYZD_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
