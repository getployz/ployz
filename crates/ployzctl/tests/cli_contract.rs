use ployz_test_support::fs::make_executable;
use std::ffi::OsStr;
use std::fs;
use std::process::{Command, Output};

use ployz_core::dataplane::{
    EbpfForwardingReady, EbpfForwardingReadyEvidence, PloyzNativeMeshMachineReady,
    PloyzNativeMeshPrepareReport, PloyzNativeMeshReady, WireGuardPublicKey, WireGuardReady,
    WireGuardReadyEvidence,
};
use ployz_core::deploy::{DeployRequest, DeployServiceSpec, ImageReference, ReplicaCount};
use ployz_core::ids::{ContainerId, MachineId, NamespaceId, RevisionId, ServiceId};
use ployz_core::ops::{
    DeployOperationState, DeployRunningStage, MAX_OPERATION_EVENT_REPLAY_LIMIT,
    OperationEventReplayLimit, OperationStatus, OperationStatusSnapshot, ReplayedOperationEvent,
};
use ployz_core::state::ActiveServiceState;
use ployz_sdk_types::{LogsTailLines, ServiceSnapshot};
use ployz_test_support::ids::{event_sequence, machine_id, operation_id};
use ployzctl::commands::init::{
    FirstMachineInitOutput, InstallRolePolicy, plan_first_machine_process_set,
};
use ployzctl::commands::machine::MachineName;
use ployzctl::commands::ops::{OpsWatchOutput, StatusOutput, WatchOutput};
use ployzctl::commands::service::{ServiceInspectOutput, ServiceListOutput};
use ployzctl::commands::{PloyzctlCliError, PloyzctlCommand, parse_command, parse_invocation};

#[test]
fn init_first_machine_reports_supervised_product_roles() {
    let machine_id = MachineId::try_new("machine_1").expect("valid machine id");
    let roles = InstallRolePolicy::install_all().without_gateway();
    let output = FirstMachineInitOutput::summary(machine_id.clone(), roles).render();

    assert_eq!(
        output,
        "init first machine machine_1\nsupervise nats-server\nsupervise roles control machine dns\n"
    );
    assert_eq!(
        plan_first_machine_process_set(&machine_id, roles).roles(),
        &[
            ployz_core::roles::DaemonProcessRole::Control,
            ployz_core::roles::DaemonProcessRole::Machine(machine_id),
            ployz_core::roles::DaemonProcessRole::Dns,
        ]
    );
}

#[test]
fn cli_init_activate_first_machine_is_explicit_subcommand() {
    let command = parse_command(
        ["init", "activate-first-machine", "--machine", "machine_1"].map(str::to_owned),
    )
    .expect("activation-only init command parses");

    let PloyzctlCommand::InitFirstMachineActivate(command) = command else {
        panic!("expected first-machine activation command");
    };

    assert_eq!(command.machine_id, machine_id("machine_1"));
    assert_eq!(command.roles, InstallRolePolicy::install_all());
}

#[test]
fn cli_init_activate_first_machine_accepts_role_opt_outs() {
    let command = parse_command(
        [
            "init",
            "activate-first-machine",
            "--machine",
            "machine_1",
            "--no-gateway",
            "--no-dns",
        ]
        .map(str::to_owned),
    )
    .expect("activation-only init command parses");

    let PloyzctlCommand::InitFirstMachineActivate(command) = command else {
        panic!("expected first-machine activation command");
    };

    assert_eq!(
        command.roles,
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns()
    );
}

