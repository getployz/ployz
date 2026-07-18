use super::support;

use ployz_core::install::{HostPortAssurance, WrappedCaKey, WrappedCoreSeeds};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::execution::{ArtifactKind, ArtifactSource, ArtifactTarget, DataplaneArtifactTargets};
use crate::execution::{HostRunnerCommandOutput, HostRunnerCommandRunner};
use crate::execution::{HostRunnerLocalConfig, HostRunnerLocalEffects};
use crate::execution::{NatsServerUnitTarget, PloyzdRoleEnvironmentFile, SupervisorUnitTarget};
use crate::lifecycle::machine_join::execution::{
    HostRunnerJoinRedeemer, HostRunnerJoinReporter, HostRunnerJoinTokenConsumer,
    RedeemedHostRunnerJoin, execute_host_runner_join,
};
use crate::lifecycle::{
    AssignedSubstrateState, SubstrateAssignment, load_assigned_substrate_state,
};
use crate::lifecycle::{
    JOIN_MATERIAL_DIR, JOIN_MATERIAL_FILE, JOIN_NATS_CREDENTIALS_FILE, JOIN_RECOVERY_KEY_FILE,
    JOIN_TRUSTED_CA_FILE,
};
use crate::plan::{
    ContainerRuntime, FirstMachineInstallTarget, HostPrerequisite, HostRunnerJoinMaterial,
    HostRunnerJoinTarget, HostRunnerStep, HostRunnerStepFailure, HostRunnerStepFailureReason,
    HostRunnerStepLabel, JoinToken, NonEmptyRoleSet, PloyzdRoleEnvironmentTarget,
    RoleNatsCredentials, first_machine_install_plan,
};
use crate::plan::{
    HostRunnerPlanFailure, HostRunnerPlanTerminal, HostRunnerStepEffects, HostRunnerStepEvent,
    HostRunnerStepRecorder, execute_host_runner_plan,
};
use crate::recovery::{ClusterNatsIdentity, ServerCertificateSans, generate_cluster_nats_identity};
use ployz_core::ids::MachineId;
use ployz_core::install::NatsMachineMaterialPaths;
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::operation::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, InstallRolePolicy};
use ployz_nats::connect::NatsClientUrl;
use ployz_sdk_types::MachineJoinReportFailure;
use ployz_test_support::ids::{failure_message, machine_id, operation_id};
use std::sync::OnceLock;
use support::artifacts::{artifact_version as version, sha256_digest as digest};

#[test]
fn externally_assured_host_ports_skip_firewall_and_store_assignment() {
    let root = temp_dir("ployz-host-runner-external-host-ports");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let state = AssignedSubstrateState {
        assignments: [SubstrateAssignment::Dataplane].into(),
        host_ports: HostPortAssurance::External,
    };
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );

    effects
        .apply_step(&HostRunnerStep::PreflightHostPorts(state.clone()))
        .expect("external assurance passes preflight");
    effects
        .apply_step(&HostRunnerStep::AssureHostPorts(state.clone()))
        .expect("external assurance skips apply");
    effects
        .apply_step(&HostRunnerStep::StoreAssignedSubstrate(state.clone()))
        .expect("assignment stores");

    assert_eq!(
        load_assigned_substrate_state(&root.join("state")).expect("assignment loads"),
        state
    );
}

#[test]
fn managed_host_ports_query_before_open() {
    let root = temp_dir("ployz-host-runner-managed-host-ports");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let mut runner = RecordingRunner::root_linux();
    runner.command_outputs = [
        failed_command(),
        succeeded_command(""),
        succeeded_command("Status: active\n"),
        succeeded_command("Status: active\n"),
        succeeded_command(""),
        succeeded_command("Status: active\n\n53/udp                     ALLOW IN    Anywhere\n"),
        succeeded_command("Status: active\n"),
        succeeded_command(""),
        succeeded_command("Status: active\n\n51820/udp                  ALLOW IN    Anywhere\n"),
    ]
    .into();
    let mut effects = HostRunnerLocalEffects::new(local_config(&root, &systemd_dir), runner);
    validate_host(&mut effects);

    effects
        .apply_step(&HostRunnerStep::AssureHostPorts(AssignedSubstrateState {
            assignments: [SubstrateAssignment::Dataplane].into(),
            host_ports: HostPortAssurance::Keeper,
        }))
        .expect("managed port opens");

    assert_eq!(
        effects.runner().command_calls,
        vec![
            "systemctl is-active --quiet firewalld.service",
            "systemctl is-active --quiet ufw.service",
            "ufw status",
            "ufw status verbose",
            "ufw insert 1 allow 53/udp",
            "ufw status verbose",
            "ufw status verbose",
            "ufw insert 1 allow 51820/udp",
            "ufw status verbose",
        ]
    );
}

fn succeeded_command(stdout: &str) -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: true,
        exit_code: Some(0),
        stdout: stdout.to_owned(),
        stdout_truncated: false,
        failure: String::new(),
    }
}

fn failed_command() -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: false,
        exit_code: Some(3),
        stdout: String::new(),
        stdout_truncated: false,
        failure: "simulated command failure".to_owned(),
    }
}

fn absent_command() -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: false,
        exit_code: Some(1),
        stdout: String::new(),
        stdout_truncated: false,
        failure: "not found".to_owned(),
    }
}

#[test]
fn local_effects_install_first_machine_process_units() {
    let root = temp_dir("ployz-host-runner-local-first-machine");
    let source = root.join("ployzd-source");
    let install_path = root.join("bin/ployzd");
    let nats_source = root.join("nats-server-source");
    let nats_install_path = root.join("bin/nats-server");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");

    let ployzd_artifact = ployzd_artifact(&source, &install_path);
    let plan = first_machine_install_plan(
        FirstMachineInstallTarget::new(
            machine_id("machine_1"),
            ployzd_artifact,
            dataplane_artifacts(&root),
            railpack_artifact(&root),
            nats_server_artifact(&nats_source, &nats_install_path),
            InstallRolePolicy::install_all().without_gateway(),
            HostPortAssurance::Keeper,
            test_identity().clone(),
            WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
            WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
        )
        .with_nats_server_unit(nats_unit(&root))
        .with_nats_material_paths(nats_material(&root))
        .with_role_environment(role_env(&root)),
    );
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let [verify, preflight, assure, store, ..] = plan.steps() else {
        panic!("first-machine install has firewall steps before mutation");
    };
    assert!(matches!(verify, HostRunnerStep::VerifyHost(_)));
    assert!(matches!(preflight, HostRunnerStep::PreflightHostPorts(_)));
    assert!(matches!(assure, HostRunnerStep::AssureHostPorts(_)));
    assert!(matches!(store, HostRunnerStep::StoreAssignedSubstrate(_)));

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert_eq!(fs::read_to_string(&install_path).unwrap(), "ployz\n");
    assert_eq!(fs::read_to_string(&nats_install_path).unwrap(), "ployz\n");
    assert_eq!(
        fs::read_to_string(root.join("etc/nats-server.conf")).unwrap(),
        expected_loopback_nats_config(&root)
    );
    assert_eq!(
        fs::read_to_string(root.join("nats/ca.pem")).unwrap(),
        test_identity().ca.as_str()
    );
    assert_eq!(
        fs::read_to_string(root.join("nats/server.crt")).unwrap(),
        test_identity().server_cert.cert_pem.as_str()
    );
    assert_eq!(
        fs::read_to_string(root.join("nats/server.key")).unwrap(),
        test_identity().server_cert.key_pem.secret()
    );
    assert_secret_file_mode(root.join("nats/server.key"));
    assert_eq!(
        fs::read(root.join("nats/ca-recovery.key")).unwrap(),
        b"wrapped-ca-key"
    );
    assert_secret_file_mode(root.join("nats/ca-recovery.key"));
    let authorized_users = fs::read_to_string(root.join("etc/authorized-users.conf")).unwrap();
    assert!(authorized_users.starts_with("authorization {\n  users [\n"));
    assert!(authorized_users.contains(test_identity().controller.public.as_str()));
    assert!(authorized_users.contains(test_identity().operator.public.as_str()));
    assert!(authorized_users.contains(test_identity().join.public.as_str()));
    for (seed_file, seed) in [
        ("nats/controller.seed", &test_identity().controller.seed),
        ("nats/operator.seed", &test_identity().operator.seed),
        ("nats/join.seed", &test_identity().join.seed),
    ] {
        assert_eq!(
            fs::read_to_string(root.join(seed_file)).unwrap(),
            seed.secret()
        );
        assert_secret_file_mode(root.join(seed_file));
    }
    assert!(!root.join("nats/machine.seed").exists());
    assert!(
        fs::read_to_string(systemd_dir.join("nats-server.service"))
            .unwrap()
            .contains("--config")
    );
    assert!(
        fs::read_to_string(systemd_dir.join("ployzd-control.service"))
            .unwrap()
            .contains("control")
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-control.env")).unwrap(),
        format!(
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_MACHINE_ID=machine_1\nPLOYZ_JOIN_NKEY_SEED_FILE={join_seed}\nPLOYZ_DATAPLANE_ENDPOINT_SUPERNET=10.198.0.0/16\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
            ca = root.join("nats/ca.pem").display(),
            seed = root.join("nats/controller.seed").display(),
            join_seed = root.join("nats/join.seed").display(),
            bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
            ctl = root.join("bin/ployz-ebpf-ctl").display()
        )
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-machine.env")).unwrap(),
        format!(
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_MACHINE_ID=machine_1\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
            ca = root.join("nats/ca.pem").display(),
            seed = root.join("nats/machine.seed").display(),
            bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
            ctl = root.join("bin/ployz-ebpf-ctl").display()
        )
    );
    assert!(
        systemd_dir
            .join("ployzd-machine-machine_1.service")
            .exists()
    );
    assert!(!systemd_dir.join("ployzd-gateway.service").exists());
}

