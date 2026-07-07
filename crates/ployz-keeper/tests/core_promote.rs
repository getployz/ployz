mod support;

use ployz_core::install::{WrappedCaKey, WrappedCoreSeeds};
use ployz_core::nats_config::NatsUserPublicKey;
use ployz_core::roles::{DaemonProcessRole, InstallRolePolicy};
use ployz_keeper::artifacts::ArtifactKind;
use ployz_keeper::steps::{
    CorePromoteTarget, FirstMachineInstallTarget, KeeperStep, core_promote_plan,
};
use ployz_keeper::systemd::SupervisorUnitTarget;
use ployz_test_support::ids::machine_id;
use ployz_test_support::keeper::{nats_server_artifact, ployzd_artifact};
use support::bootstrap::*;

const MIRRORED_MACHINE_PUBLIC: &str = "UBCXCMGAZQZN55X5TTTWMB5CZNZIKJHEDZJOJ3TV63NKPJ6FRXSR2ZO4";

fn promote_target() -> CorePromoteTarget {
    // Reuse the sub-targets first-machine builds (role env, nats-server unit,
    // material) — a promotion produces the same core material, just from an
    // already-joined machine.
    let first_machine = FirstMachineInstallTarget::new(
        machine_id("core_2"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
        test_identity().clone(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    );
    CorePromoteTarget {
        machine_id: first_machine.machine_id.clone(),
        nats_server_artifact: first_machine.nats_server_artifact.clone(),
        ployzd_artifact: first_machine.ployzd_artifact.clone(),
        nats_identity: first_machine.nats_identity.clone(),
        recovery_key_wrapped: first_machine.recovery_key_wrapped.clone(),
        machine_authorized_publics: vec![(
            machine_id("machine_9"),
            NatsUserPublicKey::try_new(MIRRORED_MACHINE_PUBLIC).expect("valid public"),
        )],
        nats_material: first_machine.nats_material.clone(),
        machine_public_ip: Some("203.0.113.9".parse().expect("valid ip")),
        nats_server_unit: first_machine.nats_server_unit.clone(),
        role_environment: first_machine.role_environment.with_seed_from_mirror(
            std::path::PathBuf::from("/var/lib/ployz/nats/intent-mirror.json"),
        ),
    }
}

#[test]
fn core_promote_plan_adds_the_core_without_reinstalling_machine_units() {
    let plan = core_promote_plan(promote_target());

    // Installs nats-server (the machine had none as a Machine), but not ployzd/ebpf.
    assert!(installs_artifact_kind(&plan, ArtifactKind::NatsServer));
    assert!(!installs_artifact_kind(&plan, ArtifactKind::Ployzd));
    assert!(!installs_artifact_kind(&plan, ArtifactKind::EbpfBytecode));

    // Renders + starts the core NATS material and server.
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, KeeperStep::WriteNatsTlsMaterial(_)))
    );
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, KeeperStep::WriteNatsClientCredentials(_)))
    );
    assert!(plan.steps().contains(&KeeperStep::StartSupervisorUnit(
        SupervisorUnitTarget::NatsServer
    )));

    // Adds only the Control process — the machine's own Machine unit is untouched.
    assert!(plan.steps().contains(&KeeperStep::StartSupervisorUnit(
        SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Control)
    )));
    assert!(!plan.steps().contains(&KeeperStep::StartSupervisorUnit(
        SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Machine(machine_id("core_2")))
    )));
}

#[test]
fn core_promote_authorized_users_carries_the_mirrored_machine_publics() {
    let plan = core_promote_plan(promote_target());
    let rendered = plan
        .steps()
        .iter()
        .find_map(|step| match step {
            KeeperStep::WriteNatsAuthorizedUsers(target) => Some(target.render()),
            KeeperStep::VerifyHost(_)
            | KeeperStep::PrepareDataplaneHost
            | KeeperStep::PrepareContainerRuntime(_)
            | KeeperStep::VerifyContainerRuntime(_)
            | KeeperStep::InstallArtifact(_)
            | KeeperStep::WritePloyzdRoleEnvironment(_)
            | KeeperStep::WriteNatsTlsMaterial(_)
            | KeeperStep::WriteNatsClientCredentials(_)
            | KeeperStep::WriteNatsServerConfig(_)
            | KeeperStep::WriteMachineJoinTemplate(_)
            | KeeperStep::WriteSupervisorUnit(_)
            | KeeperStep::StartSupervisorUnit(_)
            | KeeperStep::RestartSupervisorUnit(_)
            | KeeperStep::StoreJoinMaterial(_) => None,
        })
        .expect("promote plan writes authorized users");

    // The new core's freshly-minted principals plus the mirrored machine's public.
    assert!(rendered.contains(test_identity().controller.public.as_str()));
    assert!(rendered.contains(test_identity().operator.public.as_str()));
    assert!(rendered.contains(MIRRORED_MACHINE_PUBLIC));
}

#[test]
fn core_promote_control_env_points_at_the_seed_mirror() {
    let target = promote_target();
    let control_env = target
        .role_environment
        .render_for_role(&DaemonProcessRole::Control);
    assert!(
        control_env.contains("PLOYZ_SEED_FROM_MIRROR=/var/lib/ployz/nats/intent-mirror.json\n")
    );
}
