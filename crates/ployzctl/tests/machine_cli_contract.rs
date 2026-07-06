use std::fs;

use ployz_core::ids::MachineId;
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
    InstallSha256Digest, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
    MachineJoinTrustedNats,
};
use ployz_core::nats_config::NatsCaCertificatePem;
use ployz_core::roles::{DnsRole, GatewayRole, InstallRolePolicy};
use ployz_core::state::MachineLifecycle;
use ployz_core::state::{
    ActiveMachineState, GatewayServingStatus, GatewayStatusObservation, MachinePublicIpObservation,
};
use ployz_sdk_types::{AcceptedOperation, MachineSnapshot};
use ployz_test_support::fs::make_executable;
use ployz_test_support::ids::{event_sequence, machine_id, operation_id};
use ployzctl::bootstrap_command::{
    BootstrapInstaller, BootstrapRelease, CloudBootstrapCommand, DEFAULT_BOOTSTRAP_URL,
    DEFAULT_RELEASE_CHANNEL, FounderBootstrapCommand,
};
use ployzctl::commands::machine::{
    MachineAddInstaller, MachineAddOutput, MachineBootstrapUrl, MachineIdentity,
    MachineIdentityError, MachineInspectOutput, MachineJoinRuntimeNatsUrl, MachineJoinToken,
    MachineListOutput, MachineName, derive_machine_identity,
};
use ployzctl::commands::{PloyzctlCommand, parse_command};
use ployzctl::ssh::{SshClient, SshCommandError, SshPhase, SshTarget, SshTargetParseError};

