use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;

use ployz_core::corrosion::StoredRow;
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
    InstallSha256Digest,
};
use ployz_core::join::JoinStorageChoice;
use ployz_core::machine::MachineLifecycle;

use super::super::orchestration::tests::{accepted, join_blob, join_input};
use super::*;
use crate::{ArtifactKind, HostRunnerCommandOutput};

#[derive(Default)]
struct RecordingRunner {
    outputs: VecDeque<HostRunnerCommandOutput>,
    calls: Vec<String>,
    downloads: Vec<String>,
    docker_installed: bool,
}

impl RecordingRunner {
    fn with_outputs(outputs: impl IntoIterator<Item = HostRunnerCommandOutput>) -> Self {
        Self {
            outputs: outputs.into_iter().collect(),
            calls: Vec::new(),
            downloads: Vec::new(),
            docker_installed: true,
        }
    }
}

impl HostRunnerCommandRunner for RecordingRunner {
    fn command(
        &mut self,
        program: &str,
        args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        self.calls.push(format!("{program} {}", args.join(" ")));
        self.outputs
            .pop_front()
            .ok_or_else(|| failure("unexpected test command"))
    }

    fn is_linux(&mut self) -> bool {
        true
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(0)
    }

    fn download(&mut self, url: &str, _destination: &Path) -> Result<(), FailureMessage> {
        self.downloads.push(url.to_owned());
        Ok(())
    }

    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        Ok(())
    }

    fn docker_is_installed(&mut self) -> bool {
        self.docker_installed
    }

    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        Ok(false)
    }

    fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
        Ok(false)
    }
}

fn output(stdout: impl Into<String>) -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success: true,
        exit_code: Some(0),
        stdout: stdout.into(),
        stdout_truncated: false,
        failure: String::new(),
    }
}

fn memory(gib: u64) -> HostRunnerCommandOutput {
    output(format!("MemTotal: {} kB\n", gib * 1024 * 1024))
}

fn validated_fixture() -> (
    tempfile::TempDir,
    MachineJoinStateDirectory,
    PreparedMachineJoin,
    ValidatedMachineJoinAccepted,
) {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
    let prepared =
        super::super::prepare_machine_join(&state, join_blob(), join_input()).expect("prepared");
    let accepted = accepted(prepared.request(), prepared.blob().door_cert_fingerprint())
        .try_validate(prepared.request(), prepared.blob().door_cert_fingerprint())
        .expect("validated acceptance");
    (directory, state, prepared, accepted)
}

#[test]
fn endpoint_network_readiness_requires_the_accepted_gateway_exactly() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
    let mut runner = RecordingRunner::with_outputs([output("10.210.7.1\n")]);
    let mut profile = Some(
        crate::detect_host_platform("ID=ubuntu\nVERSION_ID=24.04\n").expect("systemd profile"),
    );
    let directories = SupervisorDirectories::new(
        directory.path().join("systemd"),
        directory.path().join("openrc"),
    );

    LinuxSubstrate::new(state.path(), &mut runner, &mut profile, &directories)
        .await_endpoint_network_gateway("10.210.7.1".parse().expect("gateway"))
        .expect("exact accepted gateway is ready");

    assert_eq!(
        runner.calls,
        ["docker network inspect ployz --format {{(index .IPAM.Config 0).Gateway}}"]
    );
}

#[test]
fn dns_activation_is_systemd_only_and_enables_then_restarts_the_unit() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
    let directories = SupervisorDirectories::new(
        directory.path().join("systemd"),
        directory.path().join("openrc"),
    );
    let mut systemd_runner = RecordingRunner::with_outputs([output(""), output(""), output("")]);
    let mut systemd_profile = Some(
        crate::detect_host_platform("ID=ubuntu\nVERSION_ID=24.04\n").expect("systemd profile"),
    );

    LinuxSubstrate::new(
        state.path(),
        &mut systemd_runner,
        &mut systemd_profile,
        &directories,
    )
    .enable_and_start_dns()
    .expect("systemd DNS activation succeeds");

    assert_eq!(
        systemd_runner.calls,
        [
            "systemctl daemon-reload",
            "systemctl enable ployzd-dns.service",
            "systemctl restart ployzd-dns.service",
        ]
    );

    let mut openrc_runner = RecordingRunner::default();
    let mut openrc_profile =
        Some(crate::detect_host_platform("ID=alpine\nVERSION_ID=3.22\n").expect("OpenRC profile"));
    let error = LinuxSubstrate::new(
        state.path(),
        &mut openrc_runner,
        &mut openrc_profile,
        &directories,
    )
    .enable_and_start_dns()
    .expect_err("OpenRC cannot provide the DNS isolation contract");

    assert!(error.as_str().contains("requires systemd"));
    assert!(openrc_runner.calls.is_empty());
}

