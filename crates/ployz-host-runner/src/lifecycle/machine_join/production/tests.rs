use std::collections::VecDeque;

use ployz_core::corrosion::StoredRow;
use ployz_core::join::JoinStorageChoice;
use ployz_core::machine::MachineLifecycle;

use super::super::orchestration::tests::{accepted, join_blob, join_input};
use super::*;
use crate::HostRunnerCommandOutput;

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
    assert_eq!(
        roster_convergence_disposition(
            &accepted,
            vec![StoredRow::new(
                "00000000000000000000000000",
                serde_json::to_string(&expected.document).expect("document")
            )]
        ),
        RosterConvergenceDisposition::Shadowed
    );

    let mut foreign = expected.document;
    foreign.cluster_id = ployz_core::ids::ClusterId::generate();
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
            "SELECT id, document FROM machines WHERE id = ?1 OR name = ?2",
            [prepared.request().machine_id.as_str(), "edge-a"]
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
            ArtifactKind::Railpack,
        ]
    );
}
