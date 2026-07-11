mod support;

use ployz_core::dataplane::MachineEndpointSupernet;
use ployz_core::install::{WrappedCaKey, WrappedCoreSeeds};
use ployz_core::nats_config::{CredentialGrant, CredentialName, CredentialRole, NatsUserPublicKey};
use ployz_core::roles::{DaemonProcessRole, InstallRolePolicy};
use ployz_host_runner::artifacts::ArtifactKind;
use ployz_host_runner::steps::{
    FirstMachineInstallTarget, HostRunnerStep, PloyzdRoleEnvironmentStep,
    first_machine_install_plan,
};
use ployz_host_runner::systemd::SupervisorUnitTarget;
use ployz_test_support::host_runner::{nats_server_artifact, ployzd_artifact};
use ployz_test_support::ids::machine_id;
use support::bootstrap::*;

#[test]
fn first_machine_install_starts_nats_and_core_roles_without_join_token() {
    let machine_id = machine_id("machine_1");
    let target = FirstMachineInstallTarget::new(
        machine_id.clone(),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all().without_gateway(),
        test_identity().clone(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    );
    let role_environment = target.role_environment.clone();
    let plan = first_machine_install_plan(target);

    assert!(installs_artifact_kind(&plan, ArtifactKind::Ployzd));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfBytecode));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfCtl));
    assert!(installs_artifact_kind(&plan, ArtifactKind::NatsServer));
    assert!(writes_nats_server_unit(&plan));
    assert!(writes_ployzd_role_units(&plan));
    assert!(
        plan.steps()
            .contains(&HostRunnerStep::WriteNatsServerConfig(
                first_machine_nats_target(machine_id.clone())
            ))
    );
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, HostRunnerStep::WriteNatsTlsMaterial(_)))
    );
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, HostRunnerStep::WriteNatsAuthorizedUsers(_)))
    );
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, HostRunnerStep::WriteNatsClientCredentials(_)))
    );
    assert!(plan_writes_unit(&plan, &SupervisorUnitTarget::NatsServer));
    assert!(plan.steps().contains(&HostRunnerStep::StartSupervisorUnit(
        SupervisorUnitTarget::NatsServer
    )));

    for role in [
        DaemonProcessRole::Control,
        DaemonProcessRole::Machine(machine_id),
    ] {
        assert!(
            plan.steps()
                .contains(&HostRunnerStep::WritePloyzdRoleEnvironment(
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
                .contains(&HostRunnerStep::StartSupervisorUnit(unit))
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
            .any(|step| matches!(step, HostRunnerStep::StoreJoinMaterial(_)))
    );
}

#[test]
fn first_machine_names_founder_and_cloud_credentials() {
    let cloud_public_key = user_public_key('C');
    let target = FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all().without_gateway(),
        test_identity().clone(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    )
    .with_founder_hostname("founder.example")
    .with_additional_credential(CredentialGrant {
        public_key: cloud_public_key.clone(),
        name: CredentialName::try_new("Ployz Cloud").expect("name"),
        role: CredentialRole::Operator,
    });
    let plan = first_machine_install_plan(target);

    let rendered = plan
        .steps()
        .iter()
        .find_map(|step| match step {
            HostRunnerStep::WriteNatsAuthorizedUsers(users) => Some(users.render()),
            HostRunnerStep::VerifyHost(_)
            | HostRunnerStep::PrepareDataplaneHost
            | HostRunnerStep::PrepareContainerRuntime(_, _)
            | HostRunnerStep::VerifyContainerRuntime(_)
            | HostRunnerStep::InstallArtifact(_)
            | HostRunnerStep::WriteNatsTlsMaterial(_)
            | HostRunnerStep::WriteNatsClientCredentials(_)
            | HostRunnerStep::WriteNatsServerConfig(_)
            | HostRunnerStep::WriteMachineJoinTemplate(_)
            | HostRunnerStep::WritePloyzdRoleEnvironment(_)
            | HostRunnerStep::WriteSupervisorUnit(_)
            | HostRunnerStep::StartSupervisorUnit(_)
            | HostRunnerStep::RestartSupervisorUnit(_)
            | HostRunnerStep::StoreJoinMaterial(_) => None,
        })
        .expect("first-machine plan writes authorized users");

    assert!(rendered.contains(test_identity().controller.public.as_str()));
    assert!(rendered.contains(test_identity().operator.public.as_str()));
    assert!(rendered.contains(test_identity().join.public.as_str()));
    assert!(rendered.contains(cloud_public_key.as_str()));
    assert!(rendered.contains("# ployz-credential-name: Founder operator (founder.example)"));
    assert!(rendered.contains("# ployz-credential-name: Ployz Cloud"));
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
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
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

fn user_public_key(_fill: char) -> NatsUserPublicKey {
    NatsUserPublicKey::try_new("UBCXCMGAZQZN55X5TTTWMB5CZNZIKJHEDZJOJ3TV63NKPJ6FRXSR2ZO4")
        .expect("valid user public key")
}

#[test]
fn first_machine_public_ip_flips_the_listener_external_in_the_secured_config() {
    let target = FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all().without_gateway(),
        test_identity().clone(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    )
    .with_machine_public_ip("203.0.113.10".parse().expect("valid IP"));
    let plan = first_machine_install_plan(target);

    let rendered = plan
        .steps()
        .iter()
        .find_map(|step| match step {
            HostRunnerStep::WriteNatsServerConfig(config) => Some(config.render_config()),
            HostRunnerStep::VerifyHost(_)
            | HostRunnerStep::PrepareDataplaneHost
            | HostRunnerStep::PrepareContainerRuntime(_, _)
            | HostRunnerStep::VerifyContainerRuntime(_)
            | HostRunnerStep::InstallArtifact(_)
            | HostRunnerStep::WritePloyzdRoleEnvironment(_)
            | HostRunnerStep::WriteNatsTlsMaterial(_)
            | HostRunnerStep::WriteNatsAuthorizedUsers(_)
            | HostRunnerStep::WriteNatsClientCredentials(_)
            | HostRunnerStep::WriteMachineJoinTemplate(_)
            | HostRunnerStep::WriteSupervisorUnit(_)
            | HostRunnerStep::StartSupervisorUnit(_)
            | HostRunnerStep::RestartSupervisorUnit(_)
            | HostRunnerStep::StoreJoinMaterial(_) => None,
        })
        .expect("first-machine plan writes the nats config");

    // TLS + authorization land in the same rendered config that opens the
    // listener — a plaintext external listener is unrepresentable.
    assert!(rendered.contains("host: 0.0.0.0\n"));
    assert!(rendered.contains("client_advertise: \"203.0.113.10:4222\"\n"));
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
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    ));

    for role in [DaemonProcessRole::Gateway, DaemonProcessRole::Dns] {
        let unit = SupervisorUnitTarget::PloyzdRole(role);
        assert!(plan_writes_unit(&plan, &unit));
        assert!(
            plan.steps()
                .contains(&HostRunnerStep::StartSupervisorUnit(unit))
        );
    }
}

#[test]
fn first_machine_plan_derives_role_supernet_after_environment_overrides() {
    let supernet = MachineEndpointSupernet::try_new("10.77.0.0/16").expect("valid supernet");
    let plan = first_machine_install_plan(
        FirstMachineInstallTarget::new(
            machine_id("machine_1"),
            ployzd_artifact(),
            dataplane_artifacts(),
            nats_server_artifact(),
            InstallRolePolicy::install_all(),
            test_identity().clone(),
            WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
            WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
        )
        .with_dataplane_endpoint_supernet(supernet)
        .with_role_environment(role_environment()),
    );

    let rendered = plan
        .steps()
        .iter()
        .find_map(|step| match step {
            HostRunnerStep::WritePloyzdRoleEnvironment(environment)
                if matches!(environment.role, DaemonProcessRole::Control) =>
            {
                Some(environment.render())
            }
            HostRunnerStep::VerifyHost(_)
            | HostRunnerStep::PrepareDataplaneHost
            | HostRunnerStep::PrepareContainerRuntime(_, _)
            | HostRunnerStep::VerifyContainerRuntime(_)
            | HostRunnerStep::InstallArtifact(_)
            | HostRunnerStep::WritePloyzdRoleEnvironment(_)
            | HostRunnerStep::WriteNatsTlsMaterial(_)
            | HostRunnerStep::WriteNatsAuthorizedUsers(_)
            | HostRunnerStep::WriteNatsClientCredentials(_)
            | HostRunnerStep::WriteNatsServerConfig(_)
            | HostRunnerStep::WriteMachineJoinTemplate(_)
            | HostRunnerStep::WriteSupervisorUnit(_)
            | HostRunnerStep::StartSupervisorUnit(_)
            | HostRunnerStep::RestartSupervisorUnit(_)
            | HostRunnerStep::StoreJoinMaterial(_) => None,
        })
        .expect("first-machine plan writes the control environment");

    assert!(rendered.contains("PLOYZ_DATAPLANE_ENDPOINT_SUPERNET=10.77.0.0/16\n"));
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
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
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