#[test]
fn first_machine_install_writes_machine_bootstrap_url_when_configured() {
    let root = temp_dir("ployz-host-runner-first-machine-bootstrap-url");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir exists");
    let ployzd_source = root.join("ployzd-source");
    let nats_source = root.join("nats-server-source");
    fs::write(&ployzd_source, "ployz\n").expect("artifact source can be written");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");
    let runner = RecordingRunner::root_linux();
    let target = FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(&ployzd_source, &root.join("bin/ployzd")),
        dataplane_artifacts(&root),
        railpack_artifact(&root),
        nats_server_artifact(&nats_source, &root.join("bin/nats-server")),
        InstallRolePolicy::install_all().without_gateway(),
        HostPortAssurance::Keeper,
        test_identity().clone(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    )
    .with_nats_server_unit(nats_unit(&root))
    .with_nats_material_paths(nats_material(&root))
    .with_role_environment(
        role_env(&root).with_machine_bootstrap_url(
            ployz_core::install::MachineBootstrapUrl::try_new("https://example.test/ployz.sh")
                .expect("valid bootstrap url"),
        ),
    );
    let plan = first_machine_install_plan(target);
    let mut effects = HostRunnerLocalEffects::new(local_config(&root, &systemd_dir), runner);

    let execution =
        execute_host_runner_plan(&plan, &mut effects, &mut RecordingRecorder::default());

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-control.env")).unwrap(),
        format!(
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_MACHINE_ID=machine_1\nPLOYZ_JOIN_NKEY_SEED_FILE={join_seed}\nPLOYZ_DATAPLANE_ENDPOINT_SUPERNET=10.198.0.0/16\nPLOYZ_MACHINE_BOOTSTRAP_URL=https://example.test/ployz.sh\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
            ca = root.join("nats/ca.pem").display(),
            seed = root.join("nats/controller.seed").display(),
            join_seed = root.join("nats/join.seed").display(),
            bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
            ctl = root.join("bin/ployz-ebpf-ctl").display()
        )
    );
}

#[test]
fn first_machine_install_writes_machine_join_template_file_when_configured() {
    let root = temp_dir("ployz-host-runner-first-machine-join-template");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir exists");
    let ployzd_source = root.join("ployzd-source");
    let nats_source = root.join("nats-server-source");
    fs::write(&ployzd_source, "ployz\n").expect("artifact source can be written");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");
    let runner = RecordingRunner::root_linux();
    let template_path = root.join("etc/machine-join-template.json");
    let target = FirstMachineInstallTarget::new(
        machine_id("machine_1"),
        ployzd_artifact(&ployzd_source, &root.join("bin/ployzd")),
        dataplane_artifacts(&root),
        railpack_artifact(&root),
        nats_server_artifact(&nats_source, &root.join("bin/nats-server")),
        InstallRolePolicy::install_all().without_gateway(),
        HostPortAssurance::Keeper,
        test_identity().clone(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    )
    .with_nats_server_unit(nats_unit(&root))
    .with_nats_material_paths(nats_material(&root))
    .with_role_environment(role_env(&root))
    .with_machine_join_template_file(
        ployz_core::install::AbsoluteInstallPath::try_new(template_path.display().to_string())
            .expect("valid template path"),
    );
    let plan = first_machine_install_plan(target);
    let mut effects = HostRunnerLocalEffects::new(local_config(&root, &systemd_dir), runner);

    let execution =
        execute_host_runner_plan(&plan, &mut effects, &mut RecordingRecorder::default());

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-control.env")).unwrap(),
        format!(
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_MACHINE_ID=machine_1\nPLOYZ_JOIN_NKEY_SEED_FILE={join_seed}\nPLOYZ_DATAPLANE_ENDPOINT_SUPERNET=10.198.0.0/16\nPLOYZ_MACHINE_JOIN_TEMPLATE_FILE={template}\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
            ca = root.join("nats/ca.pem").display(),
            seed = root.join("nats/controller.seed").display(),
            join_seed = root.join("nats/join.seed").display(),
            template = template_path.display(),
            bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
            ctl = root.join("bin/ployz-ebpf-ctl").display()
        )
    );
    let template: ployz_core::install::MachineJoinTemplate =
        serde_json::from_str(&fs::read_to_string(&template_path).expect("join template writes"))
            .expect("join template parses");
    assert_eq!(template.join_bundle.material.cluster_name.as_str(), "ployz");
    assert_eq!(
        template.join_bundle.material.runtime_nats_url.as_str(),
        "tls://127.0.0.1:4222"
    );
}

#[test]
fn local_effects_fail_before_work_when_host_is_not_root() {
    let root = temp_dir("ployz-host-runner-local-not-root");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let plan = first_machine_plan_with_ployzd(
        &root,
        ployzd_artifact(&root.join("source"), &root.join("bin/ployzd")),
    );
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            uid: 501,
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::VerifyHost(_),
            reason: HostRunnerStepFailureReason::HostPrerequisiteFailed,
            message,
        })) if message.as_str() == "Host Runner must run as root"
    ));
    assert_eq!(effects.runner().systemctl_calls, Vec::<Vec<String>>::new());
}

#[test]
fn unsupported_hosts_fail_before_any_host_mutation() {
    let root = temp_dir("ployz-host-runner-local-unsupported-host");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let plan = first_machine_plan_with_ployzd(
        &root,
        ployzd_artifact(&root.join("source"), &root.join("bin/ployzd")),
    );
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            os_release: "ID=nixos\nVERSION_ID=\"24.11\"\n".to_owned(),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::VerifyHost(_),
            reason: HostRunnerStepFailureReason::HostPrerequisiteFailed,
            message,
        })) if message.as_str().contains("host distribution nixos")
    ));
    assert_eq!(
        fs::read_dir(&systemd_dir)
            .expect("systemd directory remains readable")
            .count(),
        0
    );
    assert!(!root.join("state").exists());
    assert!(!root.join("etc").exists());
    assert!(effects.runner().command_calls.is_empty());
}

#[test]
fn local_effects_prepare_dataplane_host_before_docker() {
    let root = temp_dir("ployz-host-runner-local-dataplane-host");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan =
        first_machine_plan_with_ployzd(&root, ployzd_artifact(&source, &root.join("bin/ployzd")));
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert_eq!(effects.runner().dataplane_host_prepare_runs, 1);
}

#[test]
fn local_effects_report_dataplane_host_prepare_failure() {
    let root = temp_dir("ployz-host-runner-local-dataplane-host-fail");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan =
        first_machine_plan_with_ployzd(&root, ployzd_artifact(&source, &root.join("bin/ployzd")));
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            fail_dataplane_host_prepare: true,
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::PrepareDataplaneHost,
            reason: HostRunnerStepFailureReason::DataplaneHostPrepareFailed,
            message,
        })) if message.as_str() == "simulated dataplane host prepare failure"
    ));
    assert_eq!(effects.runner().docker_install_runs, 0);
}

