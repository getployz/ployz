use crate::execution::{
    PloyzdArtifactStore, PloyzdRole, PloyzdRoleEnvironmentFile, SupervisorUnitSpec,
    SupervisorUnitTarget,
};
use crate::execution::{PloyzdUpgradeRollback, SupervisorBackend, SupervisorChange};

#[test]
fn openrc_explicitly_does_not_claim_keeper_upgrade_rollback() {
    assert_eq!(
        SupervisorBackend::Systemd.ployzd_upgrade_rollback(),
        PloyzdUpgradeRollback::SystemdOnFailure
    );
    assert_eq!(
        SupervisorBackend::OpenRc.ployzd_upgrade_rollback(),
        PloyzdUpgradeRollback::Unsupported
    );
}

#[test]
fn openrc_api_service_keeps_environment_and_docker_dependencies() {
    let role = PloyzdRole::Api;
    let spec = SupervisorUnitSpec::PloyzdRole {
        role,
        artifact_store: PloyzdArtifactStore::new("/var/lib/ployz".into())
            .expect("absolute artifact-store state"),
        environment_file: PloyzdRoleEnvironmentFile::default_path(),
    };

    let rendered = SupervisorBackend::OpenRc
        .render(&spec)
        .expect("OpenRC API service renders");

    assert_eq!(rendered.file_name(), "ployzd-api");
    assert!(rendered.contents().contains(". \"/etc/ployz/ployzd.env\""));
    assert!(rendered.contents().contains("need net docker"));
    assert!(rendered.contents().contains("command_args=\"api\""));
    assert!(
        rendered
            .contents()
            .contains("command_user=\"ployz-api:ployz-api\"")
    );
    assert_eq!(
        SupervisorBackend::OpenRc.commands(
            SupervisorChange::InstallAndStart,
            &SupervisorUnitTarget::PloyzdRole(role),
        ),
        [
            (
                "rc-update",
                vec![
                    "add".to_owned(),
                    "ployzd-api".to_owned(),
                    "default".to_owned(),
                ]
            ),
            (
                "rc-service",
                vec!["ployzd-api".to_owned(), "restart".to_owned()]
            ),
        ]
    );
}

#[test]
fn openrc_kill_sets_the_service_context_for_supervise_daemon() {
    let role = PloyzdRole::Api;

    assert_eq!(
        SupervisorBackend::OpenRc.commands(
            SupervisorChange::Kill,
            &SupervisorUnitTarget::PloyzdRole(role),
        ),
        [(
            "env",
            vec![
                "RC_SVCNAME=ployzd-api".to_owned(),
                "supervise-daemon".to_owned(),
                "ployzd-api".to_owned(),
                "--signal".to_owned(),
                "KILL".to_owned(),
            ]
        )]
    );
}
