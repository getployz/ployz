#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::{env, fs};

use ployz_core::ids::NodeId;
use ployz_core::install::NatsMachineMaterialPaths;
use ployz_core::nats_config::{NatsCaCertificatePem, NatsListener, NatsUserSeed};
use ployz_core::ops::FailureMessage;
use ployz_core::roles::{DaemonProcessRole, FirstNodeGateway};
use ployz_keeper::artifacts::{
    ArtifactKind, ArtifactTarget, ArtifactTargetError, DataplaneArtifactTargets, Sha256Digest,
};
use ployz_keeper::cli::{KeeperCommand, load_command};
use ployz_keeper::executor::{
    KeeperPlanFailure, KeeperPlanTerminal, KeeperRecordFailure, KeeperStepEffects, KeeperStepEvent,
    KeeperStepRecorder, execute_keeper_plan,
};
use ployz_keeper::join_executor::{
    KeeperJoinRedeemer, KeeperJoinReporter, KeeperJoinTokenConsumer, RedeemedKeeperJoin,
    execute_keeper_join,
};
use ployz_keeper::nats_identity::{
    ClusterNatsIdentity, ServerCertificateSans, generate_cluster_nats_identity,
};
use ployz_keeper::report::render_step_event;
use ployz_keeper::steps::{
    ContainerRuntime, FirstNodeInstallTarget, HostPrerequisite, JoinMaterialError, JoinToken,
    KeeperJoinMaterial, KeeperJoinTarget, KeeperStep, KeeperStepEffectError, KeeperStepFailure,
    KeeperStepFailureReason, KeeperStepLabel, NonEmptyRoleSet, PloyzdRoleEnvironmentStep,
    PloyzdRoleEnvironmentTarget, RedactedJoinMaterial, RoleNatsCredentials, RoleSetError,
    first_node_install_plan, keeper_join_local_install_plan,
};
use ployz_keeper::systemd::{PloyzdRoleEnvironmentFile, SupervisorUnitTarget};
use ployz_nats::connect::NatsClientUrl;
use ployz_sdk_types::MachineJoinReportFailure;
use ployz_test_support::ids::{failure_message, node_id, operation_id};
use ployz_test_support::keeper::{
    artifact_source as source, artifact_version as version, nats_server_artifact, ployzd_artifact,
    sha256_digest as digest,
};
use std::sync::OnceLock;

#[test]
fn bootstrap_script_file_installs_only_keeper() {
    let script = fs::read_to_string(bootstrap_script_path()).expect("script is readable");

    assert!(script.contains("PLOYZ_KEEPER_URL"));
    assert!(script.contains("PLOYZ_NATS_URL"));
    assert!(script.contains("PLOYZ_JOIN_TOKEN"));
    assert!(script.contains("join-token"));
    assert!(script.contains("\"$keeper_bin\" --join-token-file \"$join_token_file\""));
    assert!(script.contains("not both"));
    assert!(script.contains("--join-token <token>"));
    assert!(script.contains("unknown ployz installer argument"));
    assert!(script.contains("install -d -m 0700"));
    assert!(script.contains("umask 077"));
    assert!(script.contains("uname -s"));
    assert!(script.contains("id -u"));
    assert!(!script.contains("PLOYZ_INSTALL_DIR"));
    assert!(!script.contains("PLOYZ_SYSTEMD_DIR"));
    assert!(!script.contains("PLOYZ_KEEPER_STATE_DIR"));
    assert!(!script.contains("cat > \"$keeper_unit\""));
    assert!(!script.contains("systemctl enable ployz-keeper.service"));
    assert!(!script.contains("systemctl restart ployz-keeper.service"));
    assert!(!script.contains("ExecStart=${keeper_bin}${keeper_args}"));
    assert!(!script.contains("ployzd"));
    assert!(!script.contains("NATS_CREDS"));
}

