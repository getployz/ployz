use std::path::PathBuf;

use ployz_core::roles::DaemonProcessRole;
use ployz_host_runner::artifacts::{ArtifactKind, ArtifactTarget};
use ployz_host_runner::systemd::{
    NatsServerUnit, NatsServerUnitTarget, PloyzdRoleEnvironmentFile, PloyzdRoleUnit,
    SupervisorUnitFileError, role_unit_name,
};
use ployz_test_support::host_runner::{
    artifact_source as source, artifact_version as version, ployzd_artifact,
    sha256_digest as digest,
};
use ployz_test_support::ids::machine_id;

#[test]
fn nats_server_unit_renders_supervised_configured_process() {
    let unit = NatsServerUnit::new("/usr/local/bin/nats-server", "/etc/ployz/nats-server.conf")
        .expect("nats unit is valid");

    assert_eq!(unit.unit_name(), "nats-server.service");
    assert_eq!(
        unit.render(),
        "[Unit]\nDescription=Ployz NATS Server\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=exec\nExecStart=/usr/local/bin/nats-server --config /etc/ployz/nats-server.conf\nExecReload=/bin/sh -c \"kill -s HUP $MAINPID\"\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
}

/// Regression (DinD e2e): `/bin/kill` is procps territory and is absent on
/// minimal Debian machines, which made every machine-add reload fail
/// terminally. The reload must rely only on `sh`'s builtin `kill`.
#[test]
fn nats_server_unit_reload_does_not_depend_on_an_external_kill_binary() {
    let unit = NatsServerUnit::new("/usr/local/bin/nats-server", "/etc/ployz/nats-server.conf")
        .expect("nats unit is valid");

    let rendered = unit.render();
    assert!(!rendered.contains("/bin/kill"), "{rendered}");
    assert!(
        rendered.contains("ExecReload=/bin/sh -c \"kill -s HUP $MAINPID\""),
        "{rendered}"
    );
}

#[test]
fn nats_server_unit_target_requires_absolute_paths() {
    assert_eq!(
        NatsServerUnitTarget::new(
            PathBuf::from("bin/nats-server"),
            PathBuf::from("/etc/ployz/nats-server.conf"),
        ),
        Err(SupervisorUnitFileError::RelativePath {
            value: PathBuf::from("bin/nats-server")
        })
    );
    assert_eq!(
        NatsServerUnitTarget::new(PathBuf::from("/usr/local/bin/nats-server"), PathBuf::new()),
        Err(SupervisorUnitFileError::EmptyPath)
    );
}

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
    let machine = DaemonProcessRole::Machine(machine_id("machine_7"));

    assert_eq!(role_unit_name(&machine), "ployzd-machine-machine_7.service");

    let machine_unit = PloyzdRoleUnit::new(machine, &ployzd_artifact(), &role_env())
        .expect("machine unit is valid");
    assert_eq!(machine_unit.unit_name(), "ployzd-machine-machine_7.service");
    assert_eq!(
        machine_unit.render(),
        "[Unit]\nDescription=Ployz machine\nAfter=network-online.target docker.service sys-fs-bpf.mount\nWants=network-online.target docker.service\n\n[Service]\nType=exec\nEnvironmentFile=/etc/ployz/ployzd.env\nExecStart=/usr/local/bin/ployzd machine --id machine_7\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
}

#[test]
fn ployzd_role_units_limit_systemd_stop_to_ten_seconds() {
    for role in [
        DaemonProcessRole::Control,
        DaemonProcessRole::Machine(machine_id("machine_7")),
        DaemonProcessRole::Gateway,
        DaemonProcessRole::Dns,
    ] {
        let rendered = PloyzdRoleUnit::new(role, &ployzd_artifact(), &role_env())
            .expect("role unit is valid")
            .render();

        assert!(rendered.contains("\nTimeoutStopSec=10s\n"), "{rendered}");
    }
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
        PloyzdRoleUnit::new(
            DaemonProcessRole::Gateway,
            &spaced_path_artifact,
            &role_env()
        )
        .expect("spaced path can be quoted")
        .render(),
        "[Unit]\nDescription=Ployz gateway\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=exec\nEnvironmentFile=/etc/ployz/ployzd.env\nExecStart=\"/opt/Ployz Tools/ployzd\" gateway\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    assert_eq!(
        PloyzdRoleUnit::new(
            DaemonProcessRole::Gateway,
            &percent_path_artifact,
            &role_env()
        )
        .expect("percent path can be escaped")
        .render(),
        "[Unit]\nDescription=Ployz gateway\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=exec\nEnvironmentFile=/etc/ployz/ployzd.env\nExecStart=/opt/ployz%%tools/ployzd gateway\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    assert_eq!(
        PloyzdRoleUnit::new(
            DaemonProcessRole::Gateway,
            &dollar_path_artifact,
            &role_env()
        )
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
