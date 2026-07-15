use super::support;

use crate::execution::ArtifactKind;
use crate::execution::SupervisorUnitTarget;
use crate::lifecycle::{AssignedSubstrateState, SubstrateAssignment};
use crate::plan::{
    CorePromoteTarget, FirstMachineInstallTarget, HostRunnerStep, core_promote_plan,
};
use ployz_core::install::{
    AbsoluteInstallPath, MachineJoinClusterName, MachineJoinRuntimeNatsUrl, WrappedCaKey,
    WrappedCoreSeeds,
};
use ployz_core::roles::{DaemonProcessRole, InstallRolePolicy};
use ployz_test_support::ids::machine_id;
use support::artifacts::{nats_server_artifact, ployzd_artifact};
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
        InstallRolePolicy::install_all().without_gateway(),
        ployz_core::install::HostPortAssurance::Keeper,
        test_identity().clone(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    );
    CorePromoteTarget {
        machine_id: first_machine.machine_id.clone(),
        dataplane_endpoint_supernet: first_machine.dataplane_endpoint_supernet.clone(),
        assigned_substrate: AssignedSubstrateState {
            assignments: [
                SubstrateAssignment::Dataplane,
                SubstrateAssignment::NatsServer,
            ]
            .into(),
            host_ports: ployz_core::install::HostPortAssurance::Keeper,
        },
        nats_server_artifact: first_machine.nats_server_artifact.clone(),
        ployzd_artifact: first_machine.ployzd_artifact.clone(),
        dataplane_artifacts: first_machine.dataplane_artifacts.clone(),
        nats_identity: first_machine.nats_identity.clone(),
        recovery_key_wrapped: first_machine.recovery_key_wrapped.clone(),
        core_seeds_wrapped: first_machine.core_seeds_wrapped.clone(),
        nats_material: first_machine.nats_material.clone(),
        machine_public_ip: Some("203.0.113.9".parse().expect("valid ip")),
        nats_server_unit: first_machine.nats_server_unit.clone(),
        role_environment: first_machine
            .role_environment
            .with_seed_from_mirror(std::path::PathBuf::from(
                "/var/lib/ployz/nats/intent-mirror.json",
            ))
            .with_machine_join_template_file(
                AbsoluteInstallPath::try_new("/etc/ployz/machine-join-template.json")
                    .expect("valid template path"),
            ),
        machine_join_template_file: AbsoluteInstallPath::try_new(
            "/etc/ployz/machine-join-template.json",
        )
        .expect("valid template path"),
        machine_join_cluster_name: MachineJoinClusterName::try_new("ployz")
            .expect("valid cluster name"),
        machine_join_runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.9:4222")
            .expect("valid nats url"),
    }
}

#[test]
fn core_promote_plan_adds_the_core_without_reinstalling_machine_units() {
    let plan = core_promote_plan(promote_target());

    let [verify, preflight, assure, store, mutate, ..] = plan.steps() else {
        panic!("core promotion has firewall steps before mutation");
    };
    assert!(matches!(verify, HostRunnerStep::VerifyHost(_)));
    assert!(matches!(preflight, HostRunnerStep::PreflightHostPorts(_)));
    assert!(matches!(assure, HostRunnerStep::AssureHostPorts(_)));
    assert!(matches!(store, HostRunnerStep::StoreAssignedSubstrate(_)));
    assert!(matches!(mutate, HostRunnerStep::InstallArtifact(_)));

    // Installs nats-server (the machine had none as a Machine), but not ployzd/ebpf.
    assert!(installs_artifact_kind(&plan, ArtifactKind::NatsServer));
    assert!(!installs_artifact_kind(&plan, ArtifactKind::Ployzd));
    assert!(!installs_artifact_kind(&plan, ArtifactKind::EbpfBytecode));

    // Renders + starts the core NATS material and server.
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, HostRunnerStep::WriteNatsTlsMaterial(_)))
    );
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, HostRunnerStep::WriteNatsClientCredentials(_)))
    );
    assert!(plan.steps().contains(&HostRunnerStep::StartSupervisorUnit(
        SupervisorUnitTarget::NatsServer
    )));
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, HostRunnerStep::WriteMachineJoinTemplate(_)))
    );

    // Adds only the Control process — the machine's own Machine unit is untouched.
    assert!(plan.steps().contains(&HostRunnerStep::StartSupervisorUnit(
        SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Control)
    )));
    assert!(!plan.steps().contains(&HostRunnerStep::StartSupervisorUnit(
        SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Machine(machine_id("core_2")))
    )));
}

#[test]
fn core_promote_authorized_users_carries_only_the_reused_core_principals() {
    let plan = core_promote_plan(promote_target());
    let rendered = plan
        .steps()
        .iter()
        .find_map(|step| match step {
            HostRunnerStep::WriteNatsAuthorizedUsers(target) => Some(target.render()),
            HostRunnerStep::VerifyHost(_)
            | HostRunnerStep::PreflightHostPorts(_)
            | HostRunnerStep::AssureHostPorts(_)
            | HostRunnerStep::StoreAssignedSubstrate(_)
            | HostRunnerStep::PrepareDataplaneHost
            | HostRunnerStep::PrepareContainerRuntime(_, _)
            | HostRunnerStep::VerifyContainerRuntime(_)
            | HostRunnerStep::InstallArtifact(_)
            | HostRunnerStep::WritePloyzdRoleEnvironment(_)
            | HostRunnerStep::WriteNatsTlsMaterial(_)
            | HostRunnerStep::WriteNatsClientCredentials(_)
            | HostRunnerStep::WriteNatsServerConfig(_)
            | HostRunnerStep::WriteMachineJoinTemplate(_)
            | HostRunnerStep::WriteSupervisorUnit(_)
            | HostRunnerStep::StartSupervisorUnit(_)
            | HostRunnerStep::RestartSupervisorUnit(_)
            | HostRunnerStep::StoreJoinMaterial(_) => None,
        })
        .expect("promote plan writes authorized users");

    // The reused core principals only — enough for nats-server to start and control
    // to connect. The mirrored machine grants are NOT written by Host Runner; control
    // seeds the full grant set into the store from the mirror at startup (ADR 0031).
    assert!(rendered.contains(test_identity().controller.public.as_str()));
    assert!(rendered.contains(test_identity().operator.public.as_str()));
    assert!(rendered.contains(test_identity().join.public.as_str()));
    assert!(!rendered.contains(MIRRORED_MACHINE_PUBLIC));
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
    assert!(
        control_env
            .contains("PLOYZ_MACHINE_JOIN_TEMPLATE_FILE=/etc/ployz/machine-join-template.json\n")
    );
}
