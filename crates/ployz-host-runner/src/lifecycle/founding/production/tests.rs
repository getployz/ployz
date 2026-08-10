use super::*;
use crate::HostRunnerCommandOutput;
use base64::Engine as _;
use std::path::PathBuf;

#[test]
fn founding_environment_has_the_public_join_door_settings() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let prepared = prepare_plain_founding(
        &state,
        fixture_artifacts(),
        RecordingRunner {
            calls: Vec::new(),
            allow_facts: true,
        },
    );

    let environment = String::from_utf8(
        prepared
            .effects
            .env_contents(false)
            .expect("environment renders"),
    )
    .expect("environment is UTF-8");

    for setting in [
        "PLOYZ_API_DOOR_LISTEN_ADDR=[::]:2021",
        "PLOYZ_API_DOOR_PRIVATE_KEY_PATH=/var/lib/ployz/door.key",
        "PLOYZ_API_DOOR_CERTIFICATE_PATH=/var/lib/ployz/door.crt",
        "PLOYZ_API_DOOR_FINGERPRINT_PATH=/var/lib/ployz/door.fingerprint",
        "PLOYZ_API_JOIN_SUBSTRATE_PATH=/var/lib/ployz/join-substrate.json",
    ] {
        assert!(
            environment.lines().any(|line| line == setting),
            "missing founding setting {setting:?} in {environment:?}"
        );
    }
    assert!(!environment.contains("PLOYZ_JOIN_DOOR_PORT="));
}

#[test]
fn founding_gateway_environment_contains_only_its_runtime_contract() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let prepared = prepare_plain_founding(
        &state,
        fixture_artifacts(),
        RecordingRunner {
            calls: Vec::new(),
            allow_facts: true,
        },
    );

    let environment = String::from_utf8(
        prepared
            .effects
            .gateway_env_contents()
            .expect("gateway environment renders"),
    )
    .expect("gateway environment is UTF-8");
    assert_eq!(environment.lines().count(), 5, "{environment}");
    for name in [
        "PLOYZ_CORROSION_API_ADDR=",
        "PLOYZ_CORROSION_BEARER_TOKEN=",
        "PLOYZ_CLUSTER_ID=",
        "PLOYZ_MACHINE_ID=",
        "PLOYZ_GATEWAY_LISTEN_ADDR=0.0.0.0:80",
    ] {
        assert!(environment.lines().any(|line| line.starts_with(name)));
    }
    assert!(environment.lines().any(|line| {
        line == format!("PLOYZ_MACHINE_ID={}", prepared.request.request().machine_id)
    }));
    assert!(!environment.contains("PLOYZ_API_"));
}

#[test]
fn founding_persists_the_complete_join_substrate_contract() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let artifacts = fixture_artifacts();
    let expected_artifacts = vec![
        artifacts.ployzd.clone(),
        artifacts.ebpf_bytecode.clone(),
        artifacts.ebpf_ctl.clone(),
        artifacts.corrosion.clone(),
        artifacts.corrosion_schema.clone(),
    ];
    let prepared = prepare_plain_founding(
        &state,
        artifacts,
        RecordingRunner {
            calls: Vec::new(),
            allow_facts: true,
        },
    );

    prepared
        .effects
        .persist_join_substrate()
        .expect("join substrate persists");

    let path = state.path().join(JOIN_SUBSTRATE_FILE);
    let substrate: ployz_core::join::JoinMachineSubstrate =
        serde_json::from_slice(&fs::read(&path).expect("join substrate bytes"))
            .expect("Core join substrate round-trips");
    assert_eq!(substrate.ployz_version().as_str(), "1");
    assert_eq!(substrate.corrosion_version(), "corrosion 0.2.0-beta.0");
    assert_eq!(substrate.artifacts(), expected_artifacts);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(
            fs::metadata(path)
                .expect("join substrate metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[derive(Debug)]
struct FactsRunner;

impl HostRunnerCommandRunner for FactsRunner {
    fn command(
        &mut self,
        program: &str,
        _args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        match program {
            "cat" => Ok(output(true, "MemTotal:       4194304 kB\n")),
            "zpool" => Ok(output(false, "")),
            _ => Err(failure("unexpected command")),
        }
    }

    fn is_linux(&mut self) -> bool {
        true
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(0)
    }

    fn download(&mut self, _url: &str, _destination: &Path) -> Result<(), FailureMessage> {
        Err(failure("download is not used during preparation"))
    }

    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        Ok(())
    }

    fn docker_is_installed(&mut self) -> bool {
        true
    }

    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        Ok(true)
    }

    fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
        Ok(true)
    }
}

