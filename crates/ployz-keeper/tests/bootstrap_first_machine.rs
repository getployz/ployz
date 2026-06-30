mod support;

use ployz_core::nats_config::NatsUserPublicKey;
use ployz_core::roles::{DaemonProcessRole, InstallRolePolicy};
use ployz_keeper::artifacts::ArtifactKind;
use ployz_keeper::steps::{
    FirstMachineInstallTarget, KeeperStep, PloyzdRoleEnvironmentStep, first_machine_install_plan,
};
use ployz_keeper::systemd::SupervisorUnitTarget;
use ployz_test_support::ids::machine_id;
use ployz_test_support::keeper::{nats_server_artifact, ployzd_artifact};
use support::bootstrap::*;

#[test]
fn first_machine_install_starts_nats_and_core_roles_without_join_token() {
    let machine_id = machine_id("machine_1");
    let target = FirstMachineInstallTarget::new(
        machine_id.clone(),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
        test_identity().clone(),
    );
    let role_environment = target.role_environment.clone();
    let plan = first_machine_install_plan(target);

    assert!(installs_artifact_kind(&plan, ArtifactKind::Ployzd));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfBytecode));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfCtl));
    assert!(installs_artifact_kind(&plan, ArtifactKind::NatsServer));
    assert!(writes_nats_server_unit(&plan));
    assert!(writes_ployzd_role_units(&plan));
    assert!(plan.steps().contains(&KeeperStep::WriteNatsServerConfig(
        first_machine_nats_target(machine_id.clone())
    )));
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, KeeperStep::WriteNatsTlsMaterial(_)))
    );
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, KeeperStep::WriteNatsAuthorizedUsers(_)))
    );
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, KeeperStep::WriteNatsClientCredentials(_)))
    );
    assert!(plan_writes_unit(&plan, &SupervisorUnitTarget::NatsServer));
    assert!(plan.steps().contains(&KeeperStep::StartSupervisorUnit(
        SupervisorUnitTarget::NatsServer
    )));

    for role in [
        DaemonProcessRole::Control,
        DaemonProcessRole::Machine(machine_id),
    ] {
        assert!(
            plan.steps()
                .contains(&KeeperStep::WritePloyzdRoleEnvironment(
                    PloyzdRoleEnvironmentStep {
                        role: role.clone(),
                        target: role_environment.clone(),
                    }
                ))
        );
        let unit = SupervisorUnitTarget::PloyzdRole(role);
        assert!(plan_writes_unit(&plan, &unit));
        assert!(
            plan.steps()
                .contains(&KeeperStep::StartSupervisorUnit(unit))
        );
    }

    assert!(!plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Gateway)
    ));
    assert!(
        !plan
            .steps()
            .iter()
            .any(|step| matches!(step, KeeperStep::StoreJoinMaterial(_)))
    );
}

#[test]
fn first_machine_can_authorize_cloud_user_public_key() {
    let cloud_public_key = user_public_key('C');
    let target = FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
        test_identity().clone(),
    )
    .with_additional_user_public_key(cloud_public_key.clone());
    let plan = first_machine_install_plan(target);

    let rendered = plan
        .steps()
        .iter()
        .find_map(|step| match step {
            KeeperStep::WriteNatsAuthorizedUsers(users) => Some(users.render()),
            KeeperStep::VerifyHost(_)
            | KeeperStep::PrepareContainerRuntime(_)
            | KeeperStep::VerifyContainerRuntime(_)
            | KeeperStep::InstallArtifact(_)
            | KeeperStep::WriteNatsTlsMaterial(_)
            | KeeperStep::WriteNatsClientCredentials(_)
            | KeeperStep::WriteNatsServerConfig(_)
            | KeeperStep::WriteMachineJoinTemplate(_)
            | KeeperStep::WritePloyzdRoleEnvironment(_)
            | KeeperStep::WriteSupervisorUnit(_)
            | KeeperStep::StartSupervisorUnit(_)
            | KeeperStep::RestartSupervisorUnit(_)
            | KeeperStep::StoreJoinMaterial(_) => None,
        })
        .expect("first-machine plan writes authorized users");

    assert!(rendered.contains(test_identity().controller.public.as_str()));
    assert!(rendered.contains(test_identity().operator.public.as_str()));
    assert!(rendered.contains(test_identity().join.public.as_str()));
    assert!(rendered.contains(cloud_public_key.as_str()));
    assert_eq!(rendered.matches("# ployz-principal: user").count(), 2);
}