#[cfg(unix)]
#[test]
fn bootstrap_script_runs_join_directly_and_does_not_start_noop_service() {
    let root = temp_dir("ployz-bootstrap-script-join");
    let keeper_source = root.join("ployz-keeper-source");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    let keeper_args_log = root.join("keeper-args.log");
    let keeper_token_log = root.join("keeper-token.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(
        &keeper_source,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$PLOYZ_TEST_KEEPER_ARGS_LOG\"\ncat \"$2\" > \"$PLOYZ_TEST_KEEPER_TOKEN_LOG\"\nexit \"${PLOYZ_TEST_KEEPER_EXIT:-0}\"\n",
    )
    .expect("fake keeper source can be written");
    write_executable(&fake_bin.join("curl"), fake_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), "#!/bin/sh\nprintf 'Linux\\n'\n");
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--join-token", "join_once"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env(
            "PLOYZ_KEEPER_URL",
            format!("file://{}", keeper_source.display()),
        )
        .env("PLOYZ_KEEPER_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_NATS_URL", "nats://127.0.0.1:4222")
        .env("PLOYZ_TEST_KEEPER_ARGS_LOG", &keeper_args_log)
        .env("PLOYZ_TEST_KEEPER_TOKEN_LOG", &keeper_token_log)
        .output()
        .expect("bootstrap script can run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected_args = format!(
        "--join-token-file {}\n",
        state_dir.join("join-token").display()
    );
    assert_eq!(
        fs::read_to_string(&keeper_args_log).expect("keeper args are recorded"),
        expected_args
    );
    assert_eq!(
        fs::read_to_string(&keeper_token_log).expect("keeper token is recorded"),
        "join_once\n"
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_join_failure_is_not_masked_by_service_start() {
    let root = temp_dir("ployz-bootstrap-script-join-failure");
    let keeper_source = root.join("ployz-keeper-source");
    let script_path = test_bootstrap_script_path(&root, &root.join("bin"), &root.join("state"));
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(&keeper_source, "#!/bin/sh\nexit 42\n").expect("fake keeper source can be written");
    write_executable(&fake_bin.join("curl"), fake_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), "#!/bin/sh\nprintf 'Linux\\n'\n");
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--join-token", "join_once"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env(
            "PLOYZ_KEEPER_URL",
            format!("file://{}", keeper_source.display()),
        )
        .env("PLOYZ_KEEPER_SHA256", KEEPER_DIGEST)
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
}

#[test]
fn bootstrap_script_rejects_positional_join_token() {
    let output = run_bootstrap_script(&["join_once"], None);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "--join-token <token>");
}

#[test]
fn bootstrap_script_rejects_unknown_join_token_flag() {
    let output = run_bootstrap_script(&["--token", "join_once"], None);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "unknown ployz installer argument: --token");
}

#[test]
fn bootstrap_script_rejects_join_token_from_flag_and_env() {
    let output = run_bootstrap_script(&["--join-token", "join_once"], Some("join_env"));

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "set join token as either --join-token or PLOYZ_JOIN_TOKEN, not both",
    );
}

#[test]
fn keeper_startup_reads_join_token_file_without_consuming_it() {
    let token_file = unique_temp_path("ployz-keeper-join-token");
    fs::write(&token_file, "join_once\n").expect("join token file can be written");

    let command = load_command(vec![
        "--join-token-file".into(),
        token_file.as_os_str().to_os_string(),
    ])
    .expect("startup reads join token");
    let KeeperCommand::Start(startup) = command else {
        panic!("expected startup command, got {command:?}");
    };
    let join = startup.join.as_ref().expect("join token is loaded");

    assert_eq!(
        &join.token,
        &JoinToken::try_new("join_once").expect("expected token is valid")
    );
    assert_eq!(join.file, token_file);
    assert_eq!(format!("{:?}", join.token), "JoinToken(\"[redacted]\")");
    assert!(token_file.exists());
    fs::remove_file(token_file).expect("test token file can be removed");
}

#[test]
fn keeper_join_installs_ployzd_and_only_assigned_role_units() {
    let roles = vec![
        DaemonProcessRole::Node(node_id("node_7")),
        DaemonProcessRole::Gateway,
    ];
    let material = keeper_join_material();
    let plan = keeper_join_local_install_plan(KeeperJoinTarget::new(
        material.clone(),
        ployzd_artifact(),
        dataplane_artifacts(),
        NonEmptyRoleSet::try_new(roles.clone()).expect("non-empty unique roles"),
        edge_role_environment(),
    ));

    assert!(installs_artifact_kind(&plan, ArtifactKind::Ployzd));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfBytecode));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfCtl));
    assert!(writes_ployzd_role_units(&plan));
    assert!(
        plan.steps()
            .contains(&KeeperStep::StoreJoinMaterial(material))
    );
    let [
        store_material,
        prepare_runtime,
        verify_runtime,
        install_ployzd,
        ..,
    ] = plan.steps()
    else {
        panic!("join install plan records material and Docker prep before artifacts");
    };
    assert!(matches!(store_material, KeeperStep::StoreJoinMaterial(_)));
    assert_eq!(
        *prepare_runtime,
        KeeperStep::PrepareContainerRuntime(ContainerRuntime::Docker)
    );
    assert_eq!(
        *verify_runtime,
        KeeperStep::VerifyContainerRuntime(ContainerRuntime::Docker)
    );
    assert!(matches!(
        install_ployzd,
        KeeperStep::InstallArtifact(ArtifactTarget {
            kind: ArtifactKind::Ployzd,
            ..
        })
    ));
    let rendered_env =
        edge_role_environment().render_for_role(&DaemonProcessRole::Node(node_id("node_7")));
    assert!(rendered_env.contains("PLOYZ_EBPF_BYTECODE=/usr/local/lib/ployz/ebpf/ployz-ebpf-tc\n"));
    assert!(rendered_env.contains("PLOYZ_EBPF_CTL=/usr/local/bin/ployz-ebpf-ctl\n"));
    assert!(
        rendered_env
            .contains("PLOYZ_NATS_NKEY_SEED_FILE=/var/lib/ployz/join-material.d/nats.creds\n")
    );
    assert!(rendered_env.contains("PLOYZ_NATS_CA_FILE=/var/lib/ployz/join-material.d/ca.pem\n"));

    for role in roles {
        assert!(
            plan.steps()
                .contains(&KeeperStep::WritePloyzdRoleEnvironment(
                    PloyzdRoleEnvironmentStep {
                        role: role.clone(),
                        target: edge_role_environment(),
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
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Control)
    ));
    assert!(!plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Dns)
    ));
}