#[test]
fn gateway_activation_enables_restarts_and_verifies_both_supervisors() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
    let directories = SupervisorDirectories::new(
        directory.path().join("systemd"),
        directory.path().join("openrc"),
    );
    let mut systemd_runner =
        RecordingRunner::with_outputs([output(""), output(""), output(""), output("")]);
    let mut systemd_profile = Some(
        crate::detect_host_platform("ID=ubuntu\nVERSION_ID=24.04\n").expect("systemd profile"),
    );

    LinuxSubstrate::new(
        state.path(),
        &mut systemd_runner,
        &mut systemd_profile,
        &directories,
    )
    .enable_start_and_verify_gateway()
    .expect("systemd Gateway activation succeeds");
    assert_eq!(
        systemd_runner.calls,
        [
            "systemctl daemon-reload",
            "systemctl enable ployzd-gateway.service",
            "systemctl restart ployzd-gateway.service",
            "systemctl is-active --quiet ployzd-gateway.service",
        ]
    );

    let mut openrc_runner = RecordingRunner::with_outputs([output(""), output(""), output("")]);
    let mut openrc_profile =
        Some(crate::detect_host_platform("ID=alpine\nVERSION_ID=3.22\n").expect("OpenRC profile"));
    LinuxSubstrate::new(
        state.path(),
        &mut openrc_runner,
        &mut openrc_profile,
        &directories,
    )
    .enable_start_and_verify_gateway()
    .expect("OpenRC Gateway activation succeeds");
    assert_eq!(
        openrc_runner.calls,
        [
            "rc-update add ployzd-gateway default",
            "rc-service ployzd-gateway restart",
            "rc-service ployzd-gateway status",
        ]
    );
}