#[test]
fn local_effects_prepare_dataplane_packages_for_each_supported_family() {
    let cases = [
        (
            "ubuntu",
            "env DEBIAN_FRONTEND=noninteractive apt-get install -y wireguard-tools iproute2 iputils-ping",
        ),
        ("rocky", "dnf install -y wireguard-tools iproute iputils"),
        (
            "fedora",
            "dnf install -y wireguard-tools iproute iproute-tc iputils",
        ),
        (
            "arch",
            "pacman -S --noconfirm --needed wireguard-tools iproute2 iputils",
        ),
        ("alpine", "apk add wireguard-tools iproute2 iputils"),
        (
            "opensuse-leap",
            "zypper --non-interactive install wireguard-tools iproute2 iputils",
        ),
    ];

    for (id, expected_install) in cases {
        let root = temp_dir(&format!("ployz-host-runner-dataplane-{id}"));
        let systemd_dir = root.join("systemd");
        fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
        fs::create_dir_all(root.join("openrc")).expect("OpenRC dir can be created");
        let runner = RecordingRunner {
            os_release: format!("ID={id}\nVERSION_ID=1\n"),
            ..RecordingRunner::root_linux()
        };
        let mut effects = HostRunnerLocalEffects::new(local_config(&root, &systemd_dir), runner);
        validate_host(&mut effects);

        effects
            .apply_step(&HostRunnerStep::PrepareDataplaneHost)
            .expect("dataplane packages install");

        assert!(
            effects
                .runner()
                .command_calls
                .contains(&expected_install.to_owned()),
            "{id} did not use {expected_install}: {:?}",
            effects.runner().command_calls,
        );
    }
}

#[test]
fn arch_package_installs_use_existing_pacman_sync_databases() {
    let root = temp_dir("ployz-host-runner-arch-docker");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let runner = RecordingRunner {
        os_release: "ID=arch\n".to_owned(),
        docker_installed: false,
        docker_running: false,
        ..RecordingRunner::root_linux()
    };
    let mut effects = HostRunnerLocalEffects::new(local_config(&root, &systemd_dir), runner);
    validate_host(&mut effects);

    effects
        .apply_step(&HostRunnerStep::PrepareDataplaneHost)
        .expect("Arch installs dataplane packages from existing pacman sync databases");
    effects
        .apply_step(&HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            ployz_core::network::MachineEndpointSupernet::default_v1(),
        ))
        .expect("Arch installs Docker from existing pacman sync databases");

    assert_eq!(
        effects
            .runner()
            .command_calls
            .iter()
            .filter(|call| call.starts_with("pacman "))
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "pacman -S --noconfirm --needed wireguard-tools iproute2 iputils",
            "pacman -S --noconfirm --needed docker docker-compose",
        ]
    );
}

#[test]
fn amazon_linux_2_falls_back_to_yum_for_dataplane_and_docker_packages() {
    let root = temp_dir("ployz-host-runner-amazon-linux-2");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let runner = RecordingRunner {
        os_release: "ID=amzn\nVERSION_ID=2\n".to_owned(),
        dnf_available: false,
        docker_installed: false,
        docker_running: false,
        ..RecordingRunner::root_linux()
    };
    let mut effects = HostRunnerLocalEffects::new(local_config(&root, &systemd_dir), runner);
    validate_host(&mut effects);

    effects
        .apply_step(&HostRunnerStep::PrepareDataplaneHost)
        .expect("Amazon Linux 2 installs dataplane packages with yum");
    effects
        .apply_step(&HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            ployz_core::network::MachineEndpointSupernet::default_v1(),
        ))
        .expect("Amazon Linux 2 installs Docker with yum");

    for command in [
        "yum install -y wireguard-tools iproute iputils",
        "yum install -y docker",
    ] {
        assert!(
            effects.runner().command_calls.contains(&command.to_owned()),
            "missing {command}: {:?}",
            effects.runner().command_calls
        );
    }
}

#[test]
fn local_effects_skip_docker_install_when_runtime_is_ready() {
    let root = temp_dir("ployz-host-runner-local-docker-ready");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan =
        first_machine_plan_with_ployzd(&root, ployzd_artifact(&source, &root.join("bin/ployzd")));
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert_eq!(effects.runner().docker_install_runs, 0);
    assert!(
        !effects
            .runner()
            .systemctl_calls
            .contains(&docker_enable_call())
    );
}

#[test]
fn local_effects_start_docker_service_when_daemon_is_stopped() {
    let root = temp_dir("ployz-host-runner-local-docker-stopped");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan =
        first_machine_plan_with_ployzd(&root, ployzd_artifact(&source, &root.join("bin/ployzd")));
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            docker_running: false,
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert_eq!(effects.runner().docker_install_runs, 0);
    assert!(
        effects
            .runner()
            .systemctl_calls
            .contains(&docker_enable_call())
    );
}

#[test]
fn local_effects_reject_stopped_classic_store_before_writing_daemon_config() {
    let root = temp_dir("ployz-host-runner-local-docker-stopped-classic");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let daemon_config = root.join("etc/docker/daemon.json");
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            docker_running: false,
            containerd_snapshotter: false,
            ..RecordingRunner::root_linux()
        },
    );
    validate_host(&mut effects);

    let result = effects.apply_step(&HostRunnerStep::PrepareContainerRuntime(
        ContainerRuntime::Docker,
        ployz_core::network::MachineEndpointSupernet::default_v1(),
    ));

    assert!(matches!(
        result,
        Err(error)
            if error.reason()
                == Some(HostRunnerStepFailureReason::ContainerRuntimeClassicStoreUnsupported)
    ));
    assert!(!daemon_config.exists());
    assert_eq!(effects.runner().docker_install_runs, 0);
}

#[test]
fn local_effects_merge_required_docker_daemon_settings() {
    let root = temp_dir("ployz-host-runner-local-docker-config");
    let systemd_dir = root.join("systemd");
    let daemon_config = root.join("etc/docker/daemon.json");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::create_dir_all(daemon_config.parent().expect("config has parent"))
        .expect("docker config dir can be created");
    fs::write(
        &daemon_config,
        br#"{"log-level":"warn","features":{"cdi":true},"insecure-registries":["192.0.2.0/24"]}"#,
    )
    .expect("docker config can be written");
    let source = root.join("source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan =
        first_machine_plan_with_ployzd(&root, ployzd_artifact(&source, &root.join("bin/ployzd")));
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(daemon_config).expect("docker config can be read"))
            .expect("docker config is JSON");
    assert_eq!(config.get("log-level"), Some(&serde_json::json!("warn")));
    assert_eq!(
        config.get("log-driver"),
        Some(&serde_json::json!("json-file"))
    );
    assert_eq!(
        config.get("log-opts"),
        Some(&serde_json::json!({"max-size":"10m","max-file":"3"}))
    );
    let features = config
        .get("features")
        .and_then(serde_json::Value::as_object)
        .expect("Docker features are an object");
    assert_eq!(features.get("cdi"), Some(&serde_json::json!(true)));
    assert_eq!(
        features.get("containerd-snapshotter"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        config.get("insecure-registries"),
        Some(&serde_json::json!(["192.0.2.0/24", "10.198.0.0/16"]))
    );
    assert!(
        effects
            .runner()
            .systemctl_calls
            .contains(&vec!["restart".to_owned(), "docker".to_owned(),])
    );
}

#[test]
fn local_effects_preserve_existing_docker_logging_driver_as_one_opinion() {
    let root = temp_dir("ployz-host-runner-local-docker-log-driver");
    let systemd_dir = root.join("systemd");
    let daemon_config = root.join("etc/docker/daemon.json");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::create_dir_all(daemon_config.parent().expect("config has parent"))
        .expect("docker config dir can be created");
    fs::write(
        &daemon_config,
        br#"{"features":{"containerd-snapshotter":true},"insecure-registries":["10.77.0.0/16"],"log-driver":"journald"}"#,
    )
    .expect("docker config can be written");
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    validate_host(&mut effects);

    effects
        .apply_step(&HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            ployz_core::network::MachineEndpointSupernet::try_new("10.77.0.0/16")
                .expect("valid custom supernet"),
        ))
        .expect("existing logging opinion is accepted");

    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(&daemon_config).expect("Docker config can be read"))
            .expect("Docker config is JSON");
    assert_eq!(
        config.get("log-driver"),
        Some(&serde_json::json!("journald"))
    );
    assert!(config.get("log-opts").is_none());
    assert!(
        !effects
            .runner()
            .systemctl_calls
            .contains(&vec!["restart".to_owned(), "docker".to_owned(),])
    );
}

