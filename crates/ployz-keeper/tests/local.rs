use std::fs;
use std::path::{Path, PathBuf};

use ployz_core::ids::{NodeId, OperationId};
use ployz_core::install::NatsMachineMaterialPaths;
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, FirstNodeGateway};
use ployz_keeper::artifacts::{
    ArtifactSource, ArtifactVersion, DataplaneArtifactTargets, EbpfBytecodeArtifactTarget,
    EbpfCtlArtifactTarget, NatsServerArtifactTarget, PloyzdArtifactTarget, Sha256Digest,
};
use ployz_keeper::executor::{
    KeeperPlanFailure, KeeperPlanTerminal, KeeperStepEffects, KeeperStepEvent, KeeperStepRecorder,
    execute_keeper_plan,
};
use ployz_keeper::join::{
    JOIN_MATERIAL_DIR, JOIN_MATERIAL_FILE, JOIN_NATS_CREDENTIALS_FILE, JOIN_TRUSTED_CA_FILE,
};
use ployz_keeper::join_executor::{
    KeeperJoinRedeemer, KeeperJoinReporter, KeeperJoinTokenConsumer, RedeemedKeeperJoin,
    execute_keeper_join,
};
use ployz_keeper::local::{KeeperCommandRunner, KeeperLocalConfig, KeeperLocalEffects};
use ployz_keeper::nats_identity::{
    ClusterNatsIdentity, ServerCertificateSans, generate_cluster_nats_identity,
};
use ployz_keeper::steps::{
    FirstNodeInstallTarget, JoinToken, KeeperJoinMaterial, KeeperJoinTarget, KeeperStep,
    KeeperStepFailure, KeeperStepFailureReason, KeeperStepLabel, NonEmptyRoleSet,
    PloyzdRoleEnvironmentTarget, RoleNatsCredentials, first_node_install_plan,
};
use ployz_keeper::systemd::{
    NatsServerUnitTarget, PloyzdRoleEnvironmentFile, SupervisorUnitTarget,
};
use ployz_nats::connect::NatsClientUrl;
use ployz_sdk_types::MachineJoinReportFailure;
use std::sync::OnceLock;

#[test]
fn local_effects_install_first_node_process_units() {
    let root = temp_dir("ployz-keeper-local-first-node");
    let source = root.join("ployzd-source");
    let install_path = root.join("bin/ployzd");
    let nats_source = root.join("nats-server-source");
    let nats_install_path = root.join("bin/nats-server");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");

    let ployzd_artifact = ployzd_artifact(&source, &install_path);
    let plan = first_node_install_plan(
        FirstNodeInstallTarget::new(
            node_id("node_1"),
            ployzd_artifact,
            dataplane_artifacts(&root),
            nats_server_artifact(&nats_source, &nats_install_path),
            FirstNodeGateway::Skip,
            test_identity().clone(),
        )
        .with_nats_server_unit(nats_unit(&root))
        .with_nats_material_paths(nats_material(&root))
        .with_role_environment(role_env(&root)),
    );
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
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
    assert!(!root.join("nats/node.seed").exists());
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
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_NODE_ID=node_1\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
            ca = root.join("nats/ca.pem").display(),
            seed = root.join("nats/controller.seed").display(),
            bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
            ctl = root.join("bin/ployz-ebpf-ctl").display()
        )
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-node.env")).unwrap(),
        format!(
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_NODE_ID=node_1\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
            ca = root.join("nats/ca.pem").display(),
            seed = root.join("nats/node.seed").display(),
            bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
            ctl = root.join("bin/ployz-ebpf-ctl").display()
        )
    );
    assert!(systemd_dir.join("ployzd-node-node_1.service").exists());
    assert!(!systemd_dir.join("ployzd-gateway.service").exists());
}