#[derive(Debug)]
struct InventoryRunner {
    pools: &'static str,
}

impl HostRunnerCommandRunner for InventoryRunner {
    fn command(
        &mut self,
        program: &str,
        _args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        match program {
            "cat" => Ok(output(true, "MemTotal:       4194304 kB\n")),
            "zpool" => Ok(output(true, self.pools)),
            _ => Err(failure("unexpected command")),
        }
    }

    fn is_linux(&mut self) -> bool {
        true
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(0)
    }

    fn download(&mut self, _url: &str, _destination: &Path) -> Result<(), FailureMessage> {
        Err(failure("download is not used during preparation"))
    }

    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        Ok(())
    }

    fn docker_is_installed(&mut self) -> bool {
        true
    }

    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        Ok(true)
    }

    fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
        Ok(true)
    }
}

#[test]
fn preparation_builds_matching_request_without_persisting_generated_material() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let prepared = prepare_linux_founding(
        &state,
        LinuxFoundingInput {
            cluster_name: "ares".to_owned(),
            machine_name: MachineName::try_new("ares").expect("machine name"),
            endpoint: Some("203.0.113.7:51820".parse().expect("endpoint")),
            prefix: MachineEndpointSupernet::default_v1(),
            hostname_mode: AutomaticHostnameMode::Disabled,
            storage: InitStorageChoice::Automatic,
            driver: FoundingDriverInput::OnHost,
            written_at: CorrosionTimestamp::try_new("2026-08-04T12:00:00Z").expect("timestamp"),
            acme_directory_url: "https://acme.example/directory".to_owned(),
            acme_contact: None,
        },
        fixture_artifacts(),
        "corrosion 0.2.0-beta.0".to_owned(),
        FactsRunner,
    )
    .expect("preparation succeeds");

    assert_eq!(
        prepared.request.request().machine.storage.mode,
        StorageMode::Plain
    );
    assert!(!state.path().join("cluster-id").exists());
    assert!(!state.path().join(MACHINE_SEED_FILE).exists());
    let debug = format!("{:?}", prepared.effects);
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains(prepared.bootstrap_credential.as_str()));
}

#[test]
fn explicit_zfs_refuses_zero_imported_pools_before_resume_anchor() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let mut founding_input = input("2026-08-04T12:00:00Z");
    founding_input.storage = InitStorageChoice::Flag {
        mode: StorageMode::Zfs,
    };

    let result = prepare_linux_founding(
        &state,
        founding_input,
        fixture_artifacts(),
        "corrosion 0.2.0-beta.0".to_owned(),
        InventoryRunner { pools: "" },
    );
    let Err(error) = result else {
        panic!("zero imported pools must refuse explicit ZFS");
    };

    assert!(matches!(
        error,
        FoundingPreparationError::Storage(InitStorageSelectionError::ZfsPoolNotImported)
    ));
    assert!(!state.path().join("cluster-id").exists());
    assert!(!state.path().join(FOUNDING_REQUEST_FILE).exists());
}