#[test]
fn local_effects_preserve_existing_docker_logging_options_as_one_opinion() {
    let root = temp_dir("ployz-host-runner-local-docker-log-opts");
    let systemd_dir = root.join("systemd");
    let daemon_config = root.join("etc/docker/daemon.json");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::create_dir_all(daemon_config.parent().expect("config has parent"))
        .expect("docker config dir can be created");
    fs::write(
        &daemon_config,
        br#"{"features":{"containerd-snapshotter":true},"insecure-registries":["10.77.0.0/16"],"log-opts":{"tag":"ployz"}}"#,
    )
    .expect("docker config can be written");
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    validate_host(&mut effects);

    effects
        .apply_step(&HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            ployz_core::network::MachineEndpointSupernet::try_new("10.77.0.0/16")
                .expect("valid custom supernet"),
        ))
        .expect("existing logging opinion is accepted");

    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(&daemon_config).expect("Docker config can be read"))
            .expect("Docker config is JSON");
    assert!(config.get("log-driver").is_none());
    assert_eq!(
        config.get("log-opts"),
        Some(&serde_json::json!({"tag":"ployz"}))
    );
    assert!(
        !effects
            .runner()
            .systemctl_calls
            .contains(&vec!["restart".to_owned(), "docker".to_owned(),])
    );
}

#[test]
fn local_effects_use_configured_supernet_without_restarting_unchanged_docker() {
    let root = temp_dir("ployz-host-runner-local-docker-custom-supernet");
    let systemd_dir = root.join("systemd");
    let daemon_config = root.join("etc/docker/daemon.json");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::create_dir_all(daemon_config.parent().expect("config has parent"))
        .expect("docker config dir can be created");
    fs::write(
        &daemon_config,
        br#"{"features":{"containerd-snapshotter":true},"insecure-registries":["10.77.0.0/16"],"log-driver":"json-file","log-opts":{"max-size":"10m","max-file":"3"}}"#,
    )
    .expect("docker config can be written");
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    validate_host(&mut effects);

    effects
        .apply_step(&HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            ployz_core::network::MachineEndpointSupernet::try_new("10.77.0.0/16")
                .expect("valid custom supernet"),
        ))
        .expect("unchanged Docker config is accepted");

    assert!(
        !effects
            .runner()
            .systemctl_calls
            .contains(&vec!["restart".to_owned(), "docker".to_owned(),])
    );
}

#[test]
fn local_effects_reject_running_docker_classic_store() {
    let root = temp_dir("ployz-host-runner-local-docker-classic-store");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan =
        first_machine_plan_with_ployzd(&root, ployzd_artifact(&source, &root.join("bin/ployzd")));
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            containerd_snapshotter: false,
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            reason: HostRunnerStepFailureReason::ContainerRuntimeClassicStoreUnsupported,
            message,
            ..
        })) if message.as_str().contains("classic image store")
    ));
}

#[test]
fn local_effects_install_docker_when_runtime_is_missing() {
    let root = temp_dir("ployz-host-runner-local-docker-missing");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan =
        first_machine_plan_with_ployzd(&root, ployzd_artifact(&source, &root.join("bin/ployzd")));
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            docker_installed: false,
            docker_running: false,
            download_body: Some(b"docker install script\n".to_vec()),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert_eq!(effects.runner().docker_install_runs, 1);
    assert!(
        effects
            .runner()
            .systemctl_calls
            .contains(&docker_enable_call())
    );
    let docker_downloads = effects
        .runner()
        .downloads
        .iter()
        .filter(|download| download.url == "https://get.docker.com")
        .collect::<Vec<_>>();
    assert_eq!(docker_downloads.len(), 1);
    assert!(
        docker_downloads
            .iter()
            .all(|download| download.is_cleaned_up())
    );
}

#[test]
fn local_effects_install_docker_from_native_packages_on_opensuse() {
    let root = temp_dir("ployz-host-runner-local-docker-opensuse");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let runner = RecordingRunner {
        docker_installed: false,
        docker_running: false,
        os_release: "ID=opensuse-leap\nVERSION_ID=16.0\n".to_owned(),
        ..RecordingRunner::root_linux()
    };
    let mut effects = HostRunnerLocalEffects::new(local_config(&root, &systemd_dir), runner);
    validate_host(&mut effects);

    effects
        .apply_step(&HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            ployz_core::network::MachineEndpointSupernet::default_v1(),
        ))
        .expect("openSUSE installs Docker from native packages");

    assert_eq!(effects.runner().docker_install_runs, 1);
    assert!(effects.runner().downloads.is_empty());
    for command in ["zypper refresh", "zypper --non-interactive install docker"] {
        assert!(
            effects.runner().command_calls.contains(&command.to_owned()),
            "missing {command}: {:?}",
            effects.runner().command_calls
        );
    }
    let config: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("etc/docker/daemon.json")).expect("Docker config can be read"),
    )
    .expect("Docker config is JSON");
    assert_eq!(
        config.pointer("/features/containerd-snapshotter"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        config.get("insecure-registries"),
        Some(&serde_json::json!(["10.198.0.0/16"]))
    );
    assert!(
        effects
            .runner()
            .systemctl_calls
            .contains(&docker_enable_call())
    );
}

#[test]
fn local_effects_install_docker_from_rhel_repository_on_rocky() {
    let root = temp_dir("ployz-host-runner-local-docker-rocky");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let runner = RecordingRunner {
        docker_installed: false,
        docker_running: false,
        os_release: "ID=rocky\nID_LIKE=\"rhel centos fedora\"\nVERSION_ID=\"10.0\"\n".to_owned(),
        download_body: Some(b"[docker-ce-stable]\nname=Docker CE Stable\n".to_vec()),
        ..RecordingRunner::root_linux()
    };
    let mut effects = HostRunnerLocalEffects::new(local_config(&root, &systemd_dir), runner);
    validate_host(&mut effects);

    effects
        .apply_step(&HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            ployz_core::network::MachineEndpointSupernet::default_v1(),
        ))
        .expect("Rocky installs Docker from the supported RHEL repository");

    assert_eq!(effects.runner().docker_install_runs, 1);
    assert_eq!(effects.runner().downloads.len(), 1);
    assert_eq!(
        fs::read_to_string(root.join("etc/yum.repos.d/docker-ce.repo"))
            .expect("Docker repository is installed"),
        "[docker-ce-stable]\nname=Docker CE Stable\n"
    );
    assert!(
        effects
            .runner()
            .command_calls
            .iter()
            .all(|command| !command.contains("config-manager"))
    );
    let config: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("etc/docker/daemon.json")).expect("Docker config can be read"),
    )
    .expect("Docker config is JSON");
    assert_eq!(
        config.pointer("/features/containerd-snapshotter"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        config.get("insecure-registries"),
        Some(&serde_json::json!(["10.198.0.0/16"]))
    );
    assert!(
        effects
            .runner()
            .systemctl_calls
            .contains(&docker_enable_call())
    );
}

#[test]
fn local_effects_install_docker_with_openrc_on_alpine() {
    let root = temp_dir("ployz-host-runner-local-docker-alpine");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::create_dir_all(root.join("openrc")).expect("OpenRC dir can be created");
    let runner = RecordingRunner {
        docker_installed: false,
        docker_running: false,
        os_release: "ID=alpine\nVERSION_ID=\"3.22\"\n".to_owned(),
        ..RecordingRunner::root_linux()
    };
    let mut effects = HostRunnerLocalEffects::new(local_config(&root, &systemd_dir), runner);
    validate_host(&mut effects);

    effects
        .apply_step(&HostRunnerStep::PrepareContainerRuntime(
            ContainerRuntime::Docker,
            ployz_core::network::MachineEndpointSupernet::default_v1(),
        ))
        .expect("Alpine installs Docker and starts it with OpenRC");

    assert_eq!(effects.runner().docker_install_runs, 1);
    assert!(effects.runner().downloads.is_empty());
    assert!(root.join("etc/docker/daemon.json").exists());
    assert!(
        effects
            .runner()
            .command_calls
            .contains(&"apk add docker docker-cli-compose".to_owned())
    );
    assert!(
        effects
            .runner()
            .command_calls
            .contains(&"rc-update add docker default".to_owned())
    );
    assert!(
        effects
            .runner()
            .command_calls
            .contains(&"rc-service docker start".to_owned())
    );
}