#[test]
fn first_node_install_writes_machine_bootstrap_url_when_configured() {
    let root = temp_dir("ployz-keeper-first-node-bootstrap-url");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir exists");
    let ployzd_source = root.join("ployzd-source");
    let nats_source = root.join("nats-server-source");
    fs::write(&ployzd_source, "ployz\n").expect("artifact source can be written");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");
    let runner = RecordingRunner::root_linux();
    let target = FirstNodeInstallTarget::new(
        node_id("node_1"),
        ployzd_artifact(&ployzd_source, &root.join("bin/ployzd")),
        dataplane_artifacts(&root),
        nats_server_artifact(&nats_source, &root.join("bin/nats-server")),
        FirstNodeGateway::Skip,
        test_identity().clone(),
    )
    .with_nats_server_unit(nats_unit(&root))
    .with_nats_material_paths(nats_material(&root))
    .with_role_environment(
        role_env(&root).with_machine_bootstrap_url(
            ployz_core::install::MachineBootstrapUrl::try_new("https://example.test/ployz.sh")
                .expect("valid bootstrap url"),
        ),
    );
    let plan = first_node_install_plan(target);
    let mut effects = KeeperLocalEffects::new(local_config(&root, &systemd_dir), runner);

    let execution = execute_keeper_plan(&plan, &mut effects, &mut RecordingRecorder::default());

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-control.env")).unwrap(),
        format!(
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_NODE_ID=node_1\nPLOYZ_MACHINE_BOOTSTRAP_URL=https://example.test/ployz.sh\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
            ca = root.join("nats/ca.pem").display(),
            seed = root.join("nats/controller.seed").display(),
            bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
            ctl = root.join("bin/ployz-ebpf-ctl").display()
        )
    );
}

#[test]
fn first_node_install_writes_machine_join_template_file_when_configured() {
    let root = temp_dir("ployz-keeper-first-node-join-template");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir exists");
    let ployzd_source = root.join("ployzd-source");
    let nats_source = root.join("nats-server-source");
    fs::write(&ployzd_source, "ployz\n").expect("artifact source can be written");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");
    let runner = RecordingRunner::root_linux();
    let template_path = root.join("etc/machine-join-template.json");
    let target = FirstNodeInstallTarget::new(
        node_id("node_1"),
        ployzd_artifact(&ployzd_source, &root.join("bin/ployzd")),
        dataplane_artifacts(&root),
        nats_server_artifact(&nats_source, &root.join("bin/nats-server")),
        FirstNodeGateway::Skip,
        test_identity().clone(),
    )
    .with_nats_server_unit(nats_unit(&root))
    .with_nats_material_paths(nats_material(&root))
    .with_role_environment(
        role_env(&root).with_machine_join_template_file(
            ployz_core::install::AbsoluteInstallPath::try_new(template_path.display().to_string())
                .expect("valid template path"),
        ),
    );
    let plan = first_node_install_plan(target);
    let mut effects = KeeperLocalEffects::new(local_config(&root, &systemd_dir), runner);

    let execution = execute_keeper_plan(&plan, &mut effects, &mut RecordingRecorder::default());

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-control.env")).unwrap(),
        format!(
            "PLOYZ_NATS_URL=tls://127.0.0.1:4222\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_NODE_ID=node_1\nPLOYZ_MACHINE_JOIN_TEMPLATE_FILE={template}\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
            ca = root.join("nats/ca.pem").display(),
            seed = root.join("nats/controller.seed").display(),
            template = template_path.display(),
            bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
            ctl = root.join("bin/ployz-ebpf-ctl").display()
        )
    );
}

#[test]
fn local_effects_fail_before_work_when_host_is_not_root() {
    let root = temp_dir("ployz-keeper-local-not-root");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let plan = first_node_plan_with_ployzd(
        &root,
        ployzd_artifact(&root.join("source"), &root.join("bin/ployzd")),
    );
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            uid: 501,
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::VerifyHost(_),
            reason: KeeperStepFailureReason::HostPrerequisiteFailed,
            message,
        })) if message.as_str() == "keeper must run as root"
    ));
    assert_eq!(effects.runner().systemctl_calls, Vec::<Vec<String>>::new());
}