#[cfg(unix)]
#[test]
fn joined_machine_writes_a_private_gateway_scoped_environment() {
    use std::os::unix::fs::PermissionsExt as _;

    let (directory, state, _prepared, accepted) = validated_fixture();
    let effects = LinuxMachineJoinHostEffects {
        state: state.clone(),
        runner: SystemHostRunnerCommandRunner::default(),
        profile: None,
        supervisor_directories: SupervisorDirectories::new(
            directory.path().join("systemd"),
            directory.path().join("openrc"),
        ),
    };
    let MachineTransport::Wireguard { addr_v6, .. } =
        &accepted.accepted().machine.document.transport
    else {
        panic!("fixture uses WireGuard")
    };

    effects
        .write_environment(&accepted, *addr_v6, "secret")
        .expect("environments write");
    let path = state.path().join(GATEWAY_ENV_FILE);
    let contents = fs::read_to_string(&path).expect("gateway environment");
    assert_eq!(
        contents,
        format!(
            "PLOYZ_CORROSION_API_ADDR=127.0.0.1:8080\nPLOYZ_CORROSION_BEARER_TOKEN=secret\nPLOYZ_CLUSTER_ID={}\nPLOYZ_MACHINE_ID={}\nPLOYZ_GATEWAY_LISTEN_ADDR=0.0.0.0:80\n",
            accepted.accepted().cluster.cluster_id,
            accepted.accepted().machine.machine_id,
        )
    );
    assert_eq!(
        fs::metadata(path)
            .expect("gateway environment metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn joined_corrosion_waits_for_keeper_owned_wireguard_on_boot() {
    let (_, systemd_unit) = crate::lifecycle::production::corrosion_unit(
        SupervisorBackend::Systemd,
        Path::new("/var/lib/ployz/corrosion.toml"),
    );
    assert!(
        systemd_unit.contains("After=network-online.target ployzd-keeper.service"),
        "{systemd_unit}"
    );
}

#[test]
fn machine_join_seeds_current_and_renders_systemd_units_from_it() {
    let (directory, state, _prepared, _accepted) = validated_fixture();
    let source = directory.path().join("ployzd");
    fs::write(&source, b"ployz\n").expect("artifact writes");
    let artifact = InstallArtifactSpec {
        version: InstallArtifactVersion::try_new("0.1.0").expect("version"),
        source: InstallArtifactSource::try_new(source.display().to_string()).expect("source"),
        sha256: InstallSha256Digest::try_new(
            "2dcc3bb1142455239d3b3391d9569a8ce0fbdfb906cd0434329e5dd736592138",
        )
        .expect("digest"),
        install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployzd").expect("path"),
    };
    let systemd = directory.path().join("systemd");
    fs::create_dir_all(&systemd).expect("systemd directory");
    let directories = SupervisorDirectories::new(systemd.clone(), directory.path().join("openrc"));
    let privilege_commands =
        crate::installed_role_privilege_commands(state.path(), crate::HostPackageFamily::Debian)
            .expect("privilege install commands");
    let expected_privilege_calls = privilege_commands
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    const ROLE_UNIT_SUPERVISOR_COMMAND_COUNT: usize = 8;
    let mut runner = RecordingRunner::with_outputs(std::iter::once(output("ID=ubuntu\n")).chain(
        std::iter::repeat_n(
            output(""),
            privilege_commands.len() + ROLE_UNIT_SUPERVISOR_COMMAND_COUNT,
        ),
    ));
    let mut profile = None;
    let mut substrate = LinuxSubstrate::new(state.path(), &mut runner, &mut profile, &directories);

    substrate
        .install_artifact(ArtifactKind::Ployzd, &artifact)
        .expect("machine join seeds ployzd store");
    let environment =
        PloyzdRoleEnvironmentFile::new(state.path().join("ployzd.env")).expect("environment path");
    substrate
        .install_ployzd_units(&environment)
        .expect("machine join writes role units");

    assert_eq!(
        fs::read_link(state.path().join("current")).expect("current link"),
        PathBuf::from("artifacts")
            .join("2dcc3bb1142455239d3b3391d9569a8ce0fbdfb906cd0434329e5dd736592138")
    );
    let keeper = fs::read_to_string(systemd.join("ployzd-keeper.service")).expect("keeper unit");
    assert!(keeper.contains(&format!(
        "ExecStart={}/current keeper",
        state.path().display()
    )));
    assert!(systemd.join("ployzd-keeper-revert.service").exists());
    assert!(!keeper.contains("/usr/local/bin/ployzd"));
    let [profile_call, remaining_calls @ ..] = runner.calls.as_slice() else {
        panic!("unit installation must inspect the host profile")
    };
    assert_eq!(profile_call, "cat /etc/os-release");
    assert!(
        remaining_calls
            .iter()
            .take(expected_privilege_calls.len())
            .eq(expected_privilege_calls.iter())
    );
}

#[test]
fn join_docker_install_uses_the_detected_non_debian_profile() {
    let cases = [
        (
            "ID=alpine\nVERSION_ID=3.22\n",
            "apk add docker",
            &["rc-update add docker default", "rc-service docker start"][..],
        ),
        (
            "ID=arch\n",
            "pacman --noconfirm -S docker",
            &["systemctl enable --now docker"][..],
        ),
        (
            "ID=opensuse-tumbleweed\n",
            "zypper --non-interactive install docker",
            &["systemctl enable --now docker"][..],
        ),
        (
            "ID=amzn\nVERSION_ID=2023\n",
            "dnf install -y docker",
            &["systemctl enable --now docker"][..],
        ),
        (
            "ID=rocky\nVERSION_ID=9.6\n",
            "dnf install -y docker-ce",
            &["systemctl enable --now docker"][..],
        ),
        (
            "ID=almalinux\nVERSION_ID=9.6\n",
            "dnf install -y docker-ce",
            &["systemctl enable --now docker"][..],
        ),
    ];

    for (os_release, install_command, supervisor_commands) in cases {
        let (directory, state, _prepared, _accepted) = validated_fixture();
        let mut runner = RecordingRunner::with_outputs(
            std::iter::once(output(os_release)).chain(std::iter::repeat_n(output(""), 3)),
        );
        runner.docker_installed = false;
        let directories = SupervisorDirectories::new(
            directory.path().join("systemd"),
            directory.path().join("openrc"),
        );
        let mut profile = None;

        LinuxSubstrate::new(state.path(), &mut runner, &mut profile, &directories)
            .ensure_docker()
            .expect("profile-specific Docker installation");

        assert!(
            runner.calls.iter().any(|call| call == install_command),
            "{os_release:?} used {:?}",
            runner.calls
        );
        for command in supervisor_commands {
            assert!(
                runner.calls.iter().any(|call| call == command),
                "{os_release:?} used {:?}",
                runner.calls
            );
        }
        assert!(runner.downloads.is_empty());
    }
}

#[test]
fn storage_inventory_applies_only_reachable_zfs_ambiguity() {
    let mut plain = RecordingRunner::with_outputs([memory(4)]);
    let plain_inventory = join_storage_inventory(
        &mut plain,
        JoinStorageChoice::Flag {
            mode: StorageMode::Plain,
        },
    )
    .expect("plain skips ZFS inventory");
    assert!(matches!(
        plain_inventory,
        JoinStorageInventory::PlainRequested { .. }
    ));
    assert_eq!(plain.calls, ["cat /proc/meminfo"]);

    let mut missing = RecordingRunner::with_outputs([memory(4), output("")]);
    assert!(
        join_storage_inventory(
            &mut missing,
            JoinStorageChoice::Flag {
                mode: StorageMode::Zfs,
            },
        )
        .is_err()
    );

    let mut low_memory = RecordingRunner::with_outputs([memory(1), output("alpha\nbeta\n")]);
    let automatic = join_storage_inventory(&mut low_memory, JoinStorageChoice::Automatic)
        .expect("low-memory automatic selection must be plain");
    assert!(automatic.facts().imported_zfs_pool);
    assert_eq!(automatic.selected_pool(), None);

    let mut ambiguous = RecordingRunner::with_outputs([memory(4), output("alpha\nbeta\n")]);
    assert!(join_storage_inventory(&mut ambiguous, JoinStorageChoice::Automatic).is_err());
}

#[test]
fn persisted_storage_inventory_resumes_without_host_probes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
    let lock = state.try_lock().expect("lock");
    let mut first =
        RecordingRunner::with_outputs([output("edge-a.example\n"), memory(4), output("tank\n")]);
    let prepared = prepare_linux_machine_join_locked(
        &state,
        &lock,
        join_blob(),
        JoinStorageChoice::Automatic,
        None,
        &mut first,
    )
    .expect("first preparation");
    drop(lock);

    let lock = state.try_lock().expect("resume lock");
    let mut resumed = RecordingRunner::default();
    let resumed_prepared = prepare_linux_machine_join_locked(
        &state,
        &lock,
        join_blob(),
        JoinStorageChoice::Flag {
            mode: StorageMode::Plain,
        },
        None,
        &mut resumed,
    )
    .expect("persisted request and inventory win");

    assert_eq!(resumed_prepared.request(), prepared.request());
    assert!(resumed.calls.is_empty());
}

#[test]
fn selected_storage_action_uses_plain_path_or_retained_zfs_pool() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
    assert_eq!(
        selected_storage_action(&state, StorageMode::Plain).expect("plain action"),
        SelectedStorageAction::Plain {
            volumes_path: directory.path().join("volumes")
        }
    );

    let inventory = JoinStorageInventory::ZfsSelected {
        total_memory_bytes: 4 * 1024 * 1024 * 1024,
        pool: ZfsPoolName::try_new("tank").expect("pool"),
    };
    state
        .persist_storage_inventory(&inventory)
        .expect("inventory");
    assert_eq!(
        selected_storage_action(&state, StorageMode::Zfs).expect("ZFS action"),
        SelectedStorageAction::Zfs {
            pool: ZfsPoolName::try_new("tank").expect("pool")
        }
    );
}