#[test]
fn one_imported_pool_is_retained_for_explicit_storage_preparation() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let mut founding_input = input("2026-08-04T12:00:00Z");
    founding_input.storage = InitStorageChoice::Flag {
        mode: StorageMode::Zfs,
    };

    let prepared = prepare_linux_founding(
        &state,
        founding_input,
        fixture_artifacts(),
        "corrosion 0.2.0-beta.0".to_owned(),
        InventoryRunner { pools: "tank\n" },
    )
    .expect("one imported pool is unambiguous");

    assert_eq!(
        prepared.request.request().machine.storage.mode,
        StorageMode::Zfs
    );
    assert_eq!(
        prepared.effects.zfs_pool.as_ref().map(ZfsPoolName::as_str),
        Some("tank")
    );
    assert!(!state.path().join(FOUNDING_REQUEST_FILE).exists());
}

#[test]
fn multiple_imported_pools_refuse_before_resume_anchor_or_canonical_request() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");

    let result = prepare_linux_founding(
        &state,
        input("2026-08-04T12:00:00Z"),
        fixture_artifacts(),
        "corrosion 0.2.0-beta.0".to_owned(),
        InventoryRunner {
            pools: "zeta\nalpha\n",
        },
    );
    let Err(error) = result else {
        panic!("ambiguous imported pools must refuse founding");
    };

    assert!(
        error
            .to_string()
            .contains("export all but the intended pool or choose --storage plain")
    );
    assert!(matches!(
        error,
        FoundingPreparationError::MultipleImportedZfsPools { ref pools }
            if pools.iter().map(ZfsPoolName::as_str).collect::<Vec<_>>() == ["alpha", "zeta"]
    ));
    assert!(!state.path().join("cluster-id").exists());
    assert!(!state.path().join(FOUNDING_REQUEST_FILE).exists());
    assert!(!state.path().join(MACHINE_SEED_FILE).exists());
    assert!(!state.path().join(ZFS_POOL_FILE).exists());
}

#[test]
fn retry_after_pool_selection_write_retains_the_exact_pool() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let mut first = prepare_linux_founding(
        &state,
        input("2026-08-04T12:00:00Z"),
        fixture_artifacts(),
        "corrosion 0.2.0-beta.0".to_owned(),
        InventoryRunner { pools: "tank\n" },
    )
    .expect("initial selection succeeds");
    first
        .effects
        .ensure_machine_identity_and_wireguard()
        .expect("machine seed persists");
    state
        .persist_cluster_id_exclusive(&first.request.request().cluster_id)
        .expect("cluster anchor persists");
    write_durable_file(state.path(), ZFS_POOL_FILE, FileMode::Secret0600, b"tank\n")
        .expect("pool selection persists before canonical request");

    let mut retry_input = input("2026-08-05T12:00:00Z");
    retry_input.cluster_name = "changed-cluster".to_owned();
    retry_input.machine_name = MachineName::try_new("changed-machine").expect("machine name");
    let retry = prepare_linux_founding(
        &state,
        retry_input,
        fixture_artifacts(),
        "corrosion 0.2.0-beta.0".to_owned(),
        InventoryRunner {
            pools: "archive\ntank\n",
        },
    )
    .expect("retry preserves the already retained pool");

    assert_eq!(
        retry.effects.zfs_pool.as_ref().map(ZfsPoolName::as_str),
        Some("tank")
    );
    assert_eq!(retry.request.request().cluster.name, "ares");
    assert_eq!(retry.request.request().machine.name.as_str(), "ares");
    assert!(!state.path().join(FOUNDING_REQUEST_FILE).exists());
}

#[derive(Debug)]
struct RecordingRunner {
    calls: Vec<String>,
    allow_facts: bool,
}

impl HostRunnerCommandRunner for RecordingRunner {
    fn command(
        &mut self,
        program: &str,
        args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        self.calls.push(format!("{program} {}", args.join(" ")));
        match program {
            "cat" if self.allow_facts => Ok(output(true, "MemTotal: 4194304 kB\n")),
            "zpool" if self.allow_facts => Ok(output(true, "tank\n")),
            _ => Err(failure(format!("unexpected command {program}"))),
        }
    }

