use ployz_core::install::{WrappedCaKey, WrappedCoreSeeds};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ployz_core::ids::MachineId;
use ployz_core::install::NatsMachineMaterialPaths;
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, InstallRolePolicy};
use ployz_host_runner::artifacts::{
    ArtifactKind, ArtifactSource, ArtifactTarget, DataplaneArtifactTargets,
};
use ployz_host_runner::command::HostRunnerCommandRunner;
use ployz_host_runner::executor::{
    HostRunnerPlanFailure, HostRunnerPlanTerminal, HostRunnerStepEffects, HostRunnerStepEvent,
    HostRunnerStepRecorder, execute_host_runner_plan,
};
use ployz_host_runner::join::{
    JOIN_MATERIAL_DIR, JOIN_MATERIAL_FILE, JOIN_NATS_CREDENTIALS_FILE, JOIN_RECOVERY_KEY_FILE,
    JOIN_TRUSTED_CA_FILE,
};
use ployz_host_runner::join_executor::{
    HostRunnerJoinRedeemer, HostRunnerJoinReporter, HostRunnerJoinTokenConsumer,
    RedeemedHostRunnerJoin, execute_host_runner_join,
};
use ployz_host_runner::local::{HostRunnerLocalConfig, HostRunnerLocalEffects};
use ployz_host_runner::nats_identity::{
    ClusterNatsIdentity, ServerCertificateSans, generate_cluster_nats_identity,
};
use ployz_host_runner::steps::{
    ContainerRuntime, FirstMachineInstallTarget, HostRunnerJoinMaterial, HostRunnerJoinTarget,
    HostRunnerStep, HostRunnerStepFailure, HostRunnerStepFailureReason, HostRunnerStepLabel,
    JoinToken, NonEmptyRoleSet, PloyzdRoleEnvironmentTarget, RoleNatsCredentials,
    first_machine_install_plan,
};
use ployz_host_runner::systemd::{
    NatsServerUnitTarget, PloyzdRoleEnvironmentFile, SupervisorUnitTarget,
};
use ployz_nats::connect::NatsClientUrl;
use ployz_sdk_types::MachineJoinReportFailure;
use ployz_test_support::host_runner::{artifact_version as version, sha256_digest as digest};
use ployz_test_support::ids::{failure_message, machine_id, operation_id};
use std::sync::OnceLock;

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
            nats_server_artifact(&nats_source, &nats_install_path),
            InstallRolePolicy::install_all()
                .without_gateway()
                .without_dns(),
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
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_MACHINE_ID=machine_1\nPLOYZ_JOIN_NKEY_SEED_FILE={join_seed}\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
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
        nats_server_artifact(&nats_source, &root.join("bin/nats-server")),
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
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
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_MACHINE_ID=machine_1\nPLOYZ_JOIN_NKEY_SEED_FILE={join_seed}\nPLOYZ_MACHINE_BOOTSTRAP_URL=https://example.test/ployz.sh\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
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
        nats_server_artifact(&nats_source, &root.join("bin/nats-server")),
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
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
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_MACHINE_ID=machine_1\nPLOYZ_JOIN_NKEY_SEED_FILE={join_seed}\nPLOYZ_MACHINE_JOIN_TEMPLATE_FILE={template}\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
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
            step: HostRunnerStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker),
            reason: HostRunnerStepFailureReason::ContainerRuntimeVerifyFailed,
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
            nats_server_artifact(&nats_source, &nats_install_path),
            InstallRolePolicy::install_all()
                .without_gateway()
                .without_dns(),
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
            nats_server_artifact(&nats_source, &nats_install_path),
            InstallRolePolicy::install_all()
                .without_gateway()
                .without_dns(),
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
        NonEmptyRoleSet::try_new(vec![
            DaemonProcessRole::Machine(machine_id("machine_2")),
            DaemonProcessRole::Gateway,
        ])
        .expect("non-empty role set"),
        edge_runtime_role_env(&root),
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
            "machine_id=machine_2\ncluster_name=prod\nnats_credentials=[redacted]\ntrusted_nats_ca_sha256={}\n",
            ployz_host_runner::steps::ca_pem_sha256(test_ca_pem().as_str())
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
            "machine_id=machine_2\ncluster_name=prod\nnats_credentials=[redacted]\ntrusted_nats_ca_sha256={}\n",
            ployz_host_runner::steps::ca_pem_sha256(test_ca_pem().as_str())
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
    linux: bool,
    uid: u32,
    docker_installed: bool,
    docker_running: bool,
    docker_install_runs: usize,
    dataplane_host_prepare_runs: usize,
    fail_docker_install: bool,
    fail_dataplane_host_prepare: bool,
    force_docker_info_failure: bool,
    systemctl_calls: Vec<Vec<String>>,
    fail_systemctl: Option<Vec<String>>,
    downloads: Vec<RecordedDownload>,
    download_body: Option<Vec<u8>>,
    fail_download: Option<String>,
}

impl RecordingRunner {
    fn root_linux() -> Self {
        Self {
            linux: true,
            uid: 0,
            docker_installed: true,
            docker_running: true,
            docker_install_runs: 0,
            dataplane_host_prepare_runs: 0,
            fail_docker_install: false,
            fail_dataplane_host_prepare: false,
            force_docker_info_failure: false,
            systemctl_calls: Vec::new(),
            fail_systemctl: None,
            downloads: Vec::new(),
            download_body: None,
            fail_download: None,
        }
    }
}

impl HostRunnerCommandRunner for RecordingRunner {
    fn is_linux(&mut self) -> bool {
        self.linux
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(self.uid)
    }

    fn systemctl(&mut self, args: &[&str]) -> Result<(), FailureMessage> {
        let call = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        self.systemctl_calls.push(call.clone());
        if self.fail_systemctl.as_ref() == Some(&call) {
            return Err(failure_message("simulated systemctl failure"));
        }
        Ok(())
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

    fn enable_docker_service(&mut self) -> Result<(), FailureMessage> {
        self.systemctl(&["enable", "--now", "docker"])?;
        if !self.docker_installed {
            return Err(failure_message("simulated docker service missing"));
        }
        self.docker_running = true;
        Ok(())
    }

    fn run_docker_install_script(&mut self, _script: &Path) -> Result<(), FailureMessage> {
        self.docker_install_runs += 1;
        if self.fail_docker_install {
            return Err(failure_message("simulated docker install failure"));
        }
        self.docker_installed = true;
        Ok(())
    }

    fn prepare_dataplane_host(&mut self) -> Result<(), FailureMessage> {
        self.dataplane_host_prepare_runs += 1;
        if self.fail_dataplane_host_prepare {
            return Err(failure_message("simulated dataplane host prepare failure"));
        }
        Ok(())
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
        systemd_dir: systemd_dir.to_path_buf(),
        state_dir: root.join("state"),
    }
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
) -> ployz_host_runner::steps::HostRunnerStepPlan {
    let nats_source = root.join("nats-server-source");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");
    first_machine_install_plan(
        FirstMachineInstallTarget::new(
            machine_id("machine_1"),
            ployzd,
            dataplane_artifacts(root),
            nats_server_artifact(&nats_source, &root.join("bin/nats-server")),
            InstallRolePolicy::install_all()
                .without_gateway()
                .without_dns(),
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