#[test]
fn corrosion_decoder_is_strict_and_bounded() {
    let valid = concat!(
        "{\"columns\":[\"id\",\"document\"]}\n",
        "{\"row\":[1,[\"machine\",\"{}\"]]}\n",
        "{\"eoq\":{\"time\":0.1}}\n"
    );
    assert_eq!(
        decode_corrosion_rows(valid).expect("valid frames"),
        [StoredRow::new("machine", "{}")]
    );
    for invalid in [
        "{\"columns\":[\"document\",\"id\"]}\n{\"eoq\":{\"time\":0.1}}\n",
        "{\"columns\":[\"id\",\"document\"]}\n{\"row\":[1,[1,\"{}\"]]}\n{\"eoq\":{\"time\":0.1}}\n",
        "{\"columns\":[\"id\",\"document\"]}\n{\"error\":\"nope\"}\n",
        "{\"columns\":[\"id\",\"document\"]}\n",
        "{\"columns\":[\"id\",\"document\"]}\n{\"eoq\":{\"time\":0.1}}\n{\"eoq\":{\"time\":0.2}}\n",
    ] {
        assert!(
            decode_corrosion_rows(invalid).is_err(),
            "accepted {invalid}"
        );
    }
    assert!(decode_corrosion_rows(&"x".repeat(CORROSION_QUERY_BODY_LIMIT as usize + 1)).is_err());
}

