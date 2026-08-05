use std::path::PathBuf;

use crate::execution::{
    PloyzdArtifactStore, PloyzdKeeperRevertUnit, PloyzdRole, PloyzdRoleEnvironmentFile,
    PloyzdRoleUnit, SupervisorUnitFileError, role_unit_name,
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
fn role_units_execute_the_stable_current_link() {
    let api = PloyzdRole::Api;

    assert_eq!(role_unit_name(&api), "ployzd-api.service");

    let api_unit =
        PloyzdRoleUnit::new(api, &artifact_store(), &role_env()).expect("API unit is valid");
    assert_eq!(api_unit.unit_name(), "ployzd-api.service");
    assert_eq!(
        api_unit.render(),
        "[Unit]\nDescription=Ployz api\nAfter=network-online.target docker.service sys-fs-bpf.mount\nWants=network-online.target docker.service\n\n[Service]\nType=exec\nEnvironmentFile=/etc/ployz/ployzd.env\nExecStart=/var/lib/ployz/current api\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
}

#[test]
fn keeper_unit_arms_systemd_rollback_on_a_failed_new_binary() {
    let rendered = PloyzdRoleUnit::new(PloyzdRole::Keeper, &artifact_store(), &role_env())
        .expect("Keeper unit is valid")
        .render();

    assert!(rendered.contains("ExecStart=/var/lib/ployz/current keeper"));
    assert!(rendered.contains("Restart=on-failure"));
    assert!(rendered.contains("RestartPreventExitStatus=75"));
    assert!(rendered.contains("OnFailure=ployzd-keeper-revert.service"));
    assert!(!rendered.contains("Restart=always"));
}

#[test]
fn ployzd_role_units_limit_systemd_stop_to_ten_seconds() {
    for role in [
        PloyzdRole::Keeper,
        PloyzdRole::Api,
        PloyzdRole::Gateway,
        PloyzdRole::Dns,
    ] {
        let rendered = PloyzdRoleUnit::new(role, &artifact_store(), &role_env())
            .expect("role unit is valid")
            .render();

        assert!(rendered.contains("\nTimeoutStopSec=10s\n"), "{rendered}");
    }
}

#[test]
fn dns_unit_runs_as_a_dynamic_user_with_only_the_port_53_capability() {
    let rendered = PloyzdRoleUnit::new(PloyzdRole::Dns, &artifact_store(), &role_env())
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
fn keeper_revert_unit_uses_only_fixed_system_commands() {
    let rendered = PloyzdKeeperRevertUnit::new(artifact_store())
        .render()
        .expect("revert unit renders");

    assert_eq!(
        PloyzdKeeperRevertUnit::unit_name(),
        "ployzd-keeper-revert.service"
    );
    assert!(rendered.contains("ConditionPathExists=/var/lib/ployz/upgrade-pending"));
    assert!(
        rendered
            .contains("ExecStart=/usr/bin/ln -sfnT /var/lib/ployz/previous /var/lib/ployz/current")
    );
    assert!(rendered.contains("ExecStart=/usr/bin/rm /var/lib/ployz/upgrade-pending"));
    for role in ["keeper", "api", "gateway", "dns"] {
        assert!(
            rendered.contains(&format!(
                "ExecStart=/usr/bin/systemctl restart ployzd-{role}.service"
            )),
            "{rendered}"
        );
    }
    assert!(!rendered.contains("ExecStart=/var/lib/ployz/current"));
    assert!(!rendered.contains("artifacts/"));
}

#[test]
fn role_units_quote_stable_paths_that_need_systemd_escaping() {
    let store =
        PloyzdArtifactStore::new(PathBuf::from("/opt/Ployz Tools")).expect("absolute state path");

    assert_eq!(
        PloyzdRoleUnit::new(PloyzdRole::Gateway, &store, &role_env())
            .expect("spaced path can be quoted")
            .render(),
        "[Unit]\nDescription=Ployz gateway\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=exec\nEnvironmentFile=/etc/ployz/ployzd.env\nExecStart=\"/opt/Ployz Tools/current\" gateway\nTimeoutStopSec=10s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
}

fn artifact_store() -> PloyzdArtifactStore {
    PloyzdArtifactStore::new(PathBuf::from("/var/lib/ployz")).expect("absolute state path")
}

fn role_env() -> PloyzdRoleEnvironmentFile {
    PloyzdRoleEnvironmentFile::new(PathBuf::from("/etc/ployz/ployzd.env"))
        .expect("valid role environment path")
}