#[test]
fn machine_add_prints_bootstrap_command_without_nats_credentials() {
    let output = MachineAddOutput {
        machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_bundle: machine_join_bundle("nats://127.0.0.1:7422"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
        join_seed: test_join_seed(),
    }
    .render();

    assert!(output.contains("operation op_machine"));
    assert!(output.contains("machine machine_2"));
    assert!(output.contains("join-token join_once_123"));
    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh'"));
    assert!(output.contains(
        " | PLOYZ_VERSION='0.1.0' PLOYZ_NATS_URL='nats://127.0.0.1:7422' PLOYZ_NATS_CA_B64="
    ));
    assert!(output.contains(&format!(
        "PLOYZ_JOIN_NKEY_SEED='{}' sh -s -- ",
        TEST_JOIN_SEED
    )));
    assert!(output.contains("--join-token 'join_once_123'"));
    assert!(!output.contains("creds"));
}

#[test]
fn machine_add_prints_runtime_nats_url_from_accepted_response() {
    let output = MachineAddOutput {
        machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_bundle: machine_join_bundle("nats://127.0.0.1:7423"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
        join_seed: test_join_seed(),
    }
    .render();

    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh'"));
    assert!(output.contains("join-token join_once_123"));
    assert!(output.contains(
        " | PLOYZ_VERSION='0.1.0' PLOYZ_NATS_URL='nats://127.0.0.1:7423' PLOYZ_NATS_CA_B64="
    ));
    assert!(output.contains("--join-token 'join_once_123'"));
}

#[test]
fn machine_add_debug_redacts_join_token() {
    let output = MachineAddOutput {
        machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_bundle: machine_join_bundle("nats://127.0.0.1:7422"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
        join_seed: test_join_seed(),
    };

    let debug = format!("{output:?}");

    assert!(debug.contains("[redacted]"));
    assert!(!debug.contains("join_once_123"));
    assert!(debug.contains("127.0.0.1"));
}

#[test]
fn machine_add_shell_quotes_join_material() {
    let output = MachineAddOutput {
        machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh/bootstrap?x='quoted'")
            .expect("valid bootstrap url"),
        join_bundle: machine_join_bundle("nats://127.0.0.1:7422"),
        join_token: MachineJoinToken::try_new("join'quoted'").expect("valid join token"),
        join_seed: test_join_seed(),
    }
    .render();

    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh/bootstrap?x='\\''quoted'\\'''"));
    assert!(output.contains("PLOYZ_VERSION='0.1.0'"));
    assert!(output.contains("PLOYZ_NATS_URL='nats://127.0.0.1:7422'"));
    assert!(output.contains("--join-token 'join'\\''quoted'\\'''"));
}

#[test]
fn machine_add_rejects_bootstrap_url_that_curl_could_treat_as_an_option() {
    assert!(MachineBootstrapUrl::try_new("--help").is_err());
    assert!(MachineBootstrapUrl::try_new("http://get.ployz.sh").is_err());
}

#[test]
fn machine_add_rejects_join_tokens_with_shell_invisible_characters() {
    assert!(MachineJoinToken::try_new("").is_err());
    assert!(MachineJoinToken::try_new("join token").is_err());
    assert!(MachineJoinToken::try_new("join\ntoken").is_err());
}

#[test]
fn machine_list_renders_machine_summaries() {
    let output = MachineListOutput {
        machines: vec![machine_snapshot(
            "machine_1",
            Some(GatewayServingStatus::Current),
        )],
    }
    .render();

    assert_eq!(
        output,
        "machine_1 edge_1 public-ip 203.0.113.10 gateway current 127.0.0.1:8080 routes 2 containers 3\n"
    );
}

#[test]
fn machine_list_renders_no_output_without_machines() {
    let output = MachineListOutput {
        machines: Vec::new(),
    }
    .render();

    assert_eq!(output, "");
}

#[test]
fn machine_inspect_renders_machine_detail() {
    let output = MachineInspectOutput::new(machine_snapshot(
        "machine_1",
        Some(GatewayServingStatus::LastKnownGood),
    ))
    .render();

    assert_eq!(
        output,
        "machine machine_1\nname edge_1\nactivated-by op_machine\npublic-ip 203.0.113.10\ngateway last-known-good 127.0.0.1:8080 routes 2\ncontainers 3\n"
    );
}

#[test]
fn machine_inspect_renders_missing_observations_as_unknown() {
    let mut machine = machine_snapshot("machine_1", None);
    machine.public_ip = None;

    let output = MachineInspectOutput::new(machine).render();

    assert_eq!(
        output,
        "machine machine_1\nname edge_1\nactivated-by op_machine\npublic-ip unknown\ngateway none\ncontainers 3\n"
    );
}

fn accepted_operation(operation_id: &str) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: self::operation_id(operation_id),
        watch_subject: format!("plz.v1.op.{operation_id}.>"),
        start_sequence: event_sequence(1),
    }
}

fn machine_snapshot(machine_id: &str, gateway: Option<GatewayServingStatus>) -> MachineSnapshot {
    let machine_id = self::machine_id(machine_id);
    MachineSnapshot {
        last_observed_at_unix_seconds: None,
        active: ActiveMachineState {
            lifecycle: MachineLifecycle::Active,
            machine_id: machine_id.clone(),
            name: MachineName::try_new("edge_1").expect("valid machine name"),
            activated_by: operation_id("op_machine"),
            public_endpoint: None,
        },
        public_ip: Some(MachinePublicIpObservation {
            machine_id: machine_id.clone(),
            public_ip: "203.0.113.10".parse().expect("valid public ip"),
        }),
        gateway: gateway.map(|serving| GatewayStatusObservation {
            machine_id,
            listen_addr: "127.0.0.1:8080".parse().expect("valid listen addr"),
            serving,
            route_count: 2,
        }),
        observed_container_count: 3,
    }
}

const TEST_JOIN_SEED: &str = "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM";

fn test_join_seed() -> ployz_core::nats_config::NatsUserSeed {
    ployz_core::nats_config::NatsUserSeed::try_new(TEST_JOIN_SEED).expect("test seed is valid")
}

fn machine_join_bundle(runtime_nats_url: &str) -> MachineJoinBundle {
    MachineJoinBundle {
        material: MachineJoinMaterial {
            cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(runtime_nats_url)
                .expect("valid runtime NATS URL"),
            trusted_nats: MachineJoinTrustedNats {
                ca_pem: NatsCaCertificatePem::try_new(
                    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                )
                .expect("valid CA pem"),
            },
            ployzd: InstallArtifactSpec {
                version: version("0.1.0"),
                source: source("/tmp/ployzd"),
                sha256: digest("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                install_path: absolute_path("/usr/local/bin/ployzd"),
            },
            ebpf_bytecode: InstallArtifactSpec {
                version: version("0.1.0"),
                source: source("/tmp/ployz-ebpf-tc"),
                sha256: digest("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                install_path: absolute_path("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"),
            },
            ebpf_ctl: InstallArtifactSpec {
                version: version("0.1.0"),
                source: source("/tmp/ployz-ebpf-ctl"),
                sha256: digest("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                install_path: absolute_path("/usr/local/bin/ployz-ebpf-ctl"),
            },
        },
    }
}

fn version(value: &str) -> InstallArtifactVersion {
    InstallArtifactVersion::try_new(value).expect("valid artifact version")
}

fn source(value: &str) -> InstallArtifactSource {
    InstallArtifactSource::try_new(value).expect("valid artifact source")
}

fn digest(value: &str) -> InstallSha256Digest {
    InstallSha256Digest::try_new(value).expect("valid digest")
}

fn absolute_path(value: &str) -> AbsoluteInstallPath {
    AbsoluteInstallPath::try_new(value).expect("valid absolute path")
}

// --- SSH target parsing (U3) ---

#[test]
fn ssh_target_parses_user_at_ip() {
    let target = SshTarget::parse("root@203.0.113.10").expect("target parses");

    assert_eq!(target.user(), "root");
    assert_eq!(target.host(), "203.0.113.10");
    assert_eq!(target.destination(), "root@203.0.113.10");
}

#[test]
fn ssh_target_parses_user_at_dns_name() {
    let target = SshTarget::parse("deploy@sg-core-1.example.com").expect("target parses");

    assert_eq!(target.user(), "deploy");
    assert_eq!(target.host(), "sg-core-1.example.com");
}

#[test]
fn ssh_target_requires_user_at_host_shape() {
    let error = SshTarget::parse("203.0.113.10").expect_err("missing user fails");

    assert_eq!(
        error,
        SshTargetParseError::MissingUser {
            target: "203.0.113.10".to_owned(),
        }
    );
    assert!(error.to_string().contains("user@host"));
    assert!(error.to_string().contains("root@203.0.113.10"));
}

#[test]
fn ssh_target_rejects_empty_user_and_empty_host() {
    assert_eq!(
        SshTarget::parse("@203.0.113.10").expect_err("empty user fails"),
        SshTargetParseError::EmptyUser {
            target: "@203.0.113.10".to_owned(),
        }
    );
    assert_eq!(
        SshTarget::parse("root@").expect_err("empty host fails"),
        SshTargetParseError::EmptyHost {
            target: "root@".to_owned(),
        }
    );
}

#[test]
fn ssh_target_rejects_values_ssh_could_parse_as_options() {
    assert_eq!(
        SshTarget::parse("-oProxyCommand=evil@host").expect_err("option-shaped user fails"),
        SshTargetParseError::InvalidUser {
            target: "-oProxyCommand=evil@host".to_owned(),
        }
    );
    assert_eq!(
        SshTarget::parse("root@-evil.example.com").expect_err("option-shaped host fails"),
        SshTargetParseError::InvalidHost {
            target: "root@-evil.example.com".to_owned(),
        }
    );
}

#[test]
fn ssh_target_rejects_whitespace_and_extra_separators() {
    assert!(matches!(
        SshTarget::parse("ro ot@host"),
        Err(SshTargetParseError::InvalidUser { .. })
    ));
    assert!(matches!(
        SshTarget::parse("root@host one"),
        Err(SshTargetParseError::InvalidHost { .. })
    ));
    assert!(matches!(
        SshTarget::parse("a@b@c"),
        Err(SshTargetParseError::InvalidHost { .. })
    ));
}

// --- Hostname-derived machine identity (U3) ---

#[test]
fn valid_hostname_derives_matching_machine_id_and_machine_name() {
    let identity = MachineIdentity::from_remote_hostname("sg-core-1").expect("hostname is valid");

    assert_eq!(identity.machine_id, machine_id("sg-core-1"));
    assert_eq!(
        identity.name,
        MachineName::try_new("sg-core-1").expect("valid machine name")
    );
    assert_eq!(identity.machine_id.as_str(), identity.name.as_str());
}

#[test]
fn invalid_hostname_fails_with_name_escape_hatch() {
    let error =
        MachineIdentity::from_remote_hostname("sg.core.1").expect_err("dotted hostname fails");

    let MachineIdentityError::InvalidHostname { hostname, .. } = &error else {
        panic!("expected invalid hostname error, got {error:?}");
    };
    assert_eq!(hostname, "sg.core.1");
    let rendered = error.to_string();
    assert!(rendered.contains("sg.core.1"));
    assert!(rendered.contains("not a valid Ployz machine identity"));
    assert!(rendered.contains("--name"));
}

#[test]
fn name_override_wins_over_remote_hostname() {
    let identity = derive_machine_identity("sg.core.1", Some("edge-1")).expect("override is valid");

    assert_eq!(identity.machine_id, machine_id("edge-1"));
    assert_eq!(identity.name.as_str(), "edge-1");
}

#[test]
fn hostname_is_used_when_no_name_override_is_given() {
    let identity = derive_machine_identity("sg-core-1", None).expect("hostname is valid");

    assert_eq!(identity.machine_id, machine_id("sg-core-1"));
}

#[test]
fn invalid_name_override_reports_the_flag() {
    let error = derive_machine_identity("sg-core-1", Some("bad name")).expect_err("override fails");

    assert!(matches!(
        error,
        MachineIdentityError::InvalidNameOverride { .. }
    ));
    let rendered = error.to_string();
    assert!(rendered.contains("--name"));
    assert!(rendered.contains("bad name"));
}

// --- Bounded SSH execution against a stand-in ssh program (U3) ---

fn fake_ssh(script_body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let script = dir.path().join("fake-ssh");
    write_executable_script(&script, &format!("#!/bin/sh\n{script_body}"));
    (dir, script)
}

#[cfg(unix)]
fn write_executable_script(path: &std::path::Path, contents: &str) {
    let staged = path.with_extension("tmp");
    fs::write(&staged, contents).expect("fake ssh writes");
    make_executable(&staged);
    fs::rename(staged, path).expect("fake ssh stages atomically");
}

fn ssh_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(5)
}

#[cfg(unix)]
#[test]
fn read_remote_hostname_returns_trimmed_first_line() {
    let (_dir, script) = fake_ssh("echo 'sg-core-1'\n");
    let client = SshClient::with_program(script, ssh_timeout());
    let target = SshTarget::parse("root@203.0.113.10").expect("target parses");

    let hostname = client
        .read_remote_hostname(&target)
        .expect("hostname reads");

    assert_eq!(hostname, "sg-core-1");
}

#[cfg(unix)]
#[test]
fn ssh_commands_pass_destination_and_remote_command() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let args_file = dir.path().join("args");
    let script = dir.path().join("fake-ssh");
    write_executable_script(
        &script,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\necho sg-core-1\n",
            args_file.display()
        ),
    );
    let client = SshClient::with_program(script, ssh_timeout());
    let target = SshTarget::parse("root@203.0.113.10").expect("target parses");

    client
        .read_remote_hostname(&target)
        .expect("hostname reads");

    let args = fs::read_to_string(&args_file).expect("args file reads");
    let lines: Vec<&str> = args.lines().collect();
    assert_eq!(
        lines,
        vec![
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "root@203.0.113.10",
            "hostname",
        ]
    );
}

#[cfg(unix)]
#[test]
fn ssh_failures_carry_phase_and_stderr() {
    let (_dir, script) = fake_ssh("echo 'Connection refused' >&2\nexit 255\n");
    let client = SshClient::with_program(script, ssh_timeout());
    let target = SshTarget::parse("root@203.0.113.10").expect("target parses");

    let error = client
        .read_remote_hostname(&target)
        .expect_err("refused connection fails");

    let SshCommandError::Failed { phase, .. } = error.as_ref() else {
        panic!("expected failed command error, got {error:?}");
    };
    assert_eq!(*phase, SshPhase::ReadHostname);
    let rendered = error.to_string();
    assert!(rendered.contains("read-hostname"));
    assert!(rendered.contains("root@203.0.113.10"));
    assert!(rendered.contains("stderr: Connection refused"));
}

#[cfg(unix)]
#[test]
fn ssh_commands_time_out_with_phase_evidence() {
    let (_dir, script) = fake_ssh("exec sleep 5\n");
    let client = SshClient::with_program(script, std::time::Duration::from_millis(200));
    let target = SshTarget::parse("root@203.0.113.10").expect("target parses");

    let error = client
        .read_remote_hostname(&target)
        .expect_err("slow command times out");

    assert!(matches!(error.as_ref(), SshCommandError::Timeout { .. }));
    let rendered = error.to_string();
    assert!(rendered.contains("read-hostname"));
    assert!(rendered.contains("timed out"));
}

// --- `machine init` CLI shape (U4) ---

fn parse(args: &[&str]) -> PloyzctlCommand {
    parse_command(args.iter().map(|arg| (*arg).to_owned())).expect("command parses")
}

#[test]
fn machine_init_parses_target_with_default_roles_and_release() {
    let PloyzctlCommand::MachineInit(command) = parse(&["machine", "init", "root@203.0.113.10"])
    else {
        panic!("expected machine init command");
    };

    assert_eq!(command.target.destination(), "root@203.0.113.10");
    assert_eq!(command.identity_override, None);
    assert_eq!(command.roles, InstallRolePolicy::install_all());
    assert_eq!(
        command.release,
        BootstrapRelease::Channel(DEFAULT_RELEASE_CHANNEL.to_owned())
    );
    assert_eq!(command.release_manifest_url, None);
    assert_eq!(command.bootstrap_url.as_str(), DEFAULT_BOOTSTRAP_URL);
    assert_eq!(command.cluster_name.as_str(), "ployz");
    assert_eq!(command.installer_script, None);
    assert_eq!(
        command.installer(),
        BootstrapInstaller::BootstrapUrl(
            MachineBootstrapUrl::try_new(DEFAULT_BOOTSTRAP_URL)
                .expect("default bootstrap url is valid")
        )
    );
}

#[test]
fn machine_init_role_opt_outs_skip_only_the_requested_roles() {
    let PloyzctlCommand::MachineInit(command) =
        parse(&["machine", "init", "root@203.0.113.10", "--no-gateway"])
    else {
        panic!("expected machine init command");
    };
    assert_eq!(command.roles.gateway, GatewayRole::Skip);
    assert_eq!(command.roles.dns, DnsRole::Install);

    let PloyzctlCommand::MachineInit(command) =
        parse(&["machine", "init", "root@203.0.113.10", "--no-dns"])
    else {
        panic!("expected machine init command");
    };
    assert_eq!(command.roles.gateway, GatewayRole::Install);
    assert_eq!(command.roles.dns, DnsRole::Skip);

    let PloyzctlCommand::MachineInit(command) = parse(&[
        "machine",
        "init",
        "root@203.0.113.10",
        "--no-gateway",
        "--no-dns",
    ]) else {
        panic!("expected machine init command");
    };
    assert_eq!(
        command.roles,
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns()
    );
}

#[test]
fn machine_init_name_override_is_validated_at_parse_time() {
    let PloyzctlCommand::MachineInit(command) = parse(&[
        "machine",
        "init",
        "root@203.0.113.10",
        "--name",
        "sg-core-1",
    ]) else {
        panic!("expected machine init command");
    };
    let Some(identity) = command.identity_override else {
        panic!("expected an identity override");
    };
    assert_eq!(identity.machine_id, machine_id("sg-core-1"));

    let error = parse_command(
        [
            "machine",
            "init",
            "root@203.0.113.10",
            "--name",
            "sg.core.1",
        ]
        .map(str::to_owned),
    )
    .expect_err("dotted name override fails");
    assert!(error.to_string().contains("--name"));
}

#[test]
fn machine_init_rejects_invalid_ssh_targets() {
    let error = parse_command(["machine", "init", "203.0.113.10"].map(str::to_owned))
        .expect_err("target without user fails");
    assert!(error.to_string().contains("user@host"));
}

#[test]
fn machine_init_accepts_expert_release_overrides() {
    let PloyzctlCommand::MachineInit(command) = parse(&[
        "machine",
        "init",
        "root@203.0.113.10",
        "--release-manifest",
        "file:///tmp/manifest.env",
        "--installer-script",
        "/tmp/ployz-install.sh",
        "--cluster-name",
        "dind-e2e",
    ]) else {
        panic!("expected machine init command");
    };
    assert_eq!(
        command.release_manifest_url.as_deref(),
        Some("file:///tmp/manifest.env")
    );
    assert_eq!(
        command.installer(),
        BootstrapInstaller::RemoteScript("/tmp/ployz-install.sh".to_owned())
    );
    assert_eq!(command.cluster_name.as_str(), "dind-e2e");
}

#[test]
fn machine_init_accepts_explicit_version_and_channel_selection() {
    let PloyzctlCommand::MachineInit(command) = parse(&[
        "machine",
        "init",
        "root@203.0.113.10",
        "--version",
        "v0.0.2-alpha.1",
    ]) else {
        panic!("expected machine init command");
    };
    assert_eq!(
        command.release,
        BootstrapRelease::Version("v0.0.2-alpha.1".to_owned())
    );

    let PloyzctlCommand::MachineInit(command) =
        parse(&["machine", "init", "root@203.0.113.10", "--channel", "alpha"])
    else {
        panic!("expected machine init command");
    };
    assert_eq!(
        command.release,
        BootstrapRelease::Channel("alpha".to_owned())
    );

    let error = parse_command(
        [
            "machine",
            "init",
            "root@203.0.113.10",
            "--channel",
            "alpha",
            "--version",
            "v0.0.2-alpha.1",
        ]
        .map(str::to_owned),
    )
    .expect_err("channel and version conflict");
    assert!(error.to_string().contains("--channel"));
}

// --- `machine add USER@HOST` CLI shape (U6) ---

#[test]
fn machine_add_remote_target_defaults_gateway_install() {
    let PloyzctlCommand::MachineAddRemote(command) =
        parse(&["machine", "add", "root@203.0.113.11"])
    else {
        panic!("expected remote machine add command");
    };
    assert_eq!(command.target.destination(), "root@203.0.113.11");
    assert_eq!(command.identity_override, None);
    assert_eq!(command.roles, InstallRolePolicy::install_all());
    assert_eq!(
        command.installer(),
        MachineAddInstaller::FromAcceptedBootstrapUrl
    );
}

#[test]
fn machine_add_remote_accepts_name_and_gateway_opt_out() {
    let PloyzctlCommand::MachineAddRemote(command) = parse(&[
        "machine",
        "add",
        "root@203.0.113.11",
        "--name",
        "sg-edge-1",
        "--no-gateway",
        "--installer-script",
        "/tmp/ployz-install.sh",
    ]) else {
        panic!("expected remote machine add command");
    };
    let Some(identity) = &command.identity_override else {
        panic!("expected an identity override");
    };
    assert_eq!(identity.machine_id, machine_id("sg-edge-1"));
    assert_eq!(
        command.roles,
        InstallRolePolicy::install_all().without_gateway()
    );
    assert_eq!(
        command.installer(),
        MachineAddInstaller::RemoteScript("/tmp/ployz-install.sh".to_owned())
    );
}

#[test]
fn machine_add_accepts_dns_and_combined_role_opt_outs() {
    let PloyzctlCommand::MachineAddRemote(command) =
        parse(&["machine", "add", "root@203.0.113.11", "--no-dns"])
    else {
        panic!("expected remote machine add command");
    };
    assert_eq!(
        command.roles,
        InstallRolePolicy::install_all().without_dns()
    );

    let PloyzctlCommand::MachineAdd(command) = parse(&[
        "internal",
        "machine-add",
        "--machine",
        "edge_2",
        "--name",
        "edge_2",
        "--operation",
        "op_machine",
        "--idempotency-key",
        "idem_machine",
        "--no-gateway",
        "--no-dns",
    ]) else {
        panic!("expected explicit machine add command");
    };
    assert_eq!(
        command.roles,
        InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns()
    );
}

#[test]
fn machine_add_explicit_flags_conflict_with_a_remote_target() {
    assert!(
        parse_command(
            [
                "internal",
                "machine-add",
                "root@203.0.113.11",
                "--machine",
                "edge_2",
            ]
            .map(str::to_owned)
        )
        .is_err()
    );
}

#[test]
fn machine_add_without_target_still_requires_explicit_flags() {
    let error = parse_command(
        [
            "internal",
            "machine-add",
            "--name",
            "edge_2",
            "--machine",
            "edge_2",
        ]
        .map(str::to_owned),
    )
    .expect_err("missing operation id fails");
    let rendered = error.to_string();
    assert!(rendered.contains("--operation"));
    assert!(rendered.contains("USER@HOST"));
}

// --- Cloud bootstrap command rendering ---

#[test]
fn cloud_bootstrap_command_renders_tokenless_interactive_command() {
    let command = CloudBootstrapCommand {
        installer: BootstrapInstaller::BootstrapUrl(
            MachineBootstrapUrl::try_new("https://ployz.sh").expect("valid bootstrap url"),
        ),
        cloud_host: None,
    };

    assert_eq!(
        command.render(),
        "curl -fsSL -- 'https://ployz.sh' | sh && sudo ployz-keeper bootstrap"
    );
}

#[test]
fn cloud_bootstrap_command_renders_custom_cloud_host_after_installer_pipe() {
    let command = CloudBootstrapCommand {
        installer: BootstrapInstaller::BootstrapUrl(
            MachineBootstrapUrl::try_new("https://ployz.sh").expect("valid bootstrap url"),
        ),
        cloud_host: Some("cloud.example.com".to_owned()),
    };

    let rendered = command.render();

    assert_eq!(
        rendered,
        "curl -fsSL -- 'https://ployz.sh' | sh && sudo ployz-keeper bootstrap --cloud-host 'cloud.example.com'"
    );
}

#[test]
fn cloud_bootstrap_command_shell_quotes_remote_script_and_host() {
    let command = CloudBootstrapCommand {
        installer: BootstrapInstaller::RemoteScript("/tmp/ployz install.sh".to_owned()),
        cloud_host: Some("cloud.example.com/path?x='quoted'".to_owned()),
    };

    let rendered = command.render();

    assert!(rendered.starts_with("sh '/tmp/ployz install.sh' && sudo ployz-keeper bootstrap"));
    assert!(rendered.contains("--cloud-host 'cloud.example.com/path?x='\\''quoted'\\'''"));
}

#[test]
fn machine_init_rejects_deferred_link_cloud_flag() {
    let error =
        parse_command(["machine", "init", "root@203.0.113.10", "--link-cloud"].map(str::to_owned))
            .expect_err("cloud link flag is deferred");

    assert!(error.to_string().contains("unexpected argument"));
}

// --- Founder install command rendering (U4) ---

#[test]
fn founder_bootstrap_command_carries_minimal_first_machine_inputs() {
    let command = FounderBootstrapCommand {
        installer: BootstrapInstaller::BootstrapUrl(
            MachineBootstrapUrl::try_new("https://ployz.sh").expect("valid bootstrap url"),
        ),
        release: BootstrapRelease::Version("v0.0.1-alpha.1".to_owned()),
        release_manifest_url: Some("file:///tmp/manifest.env".to_owned()),
        machine_id: machine_id("sg-core-1"),
        roles: InstallRolePolicy::install_all()
            .without_gateway()
            .without_dns(),
        bootstrap_url: MachineBootstrapUrl::try_new("https://ployz.sh")
            .expect("valid bootstrap url"),
        cluster_name: MachineJoinClusterName::try_new("testcluster").expect("valid cluster name"),
        runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222")
            .expect("valid runtime nats URL"),
        machine_public_ip: Some("203.0.113.10".parse().expect("valid IP")),
    };

    let rendered = command.render();

    for expected in [
        "curl -fsSL -- 'https://ployz.sh'",
        "PLOYZ_VERSION='v0.0.1-alpha.1'",
        "PLOYZ_RELEASE_MANIFEST_URL='file:///tmp/manifest.env'",
        "PLOYZ_MACHINE_PUBLIC_IP='203.0.113.10'",
        "PLOYZ_MACHINE_ID='sg-core-1'",
        "PLOYZ_GATEWAY='skip'",
        "PLOYZ_DNS='skip'",
        "PLOYZ_MACHINE_BOOTSTRAP_URL='https://ployz.sh'",
        "PLOYZ_MACHINE_JOIN_CLUSTER_NAME='testcluster'",
        "PLOYZ_MACHINE_JOIN_NATS_URL='tls://203.0.113.10:4222'",
        "sh -s -- --first-machine",
    ] {
        assert!(
            rendered.contains(expected),
            "rendered command missing {expected}: {rendered}"
        );
    }
    assert!(!rendered.contains("first-machine-spec"));
}

#[test]
fn founder_bootstrap_command_can_carry_channel_instead_of_version() {
    let command = FounderBootstrapCommand {
        installer: BootstrapInstaller::BootstrapUrl(
            MachineBootstrapUrl::try_new("https://ployz.sh").expect("valid bootstrap url"),
        ),
        release: BootstrapRelease::Channel("alpha".to_owned()),
        release_manifest_url: None,
        machine_id: machine_id("sg-core-1"),
        roles: InstallRolePolicy::install_all(),
        bootstrap_url: MachineBootstrapUrl::try_new("https://ployz.sh")
            .expect("valid bootstrap url"),
        cluster_name: MachineJoinClusterName::try_new("testcluster").expect("valid cluster name"),
        runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222")
            .expect("valid runtime nats URL"),
        machine_public_ip: None,
    };

    let rendered = command.render();

    assert!(rendered.contains("PLOYZ_CHANNEL='alpha'"));
    assert!(!rendered.contains("PLOYZ_VERSION="));
    assert!(rendered.contains("sh -s -- --first-machine"));
}

// --- Remote install command rendering (U6) ---

#[test]
fn remote_join_install_command_matches_the_printed_install_line() {
    let output = MachineAddOutput {
        machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_bundle: machine_join_bundle("nats://127.0.0.1:7422"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
        join_seed: test_join_seed(),
    };

    let rendered = output.render();
    let command = output.install_command(&MachineAddInstaller::FromAcceptedBootstrapUrl, None);
    assert!(rendered.contains(&format!("install {command}\n")));

    let with_ip = output.install_command(
        &MachineAddInstaller::FromAcceptedBootstrapUrl,
        Some("203.0.113.11".parse().expect("valid ip")),
    );
    assert!(with_ip.contains("| PLOYZ_MACHINE_PUBLIC_IP='203.0.113.11' PLOYZ_VERSION="));

    let scripted = output.install_command(
        &MachineAddInstaller::RemoteScript("/tmp/ployz-install.sh".to_owned()),
        None,
    );
    assert!(scripted.contains("sh '/tmp/ployz-install.sh' --join-token 'join_once_123'"));
    assert!(!scripted.contains("curl"));
}

#[cfg(unix)]
#[test]
fn empty_remote_hostname_is_an_explicit_error() {
    let (_dir, script) = fake_ssh("exit 0\n");
    let client = SshClient::with_program(script, ssh_timeout());
    let target = SshTarget::parse("root@203.0.113.10").expect("target parses");

    let error = client
        .read_remote_hostname(&target)
        .expect_err("empty hostname fails");

    assert!(matches!(
        error.as_ref(),
        SshCommandError::EmptyRemoteHostname { .. }
    ));
    assert!(error.to_string().contains("empty hostname"));
}