#[test]
fn first_node_install_starts_nats_and_core_roles_without_join_token() {
    let node_id = node_id("node_1");
    let target = FirstNodeInstallTarget::new(
        node_id.clone(),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        FirstNodeGateway::Skip,
        test_identity().clone(),
    );
    let role_environment = target.role_environment.clone();
    let plan = first_node_install_plan(target);

    assert!(installs_artifact_kind(&plan, ArtifactKind::Ployzd));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfBytecode));
    assert!(installs_artifact_kind(&plan, ArtifactKind::EbpfCtl));
    assert!(installs_artifact_kind(&plan, ArtifactKind::NatsServer));
    assert!(writes_nats_server_unit(&plan));
    assert!(writes_ployzd_role_units(&plan));
    assert!(
        plan.steps()
            .contains(&KeeperStep::WriteNatsServerConfig(first_node_nats_target(
                node_id.clone()
            )))
    );
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

    for role in [DaemonProcessRole::Control, DaemonProcessRole::Node(node_id)] {
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
fn first_node_role_envs_carry_tls_url_and_role_scoped_seed_paths() {
    let target = FirstNodeInstallTarget::new(
        node_id("node_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        FirstNodeGateway::Install,
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

    // Node and gateway point at the fixed node.seed path, which does not
    // exist at install time — there is no controller-seed fallback.
    for role in [
        DaemonProcessRole::Node(node_id("node_1")),
        DaemonProcessRole::Gateway,
    ] {
        let env = target.role_environment.render_for_role(&role);
        assert!(env.starts_with("PLOYZ_NATS_URL=tls://127.0.0.1:4222\n"));
        assert!(env.contains("PLOYZ_NATS_CA_FILE=/var/lib/ployz/nats/ca.pem\n"));
        assert!(env.contains("PLOYZ_NATS_NKEY_SEED_FILE=/var/lib/ployz/nats/node.seed\n"));
        assert!(!env.contains("controller.seed"));
    }

    assert_eq!(
        target
            .role_environment
            .file_for_role(&DaemonProcessRole::Control)
            .path(),
        std::path::Path::new("/etc/ployz/ployzd-control.env")
    );
}

#[test]
fn first_node_public_ip_flips_the_listener_external_in_the_secured_config() {
    let target = FirstNodeInstallTarget::new(
        node_id("node_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        FirstNodeGateway::Skip,
        test_identity().clone(),
    )
    .with_node_public_ip("203.0.113.10".parse().expect("valid IP"));
    let plan = first_node_install_plan(target);

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
            | KeeperStep::WriteSupervisorUnit(_)
            | KeeperStep::StartSupervisorUnit(_)
            | KeeperStep::RestartSupervisorUnit(_)
            | KeeperStep::StoreJoinMaterial(_) => None,
        })
        .expect("first-node plan writes the nats config");

    // TLS + authorization land in the same rendered config that opens the
    // listener — a plaintext external listener is unrepresentable.
    assert!(rendered.contains("host: 0.0.0.0\n"));
    assert!(rendered.contains("client_advertise: 203.0.113.10:4222\n"));
    assert!(rendered.contains("tls {\n"));
    assert!(rendered.contains("include \"authorized-users.conf\"\n"));
}

#[test]
fn first_node_install_can_include_gateway_role() {
    let plan = first_node_install_plan(FirstNodeInstallTarget::new(
        node_id("node_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        FirstNodeGateway::Install,
        test_identity().clone(),
    ));

    assert!(plan_writes_unit(
        &plan,
        &SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Gateway)
    ));
    assert!(plan.steps().contains(&KeeperStep::StartSupervisorUnit(
        SupervisorUnitTarget::PloyzdRole(DaemonProcessRole::Gateway)
    )));
}

#[test]
fn role_sets_reject_empty_and_duplicate_assignments() {
    assert_eq!(NonEmptyRoleSet::try_new(vec![]), Err(RoleSetError::Empty));
    assert_eq!(
        NonEmptyRoleSet::try_new(vec![DaemonProcessRole::Gateway, DaemonProcessRole::Gateway]),
        Err(RoleSetError::Duplicate {
            role: DaemonProcessRole::Gateway,
        })
    );
}

#[test]
fn join_material_cluster_name_rejects_persisted_format_breakers() {
    for value in ["prod\nnext", "prod\rnext"] {
        assert_eq!(
            RedactedJoinMaterial::new(node_id("node_7"), value, NATS_CA_DIGEST),
            Err(JoinMaterialError::InvalidJoinMaterialValue {
                label: "cluster name",
                value: value.to_owned(),
            })
        );
    }
}

#[test]
fn join_material_rejects_persisted_line_breakers() {
    for value in ["cccc\nnext", "cccc\rnext"] {
        assert_eq!(
            RedactedJoinMaterial::new(node_id("node_7"), "prod", value),
            Err(JoinMaterialError::InvalidJoinMaterialValue {
                label: "trusted NATS CA digest",
                value: value.to_owned(),
            })
        );
    }
}

#[test]
fn keeper_step_failure_is_bootstrap_scoped_and_typed() {
    let step = KeeperStep::StartSupervisorUnit(SupervisorUnitTarget::NatsServer);
    assert_eq!(
        KeeperStepFailure::from_step(&step, failure_message("simulated supervisor start failure")),
        KeeperStepFailure {
            step: KeeperStepLabel::StartSupervisorUnit(SupervisorUnitTarget::NatsServer),
            reason: KeeperStepFailureReason::SupervisorStartFailed,
            message: failure_message("simulated supervisor start failure"),
        },
    );
}

#[test]
fn keeper_plan_executor_runs_steps_in_order_and_records_progress() {
    let plan = first_node_install_plan(FirstNodeInstallTarget::new(
        node_id("node_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        FirstNodeGateway::Skip,
        test_identity().clone(),
    ));
    let mut effects = RecordingEffects::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);
    let rendered = format!("{execution:?}");

    assert_eq!(execution.terminal, KeeperPlanTerminal::Completed);
    assert!(rendered.contains("/usr/local/bin/ployzd"));
    assert!(rendered.contains(PLOYZD_DIGEST));
    assert_eq!(effects.calls.len(), plan.steps().len());
    assert_eq!(recorder.events, execution.events);
    let [first, second, third, fourth, ..] = effects.calls.as_slice() else {
        panic!("plan records at least four calls");
    };
    assert_eq!(
        *first,
        KeeperStepLabel::VerifyHost(HostPrerequisite::LinuxRootSystemd)
    );
    assert_eq!(
        *second,
        KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker)
    );
    assert_eq!(
        *third,
        KeeperStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker)
    );
    assert_eq!(*fourth, KeeperStepLabel::InstallArtifact(ployzd_artifact()));
    let [
        _,
        _,
        _,
        _,
        fifth,
        sixth,
        seventh,
        eighth,
        ninth,
        tenth,
        eleventh,
        ..,
    ] = effects.calls.as_slice()
    else {
        panic!("first-node plan records nats setup calls");
    };
    assert_eq!(
        *fifth,
        KeeperStepLabel::InstallArtifact(ebpf_bytecode_artifact())
    );
    assert_eq!(
        *sixth,
        KeeperStepLabel::InstallArtifact(ebpf_ctl_artifact())
    );
    assert_eq!(
        *seventh,
        KeeperStepLabel::InstallArtifact(nats_server_artifact())
    );
    assert_eq!(
        *eighth,
        KeeperStepLabel::WriteNatsTlsMaterial {
            state_dir: PathBuf::from("/var/lib/ployz/nats"),
        }
    );
    assert_eq!(
        *ninth,
        KeeperStepLabel::WriteNatsAuthorizedUsers {
            path: PathBuf::from("/etc/nats/authorized-users.conf"),
        }
    );
    assert_eq!(
        *tenth,
        KeeperStepLabel::WriteNatsClientCredentials {
            state_dir: PathBuf::from("/var/lib/ployz/nats"),
        }
    );
    assert_eq!(
        *eleventh,
        KeeperStepLabel::WriteNatsServerConfig(first_node_nats_target(node_id("node_1")))
    );
    assert_eq!(
        execution.events,
        plan.steps()
            .iter()
            .flat_map(|step| {
                let label = KeeperStepLabel::from_step(step);
                [
                    KeeperStepEvent::Started {
                        step: label.clone(),
                    },
                    KeeperStepEvent::Succeeded { step: label },
                ]
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn keeper_plan_executor_stops_on_first_failed_step() {
    let plan = first_node_plan();
    let ployzd_target = ployzd_artifact();
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::InstallArtifact(ployzd_target.clone())),
        fail_message: "simulated artifact install failure",
        ..RecordingEffects::default()
    };
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);
    let failure = KeeperStepFailure {
        step: KeeperStepLabel::InstallArtifact(ployzd_target.clone()),
        reason: KeeperStepFailureReason::ArtifactInstallFailed,
        message: failure_message("simulated artifact install failure"),
    };

    assert_eq!(
        execution.terminal,
        KeeperPlanTerminal::Failed(Box::new(KeeperPlanFailure::Step(failure.clone())))
    );
    assert_eq!(
        effects.calls,
        vec![
            KeeperStepLabel::VerifyHost(HostPrerequisite::LinuxRootSystemd),
            KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
            KeeperStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker),
            KeeperStepLabel::InstallArtifact(ployzd_target),
        ]
    );
    assert_eq!(
        execution.events.last(),
        Some(&KeeperStepEvent::Failed(failure))
    );
    assert_eq!(recorder.events, execution.events);
}