    fn is_linux(&mut self) -> bool {
        true
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(0)
    }

    fn download(&mut self, _url: &str, _destination: &Path) -> Result<(), FailureMessage> {
        Err(failure("unexpected download"))
    }

    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        Ok(())
    }

    fn docker_is_installed(&mut self) -> bool {
        true
    }

    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        Ok(true)
    }

    fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
        Ok(true)
    }
}

#[derive(Debug)]
struct DownloadRunner {
    payload: Vec<u8>,
    downloads: Vec<(String, PathBuf)>,
}

impl HostRunnerCommandRunner for DownloadRunner {
    fn command(
        &mut self,
        program: &str,
        _args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        match program {
            "/usr/local/bin/corrosion" => Ok(output(true, "corrosion 0.2.0-beta.0\n")),
            _ => Err(failure(format!("unexpected command {program}"))),
        }
    }

    fn is_linux(&mut self) -> bool {
        true
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(0)
    }

    fn download(&mut self, url: &str, destination: &Path) -> Result<(), FailureMessage> {
        self.downloads
            .push((url.to_owned(), destination.to_path_buf()));
        fs::write(destination, &self.payload).map_err(failure)
    }

    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        Ok(())
    }

    fn docker_is_installed(&mut self) -> bool {
        true
    }

    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        Ok(true)
    }

    fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
        Ok(true)
    }
}

#[test]
fn corrupt_cached_remote_artifact_is_redownloaded_reverified_and_installed() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let payload = b"verified ployzd artifact\n";
    let digest = format!("{:x}", Sha256::digest(payload));
    let artifacts = remote_artifacts(directory.path(), &digest);
    let mut prepared = prepare_plain_founding(
        &state,
        artifacts,
        DownloadRunner {
            payload: payload.to_vec(),
            downloads: Vec::new(),
        },
    );
    let cached = state.path().join("downloads").join(&digest);
    fs::create_dir_all(cached.parent().expect("cache has parent")).expect("create cache");
    fs::write(&cached, b"partial").expect("write corrupt cached artifact");

    prepared
        .effects
        .stage_exact_ployz_and_corrosion()
        .expect("corrupt cache is repaired");

    let [(url, staged)] = prepared.effects.runner.downloads.as_slice() else {
        panic!("corrupt cache triggers one download");
    };
    assert_eq!(url, "https://releases.example/ployzd");
    assert_ne!(staged, &cached);
    assert!(!staged.exists());
    assert_eq!(fs::read(cached).expect("cached artifact"), payload);
    assert_eq!(
        fs::read(state.path().join("artifacts").join(&digest)).expect("stored ployzd artifact"),
        payload
    );
    assert_eq!(
        fs::read_link(state.path().join("current")).expect("founding current link"),
        PathBuf::from("artifacts").join(&digest)
    );
    assert!(!directory.path().join("installed/ployzd").exists());
    for name in ["ebpf", "ebpf-ctl", "corrosion", "schema"] {
        assert_eq!(
            fs::read(directory.path().join("installed").join(name)).expect("installed artifact"),
            payload
        );
    }
}