#[test]
fn local_effects_download_remote_artifact_sources() {
    let root = temp_dir("ployz-keeper-local-remote-source");
    let systemd_dir = root.join("systemd");
    let install_path = root.join("bin/ployzd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let plan = first_node_plan_with_ployzd(
        &root,
        remote_ployzd_artifact("https://example.invalid/ployzd", &install_path),
    );
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            download_body: Some(b"ployz\n".to_vec()),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
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
    let root = temp_dir("ployz-keeper-local-remote-source-fail");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let url = "https://example.invalid/ployzd";
    let plan =
        first_node_plan_with_ployzd(&root, remote_ployzd_artifact(url, &root.join("bin/ployzd")));
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            fail_download: Some(url.to_owned()),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::InstallArtifact(_),
            reason: KeeperStepFailureReason::ArtifactDownloadFailed,
            message,
        })) if message.as_str() == "simulated download failure"
    ));
    let downloads = effects.runner().downloads.clone();
    assert_eq!(downloads.len(), 1);
    assert!(downloads.iter().all(RecordedDownload::is_cleaned_up));
}

#[test]
fn local_effects_report_remote_artifact_digest_mismatch_as_verification_failure() {
    let root = temp_dir("ployz-keeper-local-remote-digest-fail");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let plan = first_node_plan_with_ployzd(
        &root,
        remote_ployzd_artifact("https://example.invalid/ployzd", &root.join("bin/ployzd")),
    );
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner {
            download_body: Some(b"wrong\n".to_vec()),
            ..RecordingRunner::root_linux()
        },
    );
    let mut recorder = RecordingRecorder::default();

    let first = execute_keeper_plan(&plan, &mut effects, &mut recorder);
    let second = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert!(matches!(
        first.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::InstallArtifact(_),
            reason: KeeperStepFailureReason::ArtifactVerificationFailed,
            ..
        }))
    ));
    assert!(matches!(
        second.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::InstallArtifact(_),
            reason: KeeperStepFailureReason::ArtifactVerificationFailed,
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
    let root = temp_dir("ployz-keeper-local-nats-config");
    let source = root.join("ployzd-source");
    let install_path = root.join("bin/ployzd");
    let nats_source = root.join("nats-server-source");
    let nats_install_path = root.join("bin/nats-server");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");

    let ployzd_artifact = ployzd_artifact(&source, &install_path);
    let plan = first_node_install_plan(
        FirstNodeInstallTarget::new(
            node_id("node_1"),
            ployzd_artifact,
            dataplane_artifacts(&root),
            nats_server_artifact(&nats_source, &nats_install_path),
            FirstNodeGateway::Skip,
            test_identity().clone(),
        )
        .with_nats_server_unit(nats_unit(&root))
        .with_nats_material_paths(nats_material(&root))
        .with_role_environment(role_env(&root)),
    );
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
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
                KeeperStepEvent::Succeeded {
                    step: KeeperStepLabel::WriteNatsServerConfig(_)
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
                KeeperStepEvent::Started {
                    step: KeeperStepLabel::WriteSupervisorUnit(SupervisorUnitTarget::NatsServer)
                }
            )
        })
        .expect("nats unit write started");
    assert!(config_write_position < unit_write_position);
}

#[test]
fn local_effects_render_role_units_from_the_artifact_installed_by_the_plan() {
    let root = temp_dir("ployz-keeper-local-plan-artifact-source");
    let source = root.join("ployzd-source");
    let install_path = root.join("plan/bin/ployzd");
    let nats_source = root.join("nats-server-source");
    let nats_install_path = root.join("plan/bin/nats-server");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");

    let plan = first_node_install_plan(
        FirstNodeInstallTarget::new(
            node_id("node_1"),
            ployzd_artifact(&source, &install_path),
            dataplane_artifacts(&root),
            nats_server_artifact(&nats_source, &nats_install_path),
            FirstNodeGateway::Skip,
            test_identity().clone(),
        )
        .with_nats_server_unit(nats_unit(&root))
        .with_nats_material_paths(nats_material(&root))
        .with_role_environment(role_env(&root)),
    );
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    let control_unit = fs::read_to_string(systemd_dir.join("ployzd-control.service")).unwrap();
    assert!(control_unit.contains(install_path.to_str().expect("path is utf-8")));
    assert!(!control_unit.contains("ployzd-config-source"));
}