#[test]
fn keeper_progress_renders_container_runtime_steps() {
    assert_eq!(
        render_step_event(&KeeperStepEvent::Started {
            step: KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
        }),
        "started prepare-container-runtime docker"
    );
    assert_eq!(
        render_step_event(&KeeperStepEvent::Failed(KeeperStepFailure {
            step: KeeperStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker),
            reason: KeeperStepFailureReason::ContainerRuntimeVerifyFailed,
            message: failure_message("docker info failed"),
        })),
        "failed verify-container-runtime docker container-runtime-verify-failed: docker info failed"
    );
}

#[test]
fn keeper_join_executor_redacts_join_token_from_progress() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer {
        fail_message: Some("simulated join token redeem failure"),
        ..RecordingJoinRedeemer::default()
    };
    let mut effects = RecordingEffects {
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );
    let rendered = format!("{execution:?}");

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::RedeemJoinToken,
            reason: KeeperStepFailureReason::JoinTokenRedeemFailed,
            message,
        }))
            if message.as_str() == "simulated join token redeem failure"
    ));
    assert!(!rendered.contains("join_secret"));
    assert_eq!(
        redeemer.redeemed_tokens,
        vec![JoinToken::try_new("join_secret").expect("valid join token")]
    );
    assert_eq!(token_consumer.consumed, 0);
    assert!(reporter.reports.is_empty());
}