#[test]
fn every_unpublished_partial_door_material_subset_is_replaced_as_one_complete_set() {
    const KEY: u8 = 0b001;
    const CERTIFICATE: u8 = 0b010;
    const FINGERPRINT: u8 = 0b100;

    for present in 1_u8..0b111 {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::initialize(directory.path().join("state"))
            .expect("state initializes");
        for (bit, file, contents) in [
            (KEY, DOOR_KEY_FILE, "stale partial key"),
            (CERTIFICATE, DOOR_CERTIFICATE_FILE, "stale partial cert"),
            (
                FINGERPRINT,
                DOOR_FINGERPRINT_FILE,
                "stale partial fingerprint",
            ),
        ] {
            if present & bit != 0 {
                fs::write(state.path().join(file), contents).expect("write crash remnant");
            }
        }
        let mut prepared = prepare_plain_founding(
            &state,
            fixture_artifacts(),
            RecordingRunner {
                calls: Vec::new(),
                allow_facts: false,
            },
        );

        for (bit, file, contents) in [
            (KEY, DOOR_KEY_FILE, "stale partial key"),
            (CERTIFICATE, DOOR_CERTIFICATE_FILE, "stale partial cert"),
            (
                FINGERPRINT,
                DOOR_FINGERPRINT_FILE,
                "stale partial fingerprint",
            ),
        ] {
            if present & bit == 0 {
                assert!(!state.path().join(file).exists());
            } else {
                assert_eq!(
                    fs::read_to_string(state.path().join(file)).expect("crash remnant survives"),
                    contents,
                    "subset {present:03b} preparation mutated {file}"
                );
            }
        }
        prepared
            .effects
            .ensure_cluster_door_material()
            .expect("locked effect repairs and publishes replacement");
        let certificate =
            fs::read_to_string(state.path().join(DOOR_CERTIFICATE_FILE)).expect("certificate");
        let private_key = fs::read_to_string(state.path().join(DOOR_KEY_FILE)).expect("key");
        let fingerprint =
            fs::read_to_string(state.path().join(DOOR_FINGERPRINT_FILE)).expect("fingerprint");
        assert_eq!(
            fingerprint.trim(),
            format!("{:x}", Sha256::digest(pem_der(&certificate))),
            "subset {present:03b} replacement fingerprint"
        );

        assert_ne!(certificate, "stale partial cert");
        assert_ne!(private_key, "stale partial key");
        assert_ne!(fingerprint, "stale partial fingerprint");
    }
}

#[test]
fn published_partial_door_material_requires_explicit_machine_reset() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let cluster_id = ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id");
    state
        .persist_cluster_id_exclusive(&cluster_id)
        .expect("persist cluster id");
    fs::write(state.path().join(DOOR_KEY_FILE), b"published partial key")
        .expect("write crash remnant");
    state
        .record_milestone(FoundingMilestone::DoorMaterial)
        .expect("publish door milestone");
    let preflight = inspect_linux_founding(
        &state,
        &mut RecordingRunner {
            calls: Vec::new(),
            allow_facts: false,
        },
    );
    assert!(matches!(
        preflight,
        Ok(LinuxFoundingPreflight::Refused {
            refusal: FoundingRefusal::IncompleteDoorMaterial {
                repair_command: FoundingRepairCommand::ResetMachine,
            },
            cluster_id: Some(found_cluster_id),
        }) if found_cluster_id == cluster_id
    ));
    let result = try_prepare_plain_founding(
        &state,
        fixture_artifacts(),
        RecordingRunner {
            calls: Vec::new(),
            allow_facts: false,
        },
    );
    let Err(error) = result else {
        panic!("published partial material must not be regenerated");
    };

    assert!(matches!(
        error,
        FoundingPreparationError::Refused(FoundingRefusal::IncompleteDoorMaterial {
            repair_command: FoundingRepairCommand::ResetMachine,
        })
    ));
    assert_eq!(
        fs::read(state.path().join(DOOR_KEY_FILE)).expect("partial key remains evidence"),
        b"published partial key"
    );
    assert!(!state.path().join(DOOR_CERTIFICATE_FILE).exists());
    assert!(!state.path().join(DOOR_FINGERPRINT_FILE).exists());
}

#[test]
fn machine_material_persists_keys_without_mutating_keeper_owned_wireguard() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let mut prepared = prepare_linux_founding(
        &state,
        input("2026-08-04T12:00:00Z"),
        fixture_artifacts(),
        "corrosion 0.2.0-beta.0".to_owned(),
        RecordingRunner {
            calls: Vec::new(),
            allow_facts: true,
        },
    )
    .expect("preparation succeeds");
    let commands_before_material = prepared.effects.runner.calls.len();

    prepared
        .effects
        .ensure_machine_identity_and_wireguard()
        .expect("first persistence succeeds");
    prepared
        .effects
        .ensure_machine_identity_and_wireguard()
        .expect("second persistence succeeds");

    let calls = &prepared.effects.runner.calls;
    assert_eq!(calls.len(), commands_before_material);
    assert!(state.path().join(MACHINE_SEED_FILE).is_file());
    assert!(state.path().join(WIREGUARD_KEY_FILE).is_file());
}