#[test]
fn first_machine_install_uses_openrc_for_every_managed_service_on_alpine() {
    let root = temp_dir("ployz-host-runner-local-alpine-first-machine");
    let systemd_dir = root.join("systemd");
    let openrc_dir = root.join("openrc");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::create_dir_all(&openrc_dir).expect("OpenRC dir can be created");
    let source = root.join("source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan =
        first_machine_plan_with_ployzd(&root, ployzd_artifact(&source, &root.join("bin/ployzd")));
    let runner = RecordingRunner {
        os_release: "ID=alpine\nVERSION_ID=\"3.22\"\n".to_owned(),
        docker_installed: false,
        docker_running: false,
        ..RecordingRunner::root_linux()
    };
    let mut effects = HostRunnerLocalEffects::new(local_config(&root, &systemd_dir), runner);
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert!(
        systemd_dir
            .read_dir()
            .expect("systemd dir reads")
            .next()
            .is_none()
    );
    for service in [
        "nats-server",
        "ployzd-control",
        "ployzd-machine-machine_1",
        "ployzd-dns",
    ] {
        let contents = fs::read_to_string(openrc_dir.join(service))
            .unwrap_or_else(|error| panic!("{service} OpenRC service reads: {error}"));
        assert!(contents.starts_with("#!/sbin/openrc-run\n"));
        assert!(contents.contains("supervisor=\"supervise-daemon\""));
        assert!(
            effects
                .runner()
                .command_calls
                .contains(&format!("rc-update add {service} default"))
        );
    }
}

#[test]
fn local_effects_report_docker_install_failure_as_prepare_failure() {
    let root = temp_dir("ployz-host-runner-local-docker-install-fail");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan =
        first_machine_plan_with_ployzd(&root, ployzd_artifact(&source, &root.join("bin/ployzd")));
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            docker_installed: false,
            docker_running: false,
            download_body: Some(b"docker install script\n".to_vec()),
            fail_docker_install: true,
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
            reason: HostRunnerStepFailureReason::ContainerRuntimePrepareFailed,
            message,
        })) if message.as_str() == "simulated docker install failure"
    ));
}

#[test]
fn local_effects_report_docker_info_failure_after_install_as_verify_failure() {
    let root = temp_dir("ployz-host-runner-local-docker-verify-fail");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let plan =
        first_machine_plan_with_ployzd(&root, ployzd_artifact(&source, &root.join("bin/ployzd")));
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            docker_installed: false,
            docker_running: false,
            download_body: Some(b"docker install script\n".to_vec()),
            force_docker_info_failure: true,
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
            reason: HostRunnerStepFailureReason::ContainerRuntimePrepareFailed,
            message,
        })) if message.as_str() == "simulated docker info failure"
    ));
    assert_eq!(effects.runner().docker_install_runs, 1);
}

#[test]
fn local_effects_download_remote_artifact_sources() {
    let root = temp_dir("ployz-host-runner-local-remote-source");
    let systemd_dir = root.join("systemd");
    let install_path = root.join("bin/ployzd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let plan = first_machine_plan_with_ployzd(
        &root,
        remote_ployzd_artifact("https://example.invalid/ployzd", &install_path),
    );
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            download_body: Some(b"ployz\n".to_vec()),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert_eq!(fs::read_to_string(install_path).unwrap(), "ployz\n");
    let downloads = effects.runner().downloads.clone();
    assert_eq!(downloads.len(), 1);
    assert!(
        downloads
            .iter()
            .all(|download| download.url == "https://example.invalid/ployzd")
    );
    drop(effects);
    assert!(downloads.iter().all(RecordedDownload::is_cleaned_up));
}

#[test]
fn local_effects_remove_partial_remote_download_after_failure() {
    let root = temp_dir("ployz-host-runner-local-remote-source-fail");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let url = "https://example.invalid/ployzd";
    let plan = first_machine_plan_with_ployzd(
        &root,
        remote_ployzd_artifact(url, &root.join("bin/ployzd")),
    );
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            fail_download: Some(url.to_owned()),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::InstallArtifact(_),
            reason: HostRunnerStepFailureReason::ArtifactDownloadFailed,
            message,
        })) if message.as_str() == "simulated download failure"
    ));
    let downloads = effects.runner().downloads.clone();
    assert_eq!(downloads.len(), 1);
    assert!(downloads.iter().all(RecordedDownload::is_cleaned_up));
}

#[test]
fn local_effects_report_remote_artifact_digest_mismatch_as_verification_failure() {
    let root = temp_dir("ployz-host-runner-local-remote-digest-fail");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let plan = first_machine_plan_with_ployzd(
        &root,
        remote_ployzd_artifact("https://example.invalid/ployzd", &root.join("bin/ployzd")),
    );
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            download_body: Some(b"wrong\n".to_vec()),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let first = execute_host_runner_plan(&plan, &mut effects, &mut recorder);
    let second = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        first.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::InstallArtifact(_),
            reason: HostRunnerStepFailureReason::ArtifactVerificationFailed,
            ..
        }))
    ));
    assert!(matches!(
        second.terminal.failure(),
        Some(HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::InstallArtifact(_),
            reason: HostRunnerStepFailureReason::ArtifactVerificationFailed,
            ..
        }))
    ));
    assert_eq!(effects.runner().downloads.len(), 2);
    assert!(
        effects
            .runner()
            .downloads
            .iter()
            .all(RecordedDownload::is_cleaned_up)
    );
}

#[test]
fn local_effects_write_nats_config_before_nats_unit() {
    let root = temp_dir("ployz-host-runner-local-nats-config");
    let source = root.join("ployzd-source");
    let install_path = root.join("bin/ployzd");
    let nats_source = root.join("nats-server-source");
    let nats_install_path = root.join("bin/nats-server");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");

    let ployzd_artifact = ployzd_artifact(&source, &install_path);
    let plan = first_machine_install_plan(
        FirstMachineInstallTarget::new(
            machine_id("machine_1"),
            ployzd_artifact,
            dataplane_artifacts(&root),
            railpack_artifact(&root),
            nats_server_artifact(&nats_source, &nats_install_path),
            InstallRolePolicy::install_all().without_gateway(),
            HostPortAssurance::Keeper,
            test_identity().clone(),
            WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
            WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
        )
        .with_nats_server_unit(nats_unit(&root))
        .with_nats_material_paths(nats_material(&root))
        .with_role_environment(role_env(&root)),
    );
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert!(fs::read_to_string(&install_path).is_ok());
    assert_eq!(
        fs::read_to_string(root.join("etc/nats-server.conf")).unwrap(),
        expected_loopback_nats_config(&root)
    );
    assert!(systemd_dir.join("nats-server.service").exists());
    let config_write_position = recorder
        .events
        .iter()
        .position(|event| {
            matches!(
                event,
                HostRunnerStepEvent::Succeeded {
                    step: HostRunnerStepLabel::WriteNatsServerConfig(_)
                }
            )
        })
        .expect("nats config write succeeded");
    let unit_write_position = recorder
        .events
        .iter()
        .position(|event| {
            matches!(
                event,
                HostRunnerStepEvent::Started {
                    step: HostRunnerStepLabel::WriteSupervisorUnit(
                        SupervisorUnitTarget::NatsServer
                    )
                }
            )
        })
        .expect("nats unit write started");
    assert!(config_write_position < unit_write_position);
}