#[test]
fn keeper_join_keeps_token_when_material_store_fails() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let material = keeper_join_material();
    let redacted = material.redacted();
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::StoreJoinMaterial(redacted.clone())),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::StoreJoinMaterial(failed_material),
            reason: KeeperStepFailureReason::JoinMaterialStoreFailed,
            ..
        })) if *failed_material == redacted
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated keeper step failure"),
            },
        }]
    );
}

#[test]
fn keeper_join_keeps_token_when_install_fails_after_redemption() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::InstallArtifact(ployzd_artifact())),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::InstallArtifact(ArtifactTarget {
                kind: ArtifactKind::Ployzd,
                ..
            }),
            reason: KeeperStepFailureReason::ArtifactInstallFailed,
            ..
        }))
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated keeper step failure"),
            },
        }]
    );
}

#[test]
fn keeper_join_reports_docker_prepare_failure_after_redemption() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut effects = RecordingEffects {
        fail_on: Some(KeeperStepLabel::PrepareContainerRuntime(
            ContainerRuntime::Docker,
        )),
        ..RecordingEffects::default()
    };
    let mut reporter = RecordingJoinReporter::default();
    let mut token_consumer = RecordingTokenConsumer::default();
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
            reason: KeeperStepFailureReason::ContainerRuntimePrepareFailed,
            ..
        }))
    ));
    assert_eq!(token_consumer.consumed, 0);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated keeper step failure"),
            },
        }]
    );
}

