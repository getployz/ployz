use ployz_core::roles::DaemonProcessRole;
use ployz_host_runner::execution::{
    NatsServerUnitTarget, PloyzdRoleEnvironmentFile, SupervisorUnitSpec, SupervisorUnitTarget,
};
use ployz_host_runner::execution::{SupervisorBackend, SupervisorChange};
use ployz_test_support::host_runner::ployzd_artifact;
use ployz_test_support::ids::machine_id;

#[test]
fn both_supervisors_render_the_same_nats_service_contract() {
    let spec = SupervisorUnitSpec::NatsServer(NatsServerUnitTarget::default_paths());

    let systemd = SupervisorBackend::Systemd
        .render(&spec)
        .expect("systemd service renders");
    let openrc = SupervisorBackend::OpenRc
        .render(&spec)
        .expect("OpenRC service renders");

    assert_eq!(systemd.file_name(), "nats-server.service");
    assert!(systemd.contents().contains("Restart=always"));
    assert!(systemd.contents().contains("kill -s HUP $MAINPID"));
    assert_eq!(openrc.file_name(), "nats-server");
    assert!(
        openrc
            .contents()
            .contains("supervisor=\"supervise-daemon\"")
    );
    assert!(openrc.contents().contains("respawn_delay=5"));
    assert!(openrc.contents().contains("reload()"));
    assert!(openrc.contents().contains("--signal HUP"));
}

#[test]
fn openrc_machine_service_keeps_environment_and_docker_dependencies() {
    let role = DaemonProcessRole::Machine(machine_id("machine_7"));
    let spec = SupervisorUnitSpec::PloyzdRole {
        role: role.clone(),
        artifact: ployzd_artifact(),
        environment_file: PloyzdRoleEnvironmentFile::default_path(),
    };

    let rendered = SupervisorBackend::OpenRc
        .render(&spec)
        .expect("OpenRC machine service renders");

    assert_eq!(rendered.file_name(), "ployzd-machine-machine_7");
    assert!(rendered.contents().contains(". \"/etc/ployz/ployzd.env\""));
    assert!(rendered.contents().contains("need net docker"));
    assert!(
        rendered
            .contents()
            .contains("command_args=\"machine --id machine_7\"")
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
                    "ployzd-machine-machine_7".to_owned(),
                    "default".to_owned(),
                ]
            ),
            (
                "rc-service",
                vec!["ployzd-machine-machine_7".to_owned(), "restart".to_owned()]
            ),
        ]
    );
}

#[test]
fn openrc_kill_sets_the_service_context_for_supervise_daemon() {
    let role = DaemonProcessRole::Machine(machine_id("machine_7"));

    assert_eq!(
        SupervisorBackend::OpenRc.commands(
            SupervisorChange::Kill,
            &SupervisorUnitTarget::PloyzdRole(role),
        ),
        [(
            "env",
            vec![
                "RC_SVCNAME=ployzd-machine-machine_7".to_owned(),
                "supervise-daemon".to_owned(),
                "ployzd-machine-machine_7".to_owned(),
                "--signal".to_owned(),
                "KILL".to_owned(),
            ]
        )]
    );
}