#[test]
fn first_machine_role_envs_carry_tls_url_and_role_scoped_seed_paths() {
    let target = FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all(),
        test_identity().clone(),
    );

    let control_env = target
        .role_environment
        .render_for_role(&DaemonProcessRole::Control);
    assert!(control_env.starts_with("PLOYZ_NATS_URL=tls://127.0.0.1:4222\n"));
    assert!(control_env.contains("PLOYZ_NATS_CA_FILE=/var/lib/ployz/nats/ca.pem\n"));
    assert!(
        control_env.contains("PLOYZ_NATS_NKEY_SEED_FILE=/var/lib/ployz/nats/controller.seed\n")
    );
    assert!(control_env.contains("PLOYZ_JOIN_NKEY_SEED_FILE=/var/lib/ployz/nats/join.seed\n"));

    // Machine and gateway point at the fixed machine.seed path, which does not
    // exist at install time — there is no controller-seed fallback.
    for role in [
        DaemonProcessRole::Machine(machine_id("machine_1")),
        DaemonProcessRole::Gateway,
    ] {
        let env = target.role_environment.render_for_role(&role);
        assert!(env.starts_with("PLOYZ_NATS_URL=tls://127.0.0.1:4222\n"));
        assert!(env.contains("PLOYZ_NATS_CA_FILE=/var/lib/ployz/nats/ca.pem\n"));
        assert!(env.contains("PLOYZ_NATS_NKEY_SEED_FILE=/var/lib/ployz/nats/machine.seed\n"));
        assert!(!env.contains("controller.seed"));
        if matches!(role, DaemonProcessRole::Gateway) {
            assert!(env.contains("PLOYZ_GATEWAY_LISTEN_ADDR=0.0.0.0:80\n"));
        } else {
            assert!(!env.contains("PLOYZ_GATEWAY_LISTEN_ADDR"));
        }
    }

    assert_eq!(
        target
            .role_environment
            .file_for_role(&DaemonProcessRole::Control)
            .path(),
        std::path::Path::new("/etc/ployz/ployzd-control.env")
    );
}

fn user_public_key(fill: char) -> NatsUserPublicKey {
    NatsUserPublicKey::try_new(format!("U{}", fill.to_string().repeat(55)))
        .expect("valid user public key")
}

#[test]
fn first_machine_public_ip_flips_the_listener_external_in_the_secured_config() {
    let target = FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
        test_identity().clone(),
    )
    .with_machine_public_ip("203.0.113.10".parse().expect("valid IP"));
    let plan = first_machine_install_plan(target);

    let rendered = plan
        .steps()
        .iter()
        .find_map(|step| match step {
            KeeperStep::WriteNatsServerConfig(config) => Some(config.render_config()),
            KeeperStep::VerifyHost(_)
            | KeeperStep::PrepareContainerRuntime(_)
            | KeeperStep::VerifyContainerRuntime(_)
            | KeeperStep::InstallArtifact(_)
            | KeeperStep::WritePloyzdRoleEnvironment(_)
            | KeeperStep::WriteNatsTlsMaterial(_)
            | KeeperStep::WriteNatsAuthorizedUsers(_)
            | KeeperStep::WriteNatsClientCredentials(_)
            | KeeperStep::WriteMachineJoinTemplate(_)
            | KeeperStep::WriteSupervisorUnit(_)
            | KeeperStep::StartSupervisorUnit(_)
            | KeeperStep::RestartSupervisorUnit(_)
            | KeeperStep::StoreJoinMaterial(_) => None,
        })
        .expect("first-machine plan writes the nats config");

    // TLS + authorization land in the same rendered config that opens the
    // listener — a plaintext external listener is unrepresentable.
    assert!(rendered.contains("host: 0.0.0.0\n"));
    assert!(rendered.contains("client_advertise: 203.0.113.10:4222\n"));
    assert!(rendered.contains("tls {\n"));
    assert!(rendered.contains("include \"authorized-users.conf\"\n"));
}

#[test]
fn first_machine_default_install_includes_gateway_and_dns_roles() {
    let plan = first_machine_install_plan(FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all(),
        test_identity().clone(),
    ));

    for role in [DaemonProcessRole::Gateway, DaemonProcessRole::Dns] {
        let unit = SupervisorUnitTarget::PloyzdRole(role);
        assert!(plan_writes_unit(&plan, &unit));
        assert!(
            plan.steps()
                .contains(&KeeperStep::StartSupervisorUnit(unit))
        );
    }
}

#[test]
fn first_machine_dns_opt_out_skips_only_the_dns_role() {
    let plan = first_machine_install_plan(FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all().without_dns(),
        test_identity().clone(),
    ));

    assert!(plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Gateway)
    ));
    assert!(!plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Dns)
    ));
}

#[test]
fn first_machine_gateway_opt_out_skips_only_the_gateway_role() {
    let plan = first_machine_install_plan(FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all().without_gateway(),
        test_identity().clone(),
    ));

    assert!(!plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Gateway)
    ));
    assert!(plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Dns)
    ));
}