#[test]
fn keeper_join_does_not_report_completed_when_token_consume_fails() {
    let token = JoinToken::try_new("join_secret").expect("valid join token");
    let mut redeemer = RecordingJoinRedeemer::default();
    let mut reporter = RecordingJoinReporter::default();
    let mut effects = RecordingEffects {
        ..RecordingEffects::default()
    };
    let mut token_consumer = RecordingTokenConsumer {
        fail_message: Some("simulated token cleanup failure"),
        ..RecordingTokenConsumer::default()
    };
    let mut recorder = RecordingRecorder::default();

    let execution = execute_keeper_join(
        &token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );

    assert!(matches!(
        execution.terminal.failure(),
        Some(KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::ConsumeJoinTokenFile,
            reason: KeeperStepFailureReason::JoinTokenConsumeFailed,
            ..
        }))
    ));
    assert_eq!(token_consumer.consumed, 1);
    assert_eq!(
        reporter.reports,
        vec![JoinReport::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed {
                message: failure_message("simulated token cleanup failure"),
            },
        }]
    );
}

#[test]
fn keeper_plan_executor_records_started_before_applying_step() {
    let plan = first_node_plan();
    let failed_event = KeeperStepEvent::Started {
        step: KeeperStepLabel::InstallArtifact(ployzd_artifact()),
    };
    let mut effects = RecordingEffects::default();
    let mut recorder = RecordingRecorder {
        fail_on: Some(failed_event.clone()),
        fail_message: "simulated event recorder failure",
        ..RecordingRecorder::default()
    };

    let execution = execute_keeper_plan(&plan, &mut effects, &mut recorder);

    assert_eq!(
        execution.terminal,
        KeeperPlanTerminal::Failed(Box::new(KeeperPlanFailure::Record(KeeperRecordFailure {
            event: failed_event,
            message: failure_message("simulated event recorder failure"),
        })))
    );
    assert_eq!(
        effects.calls,
        vec![
            KeeperStepLabel::VerifyHost(HostPrerequisite::LinuxRootSystemd),
            KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker),
            KeeperStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker),
        ]
    );
    assert_eq!(execution.events, recorder.events);
}

#[test]
fn artifact_digest_must_be_sha256_hex() {
    assert_eq!(
        Sha256Digest::try_new("sha256:keeper"),
        Err(ArtifactTargetError::InvalidSha256Digest {
            value: "sha256:keeper".to_owned()
        })
    );
    assert!(Sha256Digest::try_new(KEEPER_DIGEST).is_ok());
}

#[test]
fn artifact_install_paths_must_be_absolute() {
    assert_eq!(
        ArtifactTarget::new(
            ArtifactKind::Ployzd,
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::new(),
        ),
        Err(ArtifactTargetError::EmptyInstallPath)
    );
    assert_eq!(
        ArtifactTarget::new(
            ArtifactKind::Ployzd,
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::from("bin/ployzd"),
        ),
        Err(ArtifactTargetError::RelativeInstallPath {
            value: PathBuf::from("bin/ployzd"),
        })
    );
    assert_eq!(
        ArtifactTarget::new(
            ArtifactKind::Ployzd,
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::from("/"),
        ),
        Err(ArtifactTargetError::MissingInstallParent {
            value: PathBuf::from("/"),
        })
    );
    assert_eq!(
        ArtifactTarget::new(
            ArtifactKind::Ployzd,
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::from("/usr/local/bin/"),
        ),
        Err(ArtifactTargetError::MissingInstallFileName {
            value: PathBuf::from("/usr/local/bin/"),
        })
    );
}

struct RecordingEffects {
    calls: Vec<KeeperStepLabel>,
    fail_on: Option<KeeperStepLabel>,
    fail_message: &'static str,
}

impl RecordingEffects {
    fn record(&mut self, label: KeeperStepLabel) -> Result<(), KeeperStepEffectError> {
        self.calls.push(label.clone());
        if self.fail_on.as_ref() == Some(&label) {
            return Err(failure_message(self.fail_message).into());
        }
        Ok(())
    }
}

impl KeeperStepEffects for RecordingEffects {
    fn apply_step(&mut self, step: &KeeperStep) -> Result<(), KeeperStepEffectError> {
        self.record(KeeperStepLabel::from_step(step))
    }
}

#[derive(Default)]
struct RecordingJoinRedeemer {
    redeemed_tokens: Vec<JoinToken>,
    fail_message: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JoinReport {
    Completed,
    Failed { failure: MachineJoinReportFailure },
}

#[derive(Default)]
struct RecordingJoinReporter {
    reports: Vec<JoinReport>,
    fail_message: Option<&'static str>,
}

#[derive(Default)]
struct RecordingTokenConsumer {
    consumed: usize,
    fail_message: Option<&'static str>,
}

impl KeeperJoinTokenConsumer for RecordingTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        self.consumed += 1;
        if let Some(message) = self.fail_message {
            return Err(failure_message(message));
        }