#[test]
fn persisted_request_and_secrets_are_reloaded_without_host_fact_probes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = FoundingStateDirectory::initialize(directory.path().join("state"))
        .expect("state initializes");
    let mut first = prepare_linux_founding(
        &state,
        input("2026-08-04T12:00:00Z"),
        fixture_artifacts(),
        "corrosion 0.2.0-beta.0".to_owned(),
        RecordingRunner {
            calls: Vec::new(),
            allow_facts: true,
        },
    )
    .expect("first preparation succeeds");
    first
        .effects
        .ensure_machine_identity_and_wireguard()
        .expect("machine material persists");
    first
        .effects
        .persist_validated_founding_request()
        .expect("request persists");
    first
        .effects
        .ensure_cluster_door_material()
        .expect("door persists");
    state
        .record_milestone(FoundingMilestone::DoorMaterial)
        .expect("complete door set can publish its milestone");
    write_durable_file(
        state.path(),
        BOOTSTRAP_CREDENTIAL_FILE,
        FileMode::Secret0600,
        first.bootstrap_credential.as_str().as_bytes(),
    )
    .expect("bootstrap credential persists");
    write_durable_file(
        state.path(),
        CORROSION_TOKEN_FILE,
        FileMode::Secret0600,
        first.effects.corrosion_token.as_bytes(),
    )
    .expect("Corrosion token persists");
    state
        .persist_cluster_id_exclusive(&first.request.request().cluster_id)
        .expect("cluster anchor persists");
    let expected_request = serde_json::to_value(first.request.request()).expect("request wire");
    let expected_bootstrap = first.bootstrap_credential.clone();
    let expected_corrosion = first.effects.corrosion_token.clone();
    let expected_door = (
        fs::read_to_string(state.path().join(DOOR_CERTIFICATE_FILE)).expect("certificate"),
        fs::read_to_string(state.path().join(DOOR_KEY_FILE)).expect("key"),
        fs::read_to_string(state.path().join(DOOR_FINGERPRINT_FILE)).expect("fingerprint"),
    );

    let mut second = prepare_linux_founding(
        &state,
        input("2026-08-05T12:00:00Z"),
        fixture_artifacts(),
        "corrosion 0.2.0-beta.0".to_owned(),
        RecordingRunner {
            calls: Vec::new(),
            allow_facts: false,
        },
    )
    .expect("resume preparation succeeds");

    assert_eq!(
        serde_json::to_value(second.request.request()).expect("request wire"),
        expected_request
    );
    assert_eq!(second.bootstrap_credential, expected_bootstrap);
    assert_eq!(second.effects.corrosion_token, expected_corrosion);
    second
        .effects
        .ensure_cluster_door_material()
        .expect("complete door material is reused under the lock");
    assert_eq!(
        (
            fs::read_to_string(state.path().join(DOOR_CERTIFICATE_FILE)).expect("certificate"),
            fs::read_to_string(state.path().join(DOOR_KEY_FILE)).expect("key"),
            fs::read_to_string(state.path().join(DOOR_FINGERPRINT_FILE)).expect("fingerprint"),
        ),
        expected_door
    );
    assert_eq!(
        second.effects.zfs_pool.as_ref().map(ZfsPoolName::as_str),
        Some("tank")
    );
    assert_eq!(
        fs::read_to_string(state.path().join(ZFS_POOL_FILE)).expect("retained pool is durable"),
        "tank\n"
    );
    assert!(second.effects.runner.calls.is_empty());
}