#[test]
fn local_join_redeems_token_then_installs_assigned_roles() {
    let root = temp_dir("ployz-keeper-local-join");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let source = root.join("ployzd-source");
    fs::write(&source, "ployz\n").expect("artifact source can be written");
    let target = KeeperJoinTarget::new(
        KeeperJoinMaterial::new(
            node_id("node_2"),
            "prod",
            NatsUserSeed::try_new("SUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .expect("valid nats credentials"),
            test_ca_pem(),
        )
        .expect("valid join material"),
        ployzd_artifact(&source, &root.join("join/bin/ployzd")),
        dataplane_artifacts(&root),
        NonEmptyRoleSet::try_new(vec![
            DaemonProcessRole::Node(node_id("node_2")),
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
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &JoinToken::try_new("join_once").expect("valid join token"),
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    assert_eq!(
        fs::read_to_string(
            root.join("state")
                .join(JOIN_MATERIAL_DIR)
                .join(JOIN_MATERIAL_FILE)
        )
        .expect("join material is stored"),
        format!(
            "node_id=node_2\ncluster_name=prod\nnats_credentials=[redacted]\ntrusted_nats_ca_sha256={}\n",
            ployz_keeper::steps::ca_pem_sha256(test_ca_pem().as_str())
        )
    );
    assert_eq!(
        fs::read_to_string(
            root.join("state")
                .join(JOIN_MATERIAL_DIR)
                .join(JOIN_NATS_CREDENTIALS_FILE),
        )
        .expect("nats credentials are stored"),
        "SUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
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
        "PLOYZ_NATS_URL=nats://127.0.0.1:7422\nPLOYZ_NATS_CA_FILE={ca}\nPLOYZ_NATS_NKEY_SEED_FILE={seed}\nPLOYZ_NODE_ID=node_2\nPLOYZ_EBPF_BYTECODE={bytecode}\nPLOYZ_EBPF_CTL={ctl}\n",
        ca = join_material_dir.join(JOIN_TRUSTED_CA_FILE).display(),
        seed = join_material_dir.join(JOIN_NATS_CREDENTIALS_FILE).display(),
        bytecode = root.join("lib/ployz/ebpf/ployz-ebpf-tc").display(),
        ctl = root.join("bin/ployz-ebpf-ctl").display(),
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-node.env")).unwrap(),
        expected_edge_env
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/ployzd-gateway.env")).unwrap(),
        expected_edge_env
    );
    assert!(systemd_dir.join("ployzd-node-node_2.service").exists());
    assert!(systemd_dir.join("ployzd-gateway.service").exists());
    assert_eq!(reporter.reports, vec![JoinReport::Completed]);
    assert_eq!(token_consumer.consumed, 1);
    assert_eq!(
        effects.runner().systemctl_calls,
        vec![
            vec!["daemon-reload".to_owned()],
            vec!["enable".to_owned(), "ployzd-node-node_2.service".to_owned(),],
            vec![
                "restart".to_owned(),
                "ployzd-node-node_2.service".to_owned(),
            ],
            vec!["daemon-reload".to_owned()],
            vec!["enable".to_owned(), "ployzd-gateway.service".to_owned()],
            vec!["restart".to_owned(), "ployzd-gateway.service".to_owned()],
        ]
    );
}

#[test]
fn local_effects_store_redacted_join_material() {
    let root = temp_dir("ployz-keeper-local-join-material");
    let systemd_dir = root.join("systemd");
    fs::create_dir_all(&systemd_dir).expect("systemd dir can be created");
    let material = KeeperJoinMaterial::new(
        node_id("node_2"),
        "prod",
        NatsUserSeed::try_new("SUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("valid nats credentials"),
        test_ca_pem(),
    )
    .expect("valid join material");
    let mut effects = KeeperLocalEffects::new(
        local_config(&root, &systemd_dir),
        RecordingRunner::root_linux(),
    );

    effects
        .apply_step(&KeeperStep::StoreJoinMaterial(material.clone()))
        .expect("join material stores");
    effects
        .apply_step(&KeeperStep::StoreJoinMaterial(material))
        .expect("join material stores idempotently");

    assert_eq!(
        fs::read_to_string(
            root.join("state")
                .join(JOIN_MATERIAL_DIR)
                .join(JOIN_MATERIAL_FILE)
        )
        .expect("join material is stored"),
        format!(
            "node_id=node_2\ncluster_name=prod\nnats_credentials=[redacted]\ntrusted_nats_ca_sha256={}\n",
            ployz_keeper::steps::ca_pem_sha256(test_ca_pem().as_str())
        )
    );
    assert_eq!(
        fs::read_to_string(
            root.join("state")
                .join(JOIN_MATERIAL_DIR)
                .join(JOIN_NATS_CREDENTIALS_FILE),
        )
        .expect("nats credentials are stored"),
        "SUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
    );
    assert_secret_file_mode(
        root.join("state")
            .join(JOIN_MATERIAL_DIR)
            .join(JOIN_NATS_CREDENTIALS_FILE),
    );
}

#[derive(Debug, Default)]
struct RecordingTokenConsumer {
    consumed: usize,
}

impl KeeperJoinTokenConsumer for RecordingTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        self.consumed += 1;
        Ok(())
    }
}

#[derive(Debug)]
struct StaticJoinRedeemer {
    expected_token: JoinToken,
    target: KeeperJoinTarget,
}

impl KeeperJoinRedeemer for StaticJoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedKeeperJoin, FailureMessage> {
        if *token != self.expected_token {
            return Err(failure_message("unexpected join token"));
        }

        Ok(RedeemedKeeperJoin::new(
            operation_id("op_machine"),
            node_id("node_2"),
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

impl KeeperJoinReporter for RecordingJoinReporter {
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
            systemctl_calls: Vec::new(),
            fail_systemctl: None,
            downloads: Vec::new(),
            download_body: None,
            fail_download: None,
        }
    }
}

impl KeeperCommandRunner for RecordingRunner {
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
    events: Vec<KeeperStepEvent>,
}

impl KeeperStepRecorder for RecordingRecorder {
    fn record_step_event(&mut self, event: &KeeperStepEvent) -> Result<(), FailureMessage> {
        self.events.push(event.clone());
        Ok(())
    }
}

fn local_config(root: &Path, systemd_dir: &Path) -> KeeperLocalConfig {
    KeeperLocalConfig {
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
        "server_name: node_1\nhost: 127.0.0.1\nport: 4222\ntls {{\n  cert_file: \"{cert}\"\n  key_file: \"{key}\"\n}}\njetstream {{\n  store_dir: \"{store}\"\n}}\ninclude \"authorized-users.conf\"\n",
        cert = root.join("nats/server.crt").display(),
        key = root.join("nats/server.key").display(),
        store = root.join("nats").display(),
    )
}

fn role_env(root: &Path) -> PloyzdRoleEnvironmentTarget {
    role_env_for_node(root, node_id("node_1"))
}

fn role_env_for_node(root: &Path, node_id: NodeId) -> PloyzdRoleEnvironmentTarget {
    PloyzdRoleEnvironmentTarget::new(
        PloyzdRoleEnvironmentFile::new(root.join("etc/ployzd.env"))
            .expect("valid ployzd role environment target"),
        node_id,
        NatsClientUrl::try_new("tls://127.0.0.1:4222").expect("valid NATS URL"),
        RoleNatsCredentials::cluster(&nats_material(root)),
    )
}

fn edge_runtime_role_env(root: &Path) -> PloyzdRoleEnvironmentTarget {
    PloyzdRoleEnvironmentTarget::new(
        PloyzdRoleEnvironmentFile::new(root.join("etc/ployzd.env"))
            .expect("valid ployzd role environment target"),
        node_id("node_2"),
        NatsClientUrl::loopback(7422),
        RoleNatsCredentials::joined(&root.join("state").join(JOIN_MATERIAL_DIR)),
    )
}

fn first_node_plan_with_ployzd(
    root: &Path,
    ployzd: PloyzdArtifactTarget,
) -> ployz_keeper::steps::KeeperStepPlan {
    let nats_source = root.join("nats-server-source");
    fs::write(&nats_source, "ployz\n").expect("nats source can be written");
    first_node_install_plan(
        FirstNodeInstallTarget::new(
            node_id("node_1"),
            ployzd,
            dataplane_artifacts(root),
            nats_server_artifact(&nats_source, &root.join("bin/nats-server")),
            FirstNodeGateway::Skip,
            test_identity().clone(),
        )
        .with_nats_server_unit(nats_unit(root))
        .with_nats_material_paths(nats_material(root))
        .with_role_environment(role_env(root)),
    )
}

fn remote_ployzd_artifact(url: &str, install_path: &Path) -> PloyzdArtifactTarget {
    PloyzdArtifactTarget::new(
        version("0.1.0"),
        ArtifactSource::try_new(url).expect("valid remote source"),
        digest(PLOYZ_NEWLINE_SHA256),
        install_path.to_path_buf(),
    )
    .expect("valid ployzd artifact")
}

fn ployzd_artifact(source: &Path, install_path: &Path) -> PloyzdArtifactTarget {
    PloyzdArtifactTarget::new(
        version("0.1.0"),
        artifact_source(source),
        digest(PLOYZ_NEWLINE_SHA256),
        install_path.to_path_buf(),
    )
    .expect("valid ployzd artifact")
}

fn nats_server_artifact(source: &Path, install_path: &Path) -> NatsServerArtifactTarget {
    NatsServerArtifactTarget::new(
        version("2.12.0"),
        artifact_source(source),
        digest(PLOYZ_NEWLINE_SHA256),
        install_path.to_path_buf(),
    )
    .expect("valid nats-server artifact")
}

fn ebpf_bytecode_artifact(root: &Path) -> EbpfBytecodeArtifactTarget {
    let source = root.join("ployz-ebpf-tc-source");
    fs::write(&source, "ployz\n").expect("eBPF bytecode source can be written");
    EbpfBytecodeArtifactTarget::new(
        version("0.1.0"),
        artifact_source(&source),
        digest(PLOYZ_NEWLINE_SHA256),
        root.join("lib/ployz/ebpf/ployz-ebpf-tc"),
    )
    .expect("valid eBPF bytecode artifact")
}

fn ebpf_ctl_artifact(root: &Path) -> EbpfCtlArtifactTarget {
    let source = root.join("ployz-ebpf-ctl-source");
    fs::write(&source, "ployz\n").expect("eBPF ctl source can be written");
    EbpfCtlArtifactTarget::new(
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

fn version(value: &str) -> ArtifactVersion {
    ArtifactVersion::try_new(value).expect("valid version")
}

fn digest(value: &str) -> Sha256Digest {
    Sha256Digest::try_new(value).expect("valid digest")
}

fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
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

#[cfg(unix)]
fn assert_secret_file_mode(path: PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)
        .expect("secret file metadata is readable")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[cfg(not(unix))]
fn assert_secret_file_mode(_path: PathBuf) {}

const PLOYZ_NEWLINE_SHA256: &str =
    "2dcc3bb1142455239d3b3391d9569a8ce0fbdfb906cd0434329e5dd736592138";