        Ok(())
    }
}

impl KeeperJoinRedeemer for RecordingJoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedKeeperJoin, FailureMessage> {
        self.redeemed_tokens.push(token.clone());
        if let Some(message) = self.fail_message {
            return Err(failure_message(message));
        }

        Ok(RedeemedKeeperJoin::new(
            operation_id("op_machine"),
            node_id("node_7"),
            KeeperJoinTarget::new(
                keeper_join_material(),
                ployzd_artifact(),
                dataplane_artifacts(),
                NonEmptyRoleSet::try_new(vec![DaemonProcessRole::Node(node_id("node_7"))])
                    .expect("non-empty role set"),
                role_environment(),
            ),
        ))
    }
}

fn keeper_join_material() -> KeeperJoinMaterial {
    KeeperJoinMaterial::new(
        node_id("node_7"),
        "prod",
        NatsUserSeed::try_new("SUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("valid nats credentials"),
        test_ca_pem(),
    )
    .expect("valid join material")
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

impl KeeperJoinReporter for RecordingJoinReporter {
    fn report_join_completed(&mut self) -> Result<(), FailureMessage> {
        self.reports.push(JoinReport::Completed);
        if let Some(message) = self.fail_message {
            return Err(failure_message(message));
        }

        Ok(())
    }

    fn report_join_failed(
        &mut self,
        failure: MachineJoinReportFailure,
    ) -> Result<(), FailureMessage> {
        self.reports.push(JoinReport::Failed { failure });
        if let Some(message) = self.fail_message {
            return Err(failure_message(message));
        }

        Ok(())
    }
}

#[derive(Default)]
struct RecordingRecorder {
    events: Vec<KeeperStepEvent>,
    fail_on: Option<KeeperStepEvent>,
    fail_message: &'static str,
}

impl KeeperStepRecorder for RecordingRecorder {
    fn record_step_event(&mut self, event: &KeeperStepEvent) -> Result<(), FailureMessage> {
        if self.fail_on.as_ref() == Some(event) {
            return Err(failure_message(self.fail_message));
        }
        self.events.push(event.clone());
        Ok(())
    }
}

impl Default for RecordingEffects {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            fail_on: None,
            fail_message: "simulated keeper step failure",
        }
    }
}

fn ebpf_bytecode_artifact() -> ArtifactTarget {
    ArtifactTarget::new(
        ArtifactKind::EbpfBytecode,
        version("0.1.0"),
        source("/tmp/ployz-ebpf-tc"),
        digest(KEEPER_DIGEST),
        PathBuf::from("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"),
    )
    .expect("valid eBPF bytecode artifact")
}

fn ebpf_ctl_artifact() -> ArtifactTarget {
    ArtifactTarget::new(
        ArtifactKind::EbpfCtl,
        version("0.1.0"),
        source("/tmp/ployz-ebpf-ctl"),
        digest(KEEPER_DIGEST),
        PathBuf::from("/usr/local/bin/ployz-ebpf-ctl"),
    )
    .expect("valid eBPF ctl artifact")
}

fn dataplane_artifacts() -> DataplaneArtifactTargets {
    DataplaneArtifactTargets::new(ebpf_bytecode_artifact(), ebpf_ctl_artifact())
}

fn role_environment() -> PloyzdRoleEnvironmentTarget {
    PloyzdRoleEnvironmentTarget::new(
        PloyzdRoleEnvironmentFile::new(PathBuf::from("/etc/ployz/ployzd.env"))
            .expect("valid role environment path"),
        node_id("node_1"),
        NatsClientUrl::try_new("nats://127.0.0.1:4222").expect("valid NATS URL"),
        RoleNatsCredentials::joined(std::path::Path::new("/var/lib/ployz/join-material.d")),
    )
    .with_ebpf_bytecode_path(PathBuf::from("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"))
    .with_ebpf_ctl_path(PathBuf::from("/usr/local/bin/ployz-ebpf-ctl"))
}

fn edge_role_environment() -> PloyzdRoleEnvironmentTarget {
    PloyzdRoleEnvironmentTarget::new(
        PloyzdRoleEnvironmentFile::new(PathBuf::from("/etc/ployz/ployzd.env"))
            .expect("valid role environment path"),
        node_id("node_7"),
        NatsClientUrl::try_new("nats://127.0.0.1:7422").expect("valid NATS URL"),
        RoleNatsCredentials::joined(std::path::Path::new("/var/lib/ployz/join-material.d")),
    )
    .with_ebpf_bytecode_path(PathBuf::from("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"))
    .with_ebpf_ctl_path(PathBuf::from("/usr/local/bin/ployz-ebpf-ctl"))
}