fn input(timestamp: &str) -> LinuxFoundingInput {
    LinuxFoundingInput {
        cluster_name: "ares".to_owned(),
        machine_name: MachineName::try_new("ares").expect("machine name"),
        endpoint: Some("203.0.113.7:51820".parse().expect("endpoint")),
        prefix: MachineEndpointSupernet::default_v1(),
        hostname_mode: AutomaticHostnameMode::Disabled,
        storage: InitStorageChoice::Automatic,
        driver: FoundingDriverInput::OnHost,
        written_at: CorrosionTimestamp::try_new(timestamp).expect("timestamp"),
        acme_directory_url: "https://acme.example/directory".to_owned(),
        acme_contact: None,
    }
}

fn fixture_artifacts() -> ReleaseArtifacts {
    let spec = |name: &str, path: &str| ployz_core::install::InstallArtifactSpec {
        version: ployz_core::install::InstallArtifactVersion::try_new("v1").expect("version"),
        source: ployz_core::install::InstallArtifactSource::try_new(format!("/tmp/{name}"))
            .expect("source"),
        sha256: ployz_core::install::InstallSha256Digest::try_new(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("digest"),
        install_path: ployz_core::install::AbsoluteInstallPath::try_new(path)
            .expect("install path"),
    };
    ReleaseArtifacts {
        ployzd: spec("ployzd", "/usr/local/bin/ployzd"),
        ebpf_bytecode: spec("ebpf", "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"),
        ebpf_ctl: spec("ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
        corrosion: spec("corrosion", "/usr/local/bin/corrosion"),
        corrosion_schema: spec("schema", "/usr/local/lib/ployz/corrosion-schema-v1.sql"),
    }
}

fn remote_artifacts(directory: &Path, digest: &str) -> ReleaseArtifacts {
    let spec = |name: &str| ployz_core::install::InstallArtifactSpec {
        version: ployz_core::install::InstallArtifactVersion::try_new("v1").expect("version"),
        source: ployz_core::install::InstallArtifactSource::try_new(format!(
            "https://releases.example/{name}"
        ))
        .expect("remote source"),
        sha256: ployz_core::install::InstallSha256Digest::try_new(digest).expect("digest"),
        install_path: ployz_core::install::AbsoluteInstallPath::try_new(
            directory.join("installed").join(name).display().to_string(),
        )
        .expect("install path"),
    };
    ReleaseArtifacts {
        ployzd: spec("ployzd"),
        ebpf_bytecode: spec("ebpf"),
        ebpf_ctl: spec("ebpf-ctl"),
        corrosion: spec("corrosion"),
        corrosion_schema: spec("schema"),
    }
}

fn prepare_plain_founding<R: HostRunnerCommandRunner>(
    state: &FoundingStateDirectory,
    artifacts: ReleaseArtifacts,
    runner: R,
) -> PreparedLinuxFounding<R> {
    try_prepare_plain_founding(state, artifacts, runner).expect("plain preparation succeeds")
}

fn try_prepare_plain_founding<R: HostRunnerCommandRunner>(
    state: &FoundingStateDirectory,
    artifacts: ReleaseArtifacts,
    runner: R,
) -> Result<PreparedLinuxFounding<R>, FoundingPreparationError> {
    let mut founding_input = input("2026-08-04T12:00:00Z");
    founding_input.storage = InitStorageChoice::Flag {
        mode: StorageMode::Plain,
    };
    prepare_linux_founding(
        state,
        founding_input,
        artifacts,
        "corrosion 0.2.0-beta.0".to_owned(),
        runner,
    )
}

fn pem_der(pem: &str) -> Vec<u8> {
    let encoded = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("valid certificate PEM")
}

fn output(success: bool, stdout: &str) -> HostRunnerCommandOutput {
    HostRunnerCommandOutput {
        success,
        exit_code: success.then_some(0),
        stdout: stdout.to_owned(),
        stdout_truncated: false,
        failure: if success {
            String::new()
        } else {
            "not installed".to_owned()
        },
    }
}