#[test]
fn local_effects_render_role_units_from_the_artifact_installed_by_the_plan() {
    let root = temp_dir("ployz-host-runner-local-plan-artifact-source");
    let source = root.join("ployzd-source");
    let install_path = root.join("plan/bin/ployzd");
    let nats_source = root.join("nats-server-source");
    let nats_install_path = root.join("plan/bin/nats-server");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");

    let plan = first_machine_install_plan(
        FirstMachineInstallTarget::new(
            machine_id("machine_1"),
            ployzd_artifact(&source, &install_path),
            dataplane_artifacts(&root),
            railpack_artifact(&root),
            nats_server_artifact(&nats_source, &nats_install_path),
            InstallRolePolicy::install_all().without_gateway(),
            HostPortAssurance::Keeper,
            test_identity().clone(),
            WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
            WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
        )
        .with_nats_server_unit(nats_unit(&root))
        .with_nats_material_paths(nats_material(&root))
        .with_role_environment(role_env(&root)),
    );
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    let control_unit = fs::read_to_string(systemd_dir.join("ployzd-control.service")).unwrap();
    assert!(control_unit.contains(install_path.to_str().expect("path is utf-8")));
    assert!(!control_unit.contains("ployzd-config-source"));
}

#[test]
fn local_join_redeems_token_then_installs_assigned_roles() {
    let root = temp_dir("ployz-host-runner-local-join");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("ployzd-source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let target = HostRunnerJoinTarget::new(
        HostRunnerJoinMaterial::new(
            machine_id("machine_2"),
            "prod",
            NatsUserSeed::try_new("SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM")
                .expect("valid nats credentials"),
            test_ca_pem(),
            WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
            WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
        )
        .expect("valid join material"),
        ployzd_artifact(&source, &root.join("join/bin/ployzd")),
        dataplane_artifacts(&root),
        railpack_artifact(&root),
        NonEmptyRoleSet::try_new(vec![
            DaemonProcessRole::Machine(machine_id("machine_2")),
            DaemonProcessRole::Gateway,
        ])
        .expect("non-empty role set"),
        edge_runtime_role_env(&root),
        HostPortAssurance::Keeper,
    );
    let mut redeemer = StaticJoinRedeemer {
        expected_token: JoinToken::try_new("join_once").expect("valid join token"),
        target,
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_host_runner_join(
        &JoinToken::try_new("join_once").expect("valid join token"),
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert_eq!(execution.terminal, HostRunnerPlanTerminal::Completed);
    assert_eq!(
        fs::read_to_string(
            root.join("state")
                .join(JOIN_MATERIAL_DIR)
                .join(JOIN_MATERIAL_FILE)
        )
        .expect("join material is stored"),
        format!(
            "machine_id=machine_2\ncluster_name=prod\nnats_credentials=[redacted]\ntrusted_nats_ca_sha256={}\ndataplane_endpoint_supernet=10.198.0.0/16\n",
            crate::plan::ca_pem_sha256(test_ca_pem().as_str())
        )
    );
    assert_eq!(
        fs::read_to_string(
            root.join("state")
                .join(JOIN_MATERIAL_DIR)
                .join(JOIN_NATS_CREDENTIALS_FILE),
        )
        .expect("nats credentials are stored"),
        "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM"
    );
    assert_secret_file_mode(
        root.join("state")
            .join(JOIN_MATERIAL_DIR)
            .join(JOIN_NATS_CREDENTIALS_FILE),
    );
    assert_eq!(
        fs::read_to_string(
            root.join("state")
                .join(JOIN_MATERIAL_DIR)
                .join(JOIN_TRUSTED_CA_FILE),
        )
        .expect("trusted CA is stored"),
        test_ca_pem().as_str()
    );
    assert!(root.join("join/bin/ployzd").exists());
    // Joined-machine roles share the single redeemed per-machine seed.
    let join_material_dir = root.join("state").join(JOIN_MATERIAL_DIR);
    let expected_edge_env = format!(
        "PLOYZ_NATS_URL=nats://127.0.0.1:7422\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_MACHINE_ID=machine_2\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
        ca = join_material_dir.join(JOIN_TRUSTED_CA_FILE).display(),
        seed = join_material_dir.join(JOIN_NATS_CREDENTIALS_FILE).display(),
        bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
        ctl = root.join("bin/ployz-ebpf-ctl").display(),
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-machine.env")).unwrap(),
        expected_edge_env
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-gateway.env")).unwrap(),
        format!(
            "PLOYZ_NATS_URL=nats://127.0.0.1:7422\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_MACHINE_ID=machine_2\nPLOYZ_GATEWAY_LISTEN_ADDR=0.0.0.0:80\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
            ca = join_material_dir.join(JOIN_TRUSTED_CA_FILE).display(),
            seed = join_material_dir.join(JOIN_NATS_CREDENTIALS_FILE).display(),
            bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
            ctl = root.join("bin/ployz-ebpf-ctl").display(),
        )
    );
    assert!(
        systemd_dir
            .join("ployzd-machine-machine_2.service")
            .exists()
    );
    assert!(systemd_dir.join("ployzd-gateway.service").exists());
    assert_eq!(reporter.reports, vec![JoinReport::Completed]);
    assert_eq!(token_consumer.consumed, 1);
    assert_eq!(
        effects.runner().systemctl_calls,
        vec![
            vec!["restart".to_owned(), "docker".to_owned()],
            vec!["daemon-reload".to_owned()],
            vec![
                "enable".to_owned(),
                "ployzd-machine-machine_2.service".to_owned(),
            ],
            vec![
                "restart".to_owned(),
                "ployzd-machine-machine_2.service".to_owned(),
            ],
            vec!["daemon-reload".to_owned()],
            vec!["enable".to_owned(), "ployzd-gateway.service".to_owned()],
            vec!["restart".to_owned(), "ployzd-gateway.service".to_owned()],
        ]
    );
}

#[test]
fn local_effects_store_redacted_join_material() {
    let root = temp_dir("ployz-host-runner-local-join-material");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let material = HostRunnerJoinMaterial::new(
        machine_id("machine_2"),
        "prod",
        NatsUserSeed::try_new("SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM")
            .expect("valid nats credentials"),
        test_ca_pem(),
        WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
        WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
    )
    .expect("valid join material");
    let mut effects = HostRunnerLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );

    effects
        .apply_step(&HostRunnerStep::StoreJoinMaterial(material.clone()))
        .expect("join material stores");
    effects
        .apply_step(&HostRunnerStep::StoreJoinMaterial(material))
        .expect("join material stores idempotently");

    assert_eq!(
        fs::read_to_string(
            root.join("state")
                .join(JOIN_MATERIAL_DIR)
                .join(JOIN_MATERIAL_FILE)
        )
        .expect("join material is stored"),
        format!(
            "machine_id=machine_2\ncluster_name=prod\nnats_credentials=[redacted]\ntrusted_nats_ca_sha256={}\ndataplane_endpoint_supernet=10.198.0.0/16\n",
            crate::plan::ca_pem_sha256(test_ca_pem().as_str())
        )
    );
    assert_eq!(
        fs::read_to_string(
            root.join("state")
                .join(JOIN_MATERIAL_DIR)
                .join(JOIN_NATS_CREDENTIALS_FILE),
        )
        .expect("nats credentials are stored"),
        "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM"
    );
    assert_secret_file_mode(
        root.join("state")
            .join(JOIN_MATERIAL_DIR)
            .join(JOIN_NATS_CREDENTIALS_FILE),
    );
    assert_eq!(
        fs::read(
            root.join("state")
                .join(JOIN_MATERIAL_DIR)
                .join(JOIN_RECOVERY_KEY_FILE),
        )
        .expect("wrapped CA recovery key is delivered to the joined machine"),
        b"wrapped-ca-key"
    );
    assert_secret_file_mode(
        root.join("state")
            .join(JOIN_MATERIAL_DIR)
            .join(JOIN_RECOVERY_KEY_FILE),
    );
}

#[derive(Debug, Default)]
struct RecordingTokenConsumer {
    consumed: usize,
}

impl HostRunnerJoinTokenConsumer for RecordingTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        self.consumed += 1;
        Ok(())
    }
}

#[derive(Debug)]
struct StaticJoinRedeemer {
    expected_token: JoinToken,
    target: HostRunnerJoinTarget,
}

impl HostRunnerJoinRedeemer for StaticJoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedHostRunnerJoin, FailureMessage> {
        if *token != self.expected_token {
            return Err(failure_message("unexpected join token"));
        }