fn first_node_plan() -> ployz_keeper::steps::KeeperStepPlan {
    first_node_install_plan(FirstNodeInstallTarget::new(
        node_id("node_1"),
        ployzd_artifact(),
        dataplane_artifacts(),
        nats_server_artifact(),
        FirstNodeGateway::Skip,
        test_identity().clone(),
    ))
}

fn installs_artifact_kind(plan: &ployz_keeper::steps::KeeperStepPlan, kind: ArtifactKind) -> bool {
    plan.steps()
        .iter()
        .any(|step| matches!(step, KeeperStep::InstallArtifact(artifact) if artifact.kind == kind))
}

fn writes_ployzd_role_units(plan: &ployz_keeper::steps::KeeperStepPlan) -> bool {
    plan.steps().iter().any(|step| {
        matches!(
            step,
            KeeperStep::WriteSupervisorUnit(spec)
                if matches!(spec.target(), SupervisorUnitTarget::PloyzdRole(_))
        )
    })
}

fn writes_nats_server_unit(plan: &ployz_keeper::steps::KeeperStepPlan) -> bool {
    plan_writes_unit(plan, &SupervisorUnitTarget::NatsServer)
}

fn plan_writes_unit(
    plan: &ployz_keeper::steps::KeeperStepPlan,
    target: &SupervisorUnitTarget,
) -> bool {
    plan.steps().iter().any(
        |step| matches!(step, KeeperStep::WriteSupervisorUnit(spec) if spec.target() == *target),
    )
}

fn first_node_nats_target(node_id: NodeId) -> ployz_keeper::steps::NatsServerConfigTarget {
    ployz_keeper::steps::NatsServerConfigTarget::for_first_node(
        node_id,
        &ployz_keeper::systemd::NatsServerUnitTarget::default_paths(),
        &NatsMachineMaterialPaths::in_default_state_dir(),
        NatsListener::Loopback,
    )
}

fn unique_temp_path(prefix: &str) -> PathBuf {
    let unique = format!(
        "{}-{}",
        prefix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

fn temp_dir(prefix: &str) -> PathBuf {
    let path = unique_temp_path(prefix);
    fs::create_dir_all(&path).expect("temp dir can be created");
    path
}

#[cfg(unix)]
fn write_executable(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).expect("executable can be written");
    let mut permissions = fs::metadata(path)
        .expect("executable metadata is readable")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("executable permissions can be set");
}

fn fake_curl_script() -> &'static str {
    "#!/bin/sh\nurl=\ndest=\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o)\n      dest=\"$2\"\n      shift 2\n      ;;\n    -*)\n      shift\n      ;;\n    *)\n      url=\"$1\"\n      shift\n      ;;\n  esac\ndone\ncase \"$url\" in\n  file://*) cp \"${url#file://}\" \"$dest\" ;;\n  *) exit 2 ;;\nesac\n"
}

fn bootstrap_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("scripts")
        .join("ployz.sh")
}

fn test_bootstrap_script_path(
    root: &std::path::Path,
    install_dir: &std::path::Path,
    state_dir: &std::path::Path,
) -> PathBuf {
    let source = fs::read_to_string(bootstrap_script_path()).expect("bootstrap script is readable");
    let rewritten = source
        .replace(
            "install_dir=\"/usr/local/bin\"",
            &format!("install_dir=\"{}\"", install_dir.display()),
        )
        .replace(
            "state_dir=\"/var/lib/ployz/keeper\"",
            &format!("state_dir=\"{}\"", state_dir.display()),
        );
    let path = root.join("ployz.sh");
    fs::write(&path, rewritten).expect("test bootstrap script can be written");
    path
}

fn run_bootstrap_script(args: &[&str], join_token_env: Option<&str>) -> Output {
    let mut command = Command::new("sh");
    command
        .arg(bootstrap_script_path())
        .args(args)
        .env("PLOYZ_KEEPER_URL", "https://example.invalid/ployz-keeper")
        .env("PLOYZ_KEEPER_SHA256", KEEPER_DIGEST);

    match join_token_env {
        Some(token) => {
            command.env("PLOYZ_JOIN_TOKEN", token);
        }
        None => {
            command.env_remove("PLOYZ_JOIN_TOKEN");
        }
    }

    command.output().expect("bootstrap script can run")
}

fn assert_stderr_contains(output: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "stderr should contain {expected:?}, got {stderr:?}"
    );
}

const KEEPER_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PLOYZD_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const NATS_CA_DIGEST: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