#[test]
fn cli_init_can_emit_keeper_first_machine_install_command() {
    let command = parse_command(init_with_keeper_install_args()).expect("init command parses");

    let PloyzctlCommand::Init(command) = command else {
        panic!("expected init command");
    };

    assert_eq!(command.machine_id(), &machine_id("machine_1"));
    assert_eq!(command.roles(), InstallRolePolicy::install_all());
    let rendered = command.render();
    assert!(rendered.contains("install ployz-keeper first-machine-install --spec -\n"));
    assert!(rendered.contains(r#""machine_id": "machine_1""#));
    assert!(rendered.contains(r#""gateway": "install""#));
    assert!(
        rendered
            .contains(r#""machine_join_template_file": "/etc/ployz/machine-join-template.json""#)
    );
}

#[test]
fn cli_init_can_pass_first_machine_public_ip_to_keeper_install() {
    let command =
        parse_command(init_with_keeper_install_args_with_public_ip()).expect("init command parses");

    let PloyzctlCommand::Init(command) = command else {
        panic!("expected init command");
    };

    assert!(
        command
            .render()
            .contains(r#""machine_public_ip": "203.0.113.10""#)
    );
}

#[test]
fn cli_init_requires_complete_keeper_install_inputs() {
    assert!(parse_command(["init", "--emit-keeper-install"].map(str::to_owned)).is_err());
}

#[test]
fn cli_init_requires_explicit_keeper_install_mode() {
    let spec = write_first_machine_install_spec(None);
    assert!(
        parse_command(
            [
                "init",
                "--install-spec",
                spec.to_str().expect("spec path is utf-8"),
            ]
            .map(str::to_owned)
        )
        .is_err()
    );
}

#[test]
fn cli_init_validates_keeper_install_inputs_before_rendering() {
    let spec = write_first_machine_install_spec_with_source("relative/ployzd", None);
    assert!(matches!(
        parse_command(
            [
                "init",
                "--emit-keeper-install",
                "--install-spec",
                spec.to_str().expect("spec path is utf-8"),
            ]
            .map(str::to_owned)
        ),
        Err(PloyzctlCliError::InvalidValue { flag, .. })
            if flag == "--install-spec"
    ));
}

#[test]
fn cli_dispatches_init_first_machine() {
    let command = parse_command(["init", "--machine", "machine_1"].map(str::to_owned))
        .expect("init command parses");

    let PloyzctlCommand::Init(command) = command else {
        panic!("expected init command");
    };
    assert_eq!(
        command.render(),
        "init first machine machine_1\nsupervise nats-server\nsupervise roles control machine gateway dns\n"
    );
}

#[test]
fn cli_rejects_init_without_machine() {
    assert!(parse_command(["init"].map(str::to_owned)).is_err());
}

#[test]
fn cli_rejects_option_like_init_machine_values() {
    assert!(parse_command(["init", "--machine", "--no-gateway"].map(str::to_owned)).is_err());
    assert!(parse_command(["init", "--machine", "--help"].map(str::to_owned)).is_err());
}

#[test]
fn cli_renders_help_for_no_args() {
    let error = parse_command(std::iter::empty::<String>()).expect_err("no args requests help");
    assert!(error.is_help_requested());
    assert!(error.to_string().contains("Usage: ployzctl"));
}

#[test]
fn cli_dispatches_ops_watch_request() {
    let command =
        parse_command(["ops", "watch", "op_deploy"].map(str::to_owned)).expect("ops watch parses");

    let PloyzctlCommand::OpsWatch(command) = command else {
        panic!("expected ops watch command");
    };
    assert_eq!(command.output, OpsWatchOutput::Text);
    let request = command.into_request();

    assert_eq!(request.operation_id, operation_id("op_deploy"));
    assert_eq!(request.start_sequence, event_sequence(1));
    assert_eq!(
        request.limit,
        OperationEventReplayLimit::try_new(MAX_OPERATION_EVENT_REPLAY_LIMIT)
            .expect("valid replay limit")
    );
}

#[test]
fn cli_dispatches_ops_watch_json_request() {
    let command = parse_command(["ops", "watch", "--json", "op_deploy"].map(str::to_owned))
        .expect("ops watch json parses");

    let PloyzctlCommand::OpsWatch(command) = command else {
        panic!("expected ops watch command");
    };

    assert_eq!(command.operation_id, operation_id("op_deploy"));
    assert_eq!(command.output, OpsWatchOutput::Json);
}

#[test]
fn cli_dispatches_ops_status_request() {
    let command = parse_command(["ops", "status", "op_deploy"].map(str::to_owned))
        .expect("ops status parses");

    let PloyzctlCommand::OpsStatus(command) = command else {
        panic!("expected ops status command");
    };

    assert_eq!(
        command.into_request().operation_id,
        operation_id("op_deploy")
    );
}

#[test]
fn cli_requires_ops_status_operation_id() {
    assert!(parse_command(["ops", "status"].map(str::to_owned)).is_err());
}

#[test]
fn cli_requires_ops_watch_operation_id() {
    assert!(parse_command(["ops", "watch"].map(str::to_owned)).is_err());
}

#[test]
fn cli_dispatches_backup_create_request() {
    let command = parse_command(
        [
            "backup",
            "create",
            "--operation",
            "op_backup",
            "--s3-bucket",
            "ployz-backups",
            "--s3-prefix",
            "clusters/dev",
            "--s3-region",
            "us-east-1",
        ]
        .map(str::to_owned),
    )
    .expect("backup create parses");

    let PloyzctlCommand::BackupCreate(command) = command else {
        panic!("expected backup create command");
    };
    let request = command.into_request();

    assert_eq!(request.operation_id, operation_id("op_backup"));
    assert_eq!(request.target, backup_target("clusters/dev"));
}

#[test]
fn cli_dispatches_backup_restore_plan() {
    let command = parse_command(
        [
            "backup",
            "restore",
            "--plan",
            "--s3-bucket",
            "ployz-backups",
            "--s3-manifest-key",
            "clusters/dev/op_backup/manifest.json",
            "--s3-region",
            "us-east-1",
            "--s3-addressing-style",
            "path",
        ]
        .map(str::to_owned),
    )
    .expect("backup restore plan parses");

    let PloyzctlCommand::BackupRestorePlan(command) = command else {
        panic!("expected backup restore plan command");
    };
    assert_eq!(
        command.source,
        ployz_core::backup::BackupRestoreSource::S3 {
            bucket: "ployz-backups".to_owned(),
            manifest_key: "clusters/dev/op_backup/manifest.json".to_owned(),
            region: "us-east-1".to_owned(),
            endpoint_url: None,
            addressing_style: ployz_core::backup::S3AddressingStyle::Path,
        }
    );
}

#[test]
fn cli_requires_backup_restore_plan_flag() {
    assert!(parse_command(["backup", "restore"].map(str::to_owned)).is_err());
}

#[test]
fn cli_dispatches_machine_add_remote_request() {
    let command = parse_command(["machine", "add", "root@203.0.113.11"].map(str::to_owned))
        .expect("machine add command parses");

    let PloyzctlCommand::MachineAddRemote(command) = command else {
        panic!("expected remote machine add command");
    };
    assert_eq!(command.target.user(), "root");
    assert_eq!(command.target.host(), "203.0.113.11");
    assert_eq!(command.roles, InstallRolePolicy::install_all());
}

#[test]
fn cli_dispatches_machine_list_request() {
    let command =
        parse_command(["machine", "list"].map(str::to_owned)).expect("machine list command parses");

    let PloyzctlCommand::MachineList(command) = command else {
        panic!("expected machine list command");
    };

    assert_eq!(
        command.into_request(),
        ployz_sdk_types::MachineListRequest {}
    );
}

#[test]
fn cli_dispatches_machine_inspect_request() {
    let command = parse_command(["machine", "inspect", "machine_2"].map(str::to_owned))
        .expect("machine inspect command parses");

    let PloyzctlCommand::MachineInspect(command) = command else {
        panic!("expected machine inspect command");
    };

    assert_eq!(command.into_request().machine_id, machine_id("machine_2"));
}

#[test]
fn cli_dispatches_service_list_request() {
    let command =
        parse_command(["service", "list"].map(str::to_owned)).expect("service list command parses");

    let PloyzctlCommand::ServiceList(command) = command else {
        panic!("expected service list command");
    };

    assert_eq!(
        command.into_request(),
        ployz_sdk_types::ServiceListRequest {}
    );
}

#[test]
fn cli_dispatches_service_inspect_request() {
    let command = parse_command(["service", "inspect", "svc_api"].map(str::to_owned))
        .expect("service inspect command parses");

    let PloyzctlCommand::ServiceInspect(command) = command else {
        panic!("expected service inspect command");
    };

    assert_eq!(
        command.into_request().service_id,
        ServiceId::try_new("svc_api").expect("valid service id")
    );
}

#[test]
fn cli_dispatches_logs_tail_request() {
    let command = parse_command(
        [
            "logs",
            "ctr_failed",
            "--machine",
            "machine_a",
            "--tail",
            "50",
        ]
        .map(str::to_owned),
    )
    .expect("logs tail command parses");

    let PloyzctlCommand::LogsTail(command) = command else {
        panic!("expected logs tail command");
    };

    assert_eq!(
        command.into_request(),
        ployz_sdk_types::LogsTailRequest {
            container_id: ContainerId::try_new("ctr_failed").expect("valid container id"),
            machine_id: Some(machine_id("machine_a")),
            tail_lines: Some(LogsTailLines::try_new(50).expect("valid logs tail lines")),
        }
    );
}

#[test]
fn cli_requires_service_inspect_service_id() {
    assert!(parse_command(["service", "inspect"].map(str::to_owned)).is_err());
}

#[test]
fn cli_requires_machine_inspect_machine_id() {
    assert!(parse_command(["machine", "inspect"].map(str::to_owned)).is_err());
}

#[test]
fn cli_parses_global_nats_url() {
    let invocation = parse_invocation(
        ["--nats", "nats://127.0.0.1:4222"]
            .into_iter()
            .map(str::to_owned)
            .chain(["machine", "add", "root@203.0.113.11"].map(str::to_owned)),
    )
    .expect("invocation parses");

    assert_eq!(
        invocation.nats_url.as_deref(),
        Some("nats://127.0.0.1:4222")
    );
    assert!(matches!(
        invocation.command,
        PloyzctlCommand::MachineAddRemote(_)
    ));
}

#[test]
fn binary_help_only_advertises_implemented_commands() {
    let output = run_ployzctl([] as [&str; 0]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).contains("Usage: ployzctl"));
    for command in [
        "deploy", "backup", "init", "machine", "service", "logs", "ops",
    ] {
        assert!(stdout(&output).contains(command), "missing {command}");
    }
    assert_eq!(stderr(&output), "");
}

#[test]
fn binary_dispatches_init_first_machine() {
    let output = run_ployzctl(&["init", "--machine", "machine_1"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        stdout(&output),
        "init first machine machine_1\nsupervise nats-server\nsupervise roles control machine gateway dns\n"
    );
    assert_eq!(stderr(&output), "");
}

#[test]
fn binary_init_can_print_keeper_first_machine_install_command() {
    let output = run_ployzctl(init_with_keeper_install_arg_refs());

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).contains("install ployz-keeper first-machine-install --spec -"));
    assert!(stdout(&output).contains(r#""machine_id": "machine_1""#));
    assert!(stdout(&output).contains(r#""gateway": "install""#));
    assert_eq!(stderr(&output), "");
}

#[test]
fn binary_init_can_run_keeper_first_machine_install_command() {
    let temp = temp_dir("ployzctl-fake-keeper");
    let keeper = temp.join("ployz-keeper");
    let captured_args = temp.join("keeper-args");
    let captured_stdin = temp.join("keeper-stdin");
    fs::write(
        &keeper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat > '{}'\nprintf 'keeper installed\\n'\n",
            captured_args.display(),
            captured_stdin.display()
        ),
    )
    .expect("fake keeper can be written");
    make_executable(&keeper);

    let output = run_ployzctl(init_with_keeper_run_arg_refs(
        keeper.to_str().expect("keeper path is utf-8"),
    ));

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "keeper installed\n");
    assert_eq!(stderr(&output), "");
    assert_eq!(
        fs::read_to_string(captured_args).expect("fake keeper captured args"),
        "first-machine-install\n--spec\n-\n"
    );
    let stdin = fs::read_to_string(captured_stdin).expect("fake keeper captured stdin");
    assert!(stdin.contains(r#""machine_id":"machine_1""#));
    assert!(stdin.contains(r#""gateway":"install""#));
}

#[test]
fn binary_init_succeeds_when_keeper_output_is_truncated() {
    let temp = temp_dir("ployzctl-verbose-keeper");
    let keeper = temp.join("ployz-keeper");
    fs::write(
        &keeper,
        "#!/bin/sh\npython3 - <<'PY'\nprint('x' * 70000)\nPY\n",
    )
    .expect("fake verbose keeper can be written");
    make_executable(&keeper);

    let output = run_ployzctl(init_with_keeper_run_arg_refs(
        keeper.to_str().expect("keeper path is utf-8"),
    ));

    assert!(
        output.status.success(),
        "stdout length: {}\nstdout:\n{}\nstderr:\n{}",
        output.stdout.len(),
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(output.stdout.len(), 64 * 1024);
    assert_eq!(stderr(&output), "");
}

#[test]
fn cli_init_rejects_emit_and_run_together() {
    let spec = write_first_machine_install_spec(None);
    assert!(
        parse_command(
            [
                "init",
                "--emit-keeper-install",
                "--run-keeper-install",
                "--install-spec",
                spec.to_str().expect("spec path is utf-8"),
            ]
            .map(str::to_owned)
        )
        .is_err()
    );
}

#[test]
fn cli_init_accepts_keeper_binary_before_run_flag() {
    let spec = write_first_machine_install_spec(None);
    let command = parse_command(
        [
            "init",
            "--keeper-binary",
            "/tmp/ployz-keeper",
            "--run-keeper-install",
            "--install-spec",
            spec.to_str().expect("spec path is utf-8"),
        ]
        .map(str::to_owned),
    )
    .expect("init command accepts order-independent keeper binary");

    let PloyzctlCommand::Init(command) = command else {
        panic!("expected init command");
    };
    assert_eq!(command.machine_id(), &machine_id("machine_1"));
}

#[test]
fn cli_init_rejects_old_activation_flag() {
    assert!(
        parse_command(
            ["init", "--machine", "machine_1", "--activate-first-machine"].map(str::to_owned)
        )
        .is_err()
    );
}

#[test]
fn cli_init_rejects_keeper_binary_with_emit_mode() {
    let spec = write_first_machine_install_spec(None);
    assert!(
        parse_command(
            [
                "init",
                "--emit-keeper-install",
                "--keeper-binary",
                "/tmp/ployz-keeper",
                "--install-spec",
                spec.to_str().expect("spec path is utf-8"),
            ]
            .map(str::to_owned)
        )
        .is_err()
    );
}

#[test]
fn binary_rejects_unimplemented_commands() {
    let output = run_ployzctl(&["service"]);

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).contains("Usage: ployzctl service"));
}

#[test]
fn binary_machine_add_requires_nats_url() {
    let output = run_ployzctl_without_context(["machine", "add", "root@203.0.113.11"]);

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "no cluster context: run `ployzctl machine init USER@HOST` to create one, pass --nats, or set PLOYZ_NATS_URL\n"
    );
}

#[test]
fn binary_ops_watch_requires_nats_url() {
    let output = run_ployzctl_without_context(["ops", "watch", "op_deploy"]);

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "no cluster context: run `ployzctl machine init USER@HOST` to create one, pass --nats, or set PLOYZ_NATS_URL\n"
    );
}

#[test]
fn binary_ops_status_requires_nats_url() {
    let output = run_ployzctl_without_context(["ops", "status", "op_deploy"]);

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "no cluster context: run `ployzctl machine init USER@HOST` to create one, pass --nats, or set PLOYZ_NATS_URL\n"
    );
}

#[test]
fn binary_machine_list_requires_nats_url() {
    let output = run_ployzctl_without_context(["machine", "list"]);

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "no cluster context: run `ployzctl machine init USER@HOST` to create one, pass --nats, or set PLOYZ_NATS_URL\n"
    );
}

/// U2: a corrupt context file is a loud error, not a silent fallback to the
/// missing-context message — otherwise a recorded cluster would be ignored.
#[test]
fn binary_rejects_corrupt_cluster_context_file() {
    let config_home = temp_dir("ployzctl-corrupt-context");
    let context_dir = config_home.join("ployz");
    fs::create_dir_all(&context_dir).expect("context dir can be created");
    fs::write(context_dir.join("context.json"), "{not json").expect("corrupt context writes");

    let output = run_ployzctl_with(["machine", "list"], |command| {
        command
            .env_remove("PLOYZ_NATS_URL")
            .env_remove("HOME")
            .env("XDG_CONFIG_HOME", &config_home);
    });

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).contains("cluster context file"));
    assert!(stderr(&output).contains("context.json"));
}

#[test]
fn binary_corrupt_cluster_context_does_not_block_local_init_summary() {
    let config_home = temp_dir("ployzctl-corrupt-context-local-init");
    let context_dir = config_home.join("ployz");
    fs::create_dir_all(&context_dir).expect("context dir can be created");
    fs::write(context_dir.join("context.json"), "{not json").expect("corrupt context writes");

    let output = run_ployzctl_with(["init", "--machine", "machine_1"], |command| {
        command
            .env_remove("HOME")
            .env("XDG_CONFIG_HOME", &config_home);
    });

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        stdout(&output),
        "init first machine machine_1\nsupervise nats-server\nsupervise roles control machine gateway dns\n"
    );
    assert_eq!(stderr(&output), "");
}

#[test]
fn binary_machine_inspect_requires_nats_url() {
    let output = run_ployzctl_without_context(["machine", "inspect", "machine_2"]);

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "no cluster context: run `ployzctl machine init USER@HOST` to create one, pass --nats, or set PLOYZ_NATS_URL\n"
    );
}

#[test]
fn init_first_machine_can_include_gateway_role() {
    let output = FirstMachineInitOutput::summary(
        MachineId::try_new("machine_1").expect("valid machine id"),
        InstallRolePolicy::install_all(),
    )
    .render();

    assert_eq!(
        output,
        "init first machine machine_1\nsupervise nats-server\nsupervise roles control machine gateway dns\n"
    );
}

#[test]
fn ops_watch_renders_persisted_operation_events() {
    let output = WatchOutput {
        events: vec![
            replayed(
                1,
                ployz_core::ops::OperationEvent::DeploySubmitted {
                    operation_id: operation_id("op_123"),
                    target: deploy_request(),
                },
            ),
            replayed(
                2,
                ployz_core::ops::OperationEvent::DeployCompleted {
                    operation_id: operation_id("op_123"),
                    outcome: ployz_core::ops::DeployCompletionOutcome::Completed,
                },
            ),
        ],
        output: OpsWatchOutput::Text,
    }
    .render();

    assert_eq!(output, "1 deploy.submitted\n2 deploy.completed\n");
}

#[test]
fn ops_watch_renders_dataplane_evidence_for_wireguard_ebpf_preparation() {
    let event = replayed(
        3,
        ployz_core::ops::OperationEvent::DeployDataplanePrepared {
            operation_id: operation_id("op_123"),
            report: PloyzNativeMeshPrepareReport::from_machines([PloyzNativeMeshMachineReady {
                machine_id: machine_id("machine_1"),
                ready: PloyzNativeMeshReady {
                    wireguard: WireGuardReady {
                        public_key: WireGuardPublicKey::try_new("public-key-1")
                            .expect("valid wireguard public key"),
                        evidence: vec![WireGuardReadyEvidence::Command {
                            program: "wg".to_owned(),
                            args: vec!["--version".to_owned()],
                        }],
                    },
                    ebpf_forwarding: EbpfForwardingReady {
                        evidence: vec![EbpfForwardingReadyEvidence::PloyzTcBytecode {
                            path: "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".to_owned(),
                            symbols: vec!["ployz_egress".to_owned(), "ployz_ingress".to_owned()],
                        }],
                    },
                },
            }])
            .expect("dataplane report is valid"),
        },
    );

    let text_output = WatchOutput {
        events: vec![event.clone()],
        output: OpsWatchOutput::Text,
    }
    .render();

    assert_eq!(text_output, "3 deploy.dataplane_prepared\n");

    let json_output = WatchOutput {
        events: vec![event],
        output: OpsWatchOutput::Json,
    }
    .render();
    let value: serde_json::Value =
        serde_json::from_str(json_output.trim()).expect("json watch output is JSONL");
    assert_eq!(
        value
            .pointer("/sequence")
            .and_then(serde_json::Value::as_str),
        Some("3")
    );
    assert_eq!(
        value
            .pointer("/event/event")
            .and_then(serde_json::Value::as_str),
        Some("deploy_dataplane_prepared")
    );
    assert_eq!(
        value
            .pointer("/event/report/machines/0/wireguard/public_key")
            .and_then(serde_json::Value::as_str),
        Some("public-key-1")
    );
    assert_eq!(
        value
            .pointer("/event/report/machines/0/ebpf_forwarding/evidence/0/kind")
            .and_then(serde_json::Value::as_str),
        Some("ployz_tc_bytecode")
    );
}

#[test]
fn ops_watch_renders_no_output_when_no_events_are_replayed() {
    let output = WatchOutput {
        events: Vec::new(),
        output: OpsWatchOutput::Text,
    }
    .render();

    assert_eq!(output, "");
}

#[test]
fn ops_status_renders_operation_state() {
    let output = StatusOutput::new(OperationStatusSnapshot::new(OperationStatus::Deploy {
        id: operation_id("op_deploy"),
        service_id: ServiceId::try_new("svc_api").expect("valid service id"),
        state: DeployOperationState::Running {
            stage: DeployRunningStage::WaitingForHealth,
        },
        last_event_sequence: event_sequence(7),
    }))
    .render();

    assert_eq!(
        output,
        "operation op_deploy\nkind deploy\nservice svc_api\nstate running:waiting-for-health\nlast-event 7\n"
    );
}

#[test]
fn ops_status_renders_unclaimed_machine_add() {
    let output = StatusOutput::new(OperationStatusSnapshot::new(OperationStatus::MachineAdd {
        id: operation_id("op_machine"),
        machine_id: machine_id("machine_2"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        roles: InstallRolePolicy::install_all().without_gateway(),
        state: ployz_core::machine::MachineAddOperationState::Completed,
        last_event_sequence: event_sequence(9),
    }))
    .render();

    assert_eq!(
        output,
        "operation op_machine\nkind machine-add\nmachine machine_2 name edge_2 gateway skip dns install\nstate completed\nlast-event 9\n"
    );
}

#[test]
fn service_list_renders_service_summaries() {
    let output = ServiceListOutput {
        services: vec![
            service_snapshot("svc_api", "rev_2"),
            service_snapshot("svc_worker", "rev_1"),
        ],
    }
    .render();

    assert_eq!(
        output,
        "svc_api active-revision rev_2\nsvc_worker active-revision rev_1\n"
    );
}

#[test]
fn service_list_renders_no_output_without_services() {
    let output = ServiceListOutput {
        services: Vec::new(),
    }
    .render();

    assert_eq!(output, "");
}

#[test]
fn service_inspect_renders_active_revision() {
    let output = ServiceInspectOutput::new(service_snapshot("svc_api", "rev_2")).render();

    assert_eq!(output, "service svc_api\nactive-revision rev_2\n");
}

fn replayed(sequence: u64, event: ployz_core::ops::OperationEvent) -> ReplayedOperationEvent {
    ReplayedOperationEvent {
        sequence: event_sequence(sequence),
        event,
    }
}

fn service_snapshot(service_id: &str, revision_id: &str) -> ServiceSnapshot {
    ServiceSnapshot {
        active: ActiveServiceState {
            namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
            service_id: ServiceId::try_new(service_id).expect("valid service id"),
            active_revision: RevisionId::try_new(revision_id).expect("valid revision id"),
        },
    }
}

fn deploy_request() -> DeployRequest {
    DeployRequest {
        namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
        target_revision: RevisionId::try_new("rev_2").expect("valid revision id"),
        services: vec![DeployServiceSpec {
            service_id: ServiceId::try_new("svc_api").expect("valid service id"),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            route: None,
        }],
    }
}

const PLOYZ_NEWLINE_SHA256: &str =
    "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e";

fn init_with_keeper_install_args() -> impl Iterator<Item = String> {
    init_with_keeper_install_arg_refs().into_iter()
}

fn init_with_keeper_install_arg_refs() -> Vec<String> {
    let spec = write_first_machine_install_spec(None);
    vec![
        "init".to_owned(),
        "--emit-keeper-install".to_owned(),
        "--install-spec".to_owned(),
        spec.to_str().expect("spec path is utf-8").to_owned(),
    ]
}

fn init_with_keeper_install_args_with_public_ip() -> Vec<String> {
    let spec = write_first_machine_install_spec(Some("203.0.113.10"));
    vec![
        "init".to_owned(),
        "--emit-keeper-install".to_owned(),
        "--install-spec".to_owned(),
        spec.to_str().expect("spec path is utf-8").to_owned(),
    ]
}

fn init_with_keeper_run_arg_refs(keeper_binary: &str) -> Vec<String> {
    let spec = write_first_machine_install_spec(None);
    vec![
        "init".to_owned(),
        "--run-keeper-install".to_owned(),
        "--install-spec".to_owned(),
        spec.to_str().expect("spec path is utf-8").to_owned(),
        "--keeper-binary".to_owned(),
        keeper_binary.to_owned(),
    ]
}

fn write_first_machine_install_spec(machine_public_ip: Option<&str>) -> std::path::PathBuf {
    write_first_machine_install_spec_with_source("/tmp/ployzd", machine_public_ip)
}

fn write_first_machine_install_spec_with_source(
    ployzd_source: &str,
    machine_public_ip: Option<&str>,
) -> std::path::PathBuf {
    let temp = temp_dir("ployzctl-first-machine-spec");
    let path = temp.join("first-machine-install.json");
    fs::write(
        &path,
        first_machine_install_spec_json(ployzd_source, machine_public_ip),
    )
    .expect("first-machine install spec can be written");
    path
}

fn first_machine_install_spec_json(ployzd_source: &str, machine_public_ip: Option<&str>) -> String {
    let machine_public_ip = machine_public_ip
        .map(|value| format!(r#""{value}""#))
        .unwrap_or_else(|| "null".to_owned());
    format!(
        r#"{{
            "machine_id": "machine_1",
            "gateway": "install",
            "dns": "install",
            "machine_public_ip": {machine_public_ip},
            "machine_bootstrap_url": null,
            "machine_join_template_file": "/etc/ployz/machine-join-template.json",
            "machine_join_cluster_name": "ployz",
            "machine_join_runtime_nats_url": "tls://203.0.113.10:4222",
            "artifacts": {{
                "ployzd": {{
                    "version": "0.1.0",
                    "source": "{ployzd_source}",
                    "sha256": "{PLOYZ_NEWLINE_SHA256}",
                    "install_path": "/usr/local/bin/ployzd"
                }},
                "ebpf_bytecode": {{
                    "version": "0.1.0",
                    "source": "/tmp/ployz-ebpf-tc",
                    "sha256": "{PLOYZ_NEWLINE_SHA256}",
                    "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
                }},
                "ebpf_ctl": {{
                    "version": "0.1.0",
                    "source": "/tmp/ployz-ebpf-ctl",
                    "sha256": "{PLOYZ_NEWLINE_SHA256}",
                    "install_path": "/usr/local/bin/ployz-ebpf-ctl"
                }},
                "nats_server": {{
                    "version": "2.12.0",
                    "source": "/tmp/nats-server",
                    "sha256": "{PLOYZ_NEWLINE_SHA256}",
                    "binary": "/usr/local/bin/nats-server",
                    "config": "/etc/nats/nats-server.conf"
                }}
            }}
        }}"#
    )
}

fn run_ployzctl<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_ployzctl_with(args, |_| {})
}

fn run_ployzctl_without_context<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_ployzctl_with(args, |command| {
        command
            .env_remove("PLOYZ_NATS_URL")
            .env_remove("HOME")
            .env_remove("XDG_CONFIG_HOME");
    })
}

fn run_ployzctl_with<I, S>(args: I, configure: impl FnOnce(&mut Command)) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployzctl"));
    configure(&mut command);
    command.args(args).output().expect("ployzctl binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn backup_target(key_prefix: &str) -> ployz_core::backup::BackupTarget {
    ployz_core::backup::BackupTarget::S3 {
        bucket: "ployz-backups".to_owned(),
        key_prefix: key_prefix.to_owned(),
        region: "us-east-1".to_owned(),
        endpoint_url: None,
        addressing_style: ployz_core::backup::S3AddressingStyle::VirtualHosted,
    }
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{}-{}-{unique}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir can be created");
    dir
}