        Ok(RedeemedHostRunnerJoin::new(
            operation_id("op_machine"),
            machine_id("machine_2"),
            self.target.clone(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JoinReport {
    Completed,
    Failed { failure: MachineJoinReportFailure },
}

#[derive(Debug, Default)]
struct RecordingJoinReporter {
    reports: Vec<JoinReport>,
}

impl HostRunnerJoinReporter for RecordingJoinReporter {
    fn report_join_completed(&mut self) -> Result<(), FailureMessage> {
        self.reports.push(JoinReport::Completed);
        Ok(())
    }

    fn report_join_failed(
        &mut self,
        failure: MachineJoinReportFailure,
    ) -> Result<(), FailureMessage> {
        self.reports.push(JoinReport::Failed { failure });
        Ok(())
    }
}

#[derive(Debug)]
struct RecordingRunner {
    command_calls: Vec<String>,
    command_outputs: VecDeque<HostRunnerCommandOutput>,
    linux: bool,
    uid: u32,
    os_release: String,
    dnf_available: bool,
    docker_installed: bool,
    docker_running: bool,
    docker_install_runs: usize,
    docker_install_scripts: Vec<String>,
    dataplane_host_prepare_runs: usize,
    dataplane_ready: bool,
    fail_docker_install: bool,
    fail_dataplane_host_prepare: bool,
    force_docker_info_failure: bool,
    containerd_snapshotter: bool,
    systemctl_calls: Vec<Vec<String>>,
    fail_systemctl: Option<Vec<String>>,
    downloads: Vec<RecordedDownload>,
    download_body: Option<Vec<u8>>,
    fail_download: Option<String>,
}

impl RecordingRunner {
    fn root_linux() -> Self {
        Self {
            command_calls: Vec::new(),
            command_outputs: VecDeque::new(),
            linux: true,
            uid: 0,
            os_release: "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n".to_owned(),
            dnf_available: true,
            docker_installed: true,
            docker_running: true,
            docker_install_runs: 0,
            docker_install_scripts: Vec::new(),
            dataplane_host_prepare_runs: 0,
            dataplane_ready: false,
            fail_docker_install: false,
            fail_dataplane_host_prepare: false,
            force_docker_info_failure: false,
            containerd_snapshotter: true,
            systemctl_calls: Vec::new(),
            fail_systemctl: None,
            downloads: Vec::new(),
            download_body: None,
            fail_download: None,
        }
    }
}

impl HostRunnerCommandRunner for RecordingRunner {
    fn command(
        &mut self,
        program: &str,
        args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        self.command_calls
            .push(format!("{program} {}", args.join(" ")));
        if let Some(output) = self.command_outputs.pop_front() {
            return Ok(output);
        }
        if program == "dnf" && !self.dnf_available {
            return Err(failure_message("failed to run dnf"));
        }
        if program == "systemctl" {
            let call = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
            if !matches!(args.first(), Some(&"is-active" | &"list-units")) {
                self.systemctl_calls.push(call.clone());
            }
            if self.fail_systemctl.as_ref() == Some(&call) {
                return Ok(HostRunnerCommandOutput {
                    success: false,
                    exit_code: Some(1),
                    stdout: String::new(),
                    stdout_truncated: false,
                    failure: "simulated systemctl failure".to_owned(),
                });
            }
            if args.first() == Some(&"is-active") {
                return Ok(HostRunnerCommandOutput {
                    success: false,
                    exit_code: Some(3),
                    stdout: String::new(),
                    stdout_truncated: false,
                    failure: "inactive".to_owned(),
                });
            }
            if args == ["enable", "--now", "docker"] && self.docker_installed {
                self.docker_running = true;
            }
            return Ok(succeeded_command(""));
        }
        if matches!(program, "apk" | "pacman" | "zypper" | "dnf" | "yum")
            && args
                .iter()
                .any(|arg| *arg == "docker" || *arg == "docker-ce")
        {
            self.docker_installed = true;
            self.docker_install_runs += 1;
            return Ok(succeeded_command(""));
        }
        if program == "apt-get" && args == ["update"] {
            return Ok(succeeded_command(""));
        }
        if program == "zypper" && args == ["refresh"] {
            return Ok(succeeded_command(""));
        }
        if args.contains(&"wireguard-tools") {
            self.dataplane_host_prepare_runs += 1;
            if self.fail_dataplane_host_prepare {
                return Ok(HostRunnerCommandOutput {
                    success: false,
                    exit_code: Some(1),
                    stdout: String::new(),
                    stdout_truncated: false,
                    failure: "simulated dataplane host prepare failure".to_owned(),
                });
            }
            self.dataplane_ready = true;
            return Ok(succeeded_command(""));
        }
        if program == "rc-update" {
            return Ok(succeeded_command(""));
        }
        if program == "rc-service" {
            if args.get(1) == Some(&"status") {
                return Ok(failed_command());
            }
            if args.first() == Some(&"docker") {
                self.docker_running = true;
            }
            return Ok(succeeded_command(""));
        }
        if program == "rc-status" {
            return Ok(succeeded_command(""));
        }
        Ok({
            if program == "systemctl" && args.first() == Some(&"list-units") {
                succeeded_command("")
            } else if program == "sh" && args.first() == Some(&"-c") {
                absent_command()
            } else {
                failed_command()
            }
        })
    }

    fn command_with_timeout(
        &mut self,
        program: &str,
        args: &[&str],
        _timeout: Duration,
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        if program != "sh" {
            return self.command(program, args);
        }
        let [script] = args else {
            return Ok(failed_command());
        };
        let script = fs::read_to_string(script)
            .map_err(|error| failure_message(&format!("failed to read fake script: {error}")))?;
        self.docker_install_scripts.push(script);
        self.docker_install_runs += 1;
        if self.fail_docker_install {
            return Ok(HostRunnerCommandOutput {
                success: false,
                exit_code: Some(1),
                stdout: String::new(),
                stdout_truncated: false,
                failure: "simulated docker install failure".to_owned(),
            });
        }
        self.docker_installed = true;
        Ok(succeeded_command(""))
    }

    fn read_os_release(&mut self) -> Result<String, FailureMessage> {
        Ok(self.os_release.clone())
    }

    fn is_linux(&mut self) -> bool {
        self.linux
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(self.uid)
    }

    fn download(&mut self, url: &str, destination: &Path) -> Result<(), FailureMessage> {
        if self.fail_download.as_deref() == Some(url) {
            fs::write(destination, b"partial")
                .map_err(|error| failure_message(&format!("failed fake partial write: {error}")))?;
            self.downloads.push(RecordedDownload {
                url: url.to_owned(),
                destination: destination.to_path_buf(),
            });
            return Err(failure_message("simulated download failure"));
        }
        let body = self
            .download_body
            .as_deref()
            .ok_or_else(|| failure_message("missing fake artifact body"))?;
        fs::write(destination, body)
            .map_err(|error| failure_message(&format!("failed fake artifact write: {error}")))?;
        self.downloads.push(RecordedDownload {
            url: url.to_owned(),
            destination: destination.to_path_buf(),
        });
        Ok(())
    }

    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        if self.force_docker_info_failure {
            return Err(failure_message("simulated docker info failure"));
        }
        if self.docker_installed && self.docker_running {
            return Ok(());
        }
        Err(failure_message("simulated docker info failure"))
    }

    fn docker_is_installed(&mut self) -> bool {
        self.docker_installed
    }

    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        self.docker_info()?;
        Ok(self.containerd_snapshotter)
    }

    fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
        Ok(true)
    }

    fn dataplane_host_ready(&mut self) -> bool {
        self.dataplane_ready
    }
}

#[derive(Debug, Clone)]
struct RecordedDownload {
    url: String,
    destination: PathBuf,
}

impl RecordedDownload {
    fn is_cleaned_up(&self) -> bool {
        !self.destination.exists()
            && self
                .destination
                .parent()
                .is_none_or(|parent| !parent.exists())
    }
}

#[derive(Default)]
struct RecordingRecorder {
    events: Vec<HostRunnerStepEvent>,
}

impl HostRunnerStepRecorder for RecordingRecorder {
    fn record_step_event(&mut self, event: &HostRunnerStepEvent) -> Result<(), FailureMessage> {
        self.events.push(event.clone());
        Ok(())
    }
}

fn local_config(root: &Path, systemd_dir: &Path) -> HostRunnerLocalConfig {
    HostRunnerLocalConfig {
        supervisor_dirs: crate::execution::SupervisorDirectories::new(
            systemd_dir.to_path_buf(),
            root.join("openrc"),
        ),
        state_dir: root.join("state"),
        docker_daemon_config: root.join("etc/docker/daemon.json"),
        docker_repository_dir: root.join("etc/yum.repos.d"),
    }
}

fn validate_host(effects: &mut HostRunnerLocalEffects<RecordingRunner>) {
    effects
        .apply_step(&HostRunnerStep::VerifyHost(HostPrerequisite::LinuxRoot))
        .expect("host platform validates");
}

fn nats_unit(root: &Path) -> NatsServerUnitTarget {
    NatsServerUnitTarget::new(
        root.join("bin/nats-server"),
        root.join("etc/nats-server.conf"),
    )
    .expect("valid nats-server unit target")
}

fn nats_material(root: &Path) -> NatsMachineMaterialPaths {
    NatsMachineMaterialPaths::new(root.join("nats"))
}

fn test_ca_pem() -> NatsCaCertificatePem {
    NatsCaCertificatePem::try_new(
        "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
    )
    .expect("valid test CA pem")
}

fn test_identity() -> &'static ClusterNatsIdentity {
    static IDENTITY: OnceLock<ClusterNatsIdentity> = OnceLock::new();
    IDENTITY.get_or_init(|| {
        generate_cluster_nats_identity(
            &ServerCertificateSans::try_new(None, None).expect("valid SAN inputs"),
        )
        .expect("test identity generates")
    })
}