#[test]
fn convergence_requires_exact_accepted_name_winner() {
    let (_directory, _state, _prepared, accepted) = validated_fixture();
    let expected = accepted.accepted().machine.clone();
    let document = serde_json::to_string(&expected.document).expect("document");
    assert_eq!(
        roster_convergence_disposition(
            &accepted,
            vec![StoredRow::new(expected.machine_id.as_str(), document)]
        ),
        RosterConvergenceDisposition::Converged
    );
    assert_eq!(
        roster_convergence_disposition(&accepted, Vec::new()),
        RosterConvergenceDisposition::Missing
    );

    let mut divergent = expected.document.clone();
    divergent.lifecycle = MachineLifecycle::Draining;
    assert_eq!(
        roster_convergence_disposition(
            &accepted,
            vec![StoredRow::new(
                expected.machine_id.as_str(),
                serde_json::to_string(&divergent).expect("document")
            )]
        ),
        RosterConvergenceDisposition::Divergent
    );
    let mut foreign = expected.document;
    foreign.cluster_id =
        ployz_core::ids::ClusterName::try_new("foreign-cluster").expect("cluster name");
    assert_eq!(
        roster_convergence_disposition(
            &accepted,
            vec![StoredRow::new(
                expected.machine_id.as_str(),
                serde_json::to_string(&foreign).expect("document")
            )]
        ),
        RosterConvergenceDisposition::Skipped
    );
}

#[test]
fn query_token_environment_and_artifact_contracts_are_exact() {
    let (directory, state, prepared, accepted) = validated_fixture();
    let secret_path = directory.path().join(CORROSION_TOKEN_FILE);
    fs::write(&secret_path, b"super-secret\n").expect("secret");
    let token = CorrosionBearerToken::from_file(&secret_path).expect("token");
    assert_eq!(format!("{token:?}"), "CorrosionBearerToken([REDACTED])");
    assert!(!format!("{token:?}").contains("super-secret"));

    let query = corrosion_roster_query(&accepted).expect("query");
    assert_eq!(query.url, "http://127.0.0.1:8080/v1/queries");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&query.body).expect("body"),
        serde_json::json!([
            "SELECT id, document FROM machines WHERE id = ?",
            [prepared.request().name.as_str()]
        ])
    );

    let MachineTransport::Wireguard { addr_v6, .. } =
        accepted.accepted().machine.document.transport
    else {
        panic!("fixture transport")
    };
    let environment = render_environment(&state, &accepted, addr_v6, "super-secret");
    for line in [
        format!(
            "PLOYZ_API_DOOR_PRIVATE_KEY_PATH={}",
            directory.path().join(DOOR_KEY_FILE).display()
        ),
        format!(
            "PLOYZ_API_DOOR_CERTIFICATE_PATH={}",
            directory.path().join(DOOR_CERTIFICATE_FILE).display()
        ),
        format!(
            "PLOYZ_API_DOOR_FINGERPRINT_PATH={}",
            directory.path().join(DOOR_FINGERPRINT_FILE).display()
        ),
        format!(
            "{JOIN_SUBSTRATE_ENV}={}",
            state.join_substrate_path().display()
        ),
        format!(
            "PLOYZ_WIREGUARD_PRIVATE_KEY_PATH={}",
            directory.path().join(WIREGUARD_KEY_FILE).display()
        ),
        "PLOYZ_CORROSION_VERSION=0.3.1".to_owned(),
    ] {
        assert!(environment.lines().any(|candidate| candidate == line));
    }
    assert!(!environment.contains("PLOYZ_JOIN_DOOR_PORT="));
    let config = render_corrosion_config(
        &state,
        &accepted,
        "/usr/local/lib/ployz/corrosion-schema-v1.sql",
        addr_v6,
        "super-secret",
    );
    assert!(config.contains("authz.bearer-token = \"super-secret\""));

    assert_eq!(
        ArtifactKind::ALL,
        [
            ArtifactKind::Ployzd,
            ArtifactKind::EbpfBytecode,
            ArtifactKind::EbpfCtl,
            ArtifactKind::Corrosion,
            ArtifactKind::CorrosionSchema,
        ]
    );
}