fn expected_loopback_nats_config(root: &Path) -> String {
    format!(
        "server_name: machine_1\nhost: 127.0.0.1\nport: 4222\ntls {{\n  cert_file: \"{cert}\"\n  key_file: \"{key}\"\n}}\njetstream: disabled\ninclude \"authorized-users.conf\"\n",
        cert = root.join("nats/server.crt").display(),
        key = root.join("nats/server.key").display(),
    )
}

fn role_env(root: &Path) -> PloyzdRoleEnvironmentTarget {
    role_env_for_machine(root, machine_id("machine_1"))
}

fn role_env_for_machine(root: &Path, machine_id: MachineId) -> PloyzdRoleEnvironmentTarget {
    PloyzdRoleEnvironmentTarget::new(
        PloyzdRoleEnvironmentFile::new(root.join("etc/ployzd.env"))
            .expect("valid ployzd role environment target"),
        machine_id,
        NatsClientUrl::try_new("tls://127.0.0.1:4222").expect("valid NATS URL"),
        RoleNatsCredentials::cluster(&nats_material(root)),
    )
}

fn edge_runtime_role_env(root: &Path) -> PloyzdRoleEnvironmentTarget {
    PloyzdRoleEnvironmentTarget::new(
        PloyzdRoleEnvironmentFile::new(root.join("etc/ployzd.env"))
            .expect("valid ployzd role environment target"),
        machine_id("machine_2"),
        NatsClientUrl::loopback(7422),
        RoleNatsCredentials::joined(&root.join("state").join(JOIN_MATERIAL_DIR)),
    )
}

fn first_machine_plan_with_ployzd(
    root: &Path,
    ployzd: ArtifactTarget,
) -> crate::plan::HostRunnerStepPlan {
    let nats_source = root.join("nats-server-source");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");
    first_machine_install_plan(
        FirstMachineInstallTarget::new(
            machine_id("machine_1"),
            ployzd,
            dataplane_artifacts(root),
            railpack_artifact(root),
            nats_server_artifact(&nats_source, &root.join("bin/nats-server")),
            InstallRolePolicy::install_all().without_gateway(),
            HostPortAssurance::Keeper,
            test_identity().clone(),
            WrappedCaKey::new(b"wrapped-ca-key".to_vec()),
            WrappedCoreSeeds::new(b"wrapped-core-seeds".to_vec()),
        )
        .with_nats_server_unit(nats_unit(root))
        .with_nats_material_paths(nats_material(root))
        .with_role_environment(role_env(root)),
    )
}

fn remote_ployzd_artifact(url: &str, install_path: &Path) -> ArtifactTarget {
    ArtifactTarget::new(
        ArtifactKind::Ployzd,
        version("0.1.0"),
        ArtifactSource::try_new(url).expect("valid remote source"),
        digest(PLOYZ_NEWLINE_SHA256),
        install_path.to_path_buf(),
    )
    .expect("valid ployzd artifact")
}

fn ployzd_artifact(source: &Path, install_path: &Path) -> ArtifactTarget {
    ArtifactTarget::new(
        ArtifactKind::Ployzd,
        version("0.1.0"),
        artifact_source(source),
        digest(PLOYZ_NEWLINE_SHA256),
        install_path.to_path_buf(),
    )
    .expect("valid ployzd artifact")
}

fn railpack_artifact(root: &Path) -> ArtifactTarget {
    let source = root.join("railpack-source");
    fs::write(&source, "ployz\n").expect("Railpack source can be written");
    ArtifactTarget::new(
        ArtifactKind::Railpack,
        version("v0.31.0"),
        artifact_source(&source),
        digest(PLOYZ_NEWLINE_SHA256),
        root.join("lib/ployz/railpack/v0.31.0/railpack"),
    )
    .expect("valid Railpack artifact")
}

fn nats_server_artifact(source: &Path, install_path: &Path) -> ArtifactTarget {
    write_nats_server_archive(source, b"ployz\n");
    ArtifactTarget::new(
        ArtifactKind::NatsServer,
        version("2.12.0"),
        artifact_source(source),
        digest(sha256(source).as_str()),
        install_path.to_path_buf(),
    )
    .expect("valid nats-server artifact")
}

fn write_nats_server_archive(path: &Path, binary: &[u8]) {
    let root = path
        .parent()
        .expect("test nats archive path has parent")
        .join("nats-server-archive");
    let package = root.join("nats-server-v2.12.0-linux-amd64");
    fs::create_dir_all(&package).expect("nats archive package dir can be created");
    fs::write(package.join("nats-server"), binary).expect("nats binary can be written");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(path)
        .arg("-C")
        .arg(&root)
        .arg("nats-server-v2.12.0-linux-amd64")
        .status()
        .expect("tar can run");
    assert!(status.success());
}

fn sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};

    let bytes = fs::read(path).expect("file can be read");
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

fn ebpf_bytecode_artifact(root: &Path) -> ArtifactTarget {
    let source = root.join("ployz-ebpf-tc-source");
    fs::write(&source, "ployz\n").expect("eBPF bytecode source can be written");
    ArtifactTarget::new(
        ArtifactKind::EbpfBytecode,
        version("0.1.0"),
        artifact_source(&source),
        digest(PLOYZ_NEWLINE_SHA256),
        root.join("lib/ployz/ebpf/ployz-ebpf-tc"),
    )
    .expect("valid eBPF bytecode artifact")
}

fn ebpf_ctl_artifact(root: &Path) -> ArtifactTarget {
    let source = root.join("ployz-ebpf-ctl-source");
    fs::write(&source, "ployz\n").expect("eBPF ctl source can be written");
    ArtifactTarget::new(
        ArtifactKind::EbpfCtl,
        version("0.1.0"),
        artifact_source(&source),
        digest(PLOYZ_NEWLINE_SHA256),
        root.join("bin/ployz-ebpf-ctl"),
    )
    .expect("valid eBPF ctl artifact")
}

fn dataplane_artifacts(root: &Path) -> DataplaneArtifactTargets {
    DataplaneArtifactTargets::new(ebpf_bytecode_artifact(root), ebpf_ctl_artifact(root))
}

fn artifact_source(path: &Path) -> ArtifactSource {
    ArtifactSource::try_new(path.to_str().expect("temp path is utf-8")).expect("valid source")
}

fn docker_enable_call() -> Vec<String> {
    ["enable", "--now", "docker"]
        .iter()
        .map(|value| (*value).to_owned())
        .collect()
}

fn temp_dir(prefix: &str) -> PathBuf {
    let unique = format!(
        "{}-{}",
        prefix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    fs::create_dir_all(&path).expect("temp dir can be created");
    path
}

fn assert_secret_file_mode(path: PathBuf) {
    ployz_test_support::fs::assert_file_mode(&path, 0o600);
}

const PLOYZ_NEWLINE_SHA256: &str =
    "2dcc3bb1142455239d3b3391d9569a8ce0fbdfb906cd0434329e5dd736592138";
