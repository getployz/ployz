//! Resumable ordering for joining one machine through the public token door.

use std::future::Future;
use std::net::SocketAddr;

use ployz_core::join::{
    JoinAcceptanceValidationError, JoinBlob, JoinStorageChoice, JoinStorageFacts,
    MachineJoinAccepted, MachineJoinRequest, ValidatedMachineJoinAccepted,
};
use ployz_core::machine::MachineName;
use ployz_core::operation::FailureMessage;

use super::{
    MachineJoinIdentity, MachineJoinInspection, MachineJoinLocalRefusal, MachineJoinLock,
    MachineJoinMilestone, MachineJoinStateDirectory, MachineJoinStateError,
};

/// Non-secret host facts combined with the durably retained identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineJoinInput {
    pub name: MachineName,
    pub endpoint: Option<SocketAddr>,
    pub storage_choice: JoinStorageChoice,
    pub storage_facts: JoinStorageFacts,
}

/// A canonical admission request whose identity and host facts are already durable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedMachineJoin {
    blob: JoinBlob,
    request: MachineJoinRequest,
}

impl PreparedMachineJoin {
    #[must_use]
    pub const fn blob(&self) -> &JoinBlob {
        &self.blob
    }

    #[must_use]
    pub const fn request(&self) -> &MachineJoinRequest {
        &self.request
    }
}

/// Persists machine identity and admission facts before any network call.
pub fn prepare_machine_join(
    state: &MachineJoinStateDirectory,
    blob: JoinBlob,
    input: MachineJoinInput,
) -> Result<PreparedMachineJoin, MachineJoinFailure> {
    let lock = state.try_lock()?;
    prepare_machine_join_locked(state, &lock, blob, input)
}

pub(crate) fn prepare_machine_join_locked(
    state: &MachineJoinStateDirectory,
    _lock: &MachineJoinLock,
    blob: JoinBlob,
    input: MachineJoinInput,
) -> Result<PreparedMachineJoin, MachineJoinFailure> {
    let fingerprint = blob.door_cert_fingerprint();
    let identity = match state.inspect(fingerprint)? {
        MachineJoinInspection::Clean => state.ensure_identity(fingerprint, &input.name)?,
        MachineJoinInspection::ReadyToRedeem { identity }
        | MachineJoinInspection::ReadyToActivate { identity }
        | MachineJoinInspection::NoOp { identity } => identity,
        MachineJoinInspection::Refused { refusal } => {
            return Err(MachineJoinFailure::Refused { refusal });
        }
    };
    let proposed = request_from_input(&identity, input);
    let request = match state.read_request()? {
        Some(persisted) => {
            validate_request_identity(&persisted, &identity)?;
            persisted
        }
        None => {
            state.persist_request(&proposed)?;
            proposed
        }
    };
    Ok(PreparedMachineJoin { blob, request })
}

fn request_from_input(
    identity: &MachineJoinIdentity,
    input: MachineJoinInput,
) -> MachineJoinRequest {
    let MachineJoinInput {
        name,
        endpoint,
        storage_choice,
        storage_facts,
    } = input;
    MachineJoinRequest {
        name,
        public_key: identity.public_key().clone(),
        endpoint,
        storage_choice,
        storage_facts,
    }
}

fn validate_request_identity(
    request: &MachineJoinRequest,
    identity: &MachineJoinIdentity,
) -> Result<(), MachineJoinFailure> {
    if request.name != *identity.machine_id() || request.public_key != *identity.public_key() {
        return Err(MachineJoinFailure::PersistedRequestIdentityMismatch);
    }
    Ok(())
}

/// The only network boundary in Host Runner's join workflow.
pub trait MachineJoinDoor {
    fn redeem_machine(
        &mut self,
        blob: &JoinBlob,
        request: &MachineJoinRequest,
    ) -> impl Future<Output = Result<MachineJoinAccepted, FailureMessage>> + Send;
}

/// Idempotent privileged effects for activating one accepted machine.
///
/// Configuration persists the accepted `JoinMachineSubstrate` verbatim at
/// `/var/lib/ployz/join-substrate.json` and points the API role at it through
/// `PLOYZ_API_JOIN_SUBSTRATE_PATH`; this is how a joined machine can serve the
/// same door contract to later members.
pub trait MachineJoinHostEffects {
    fn apply_milestone(
        &mut self,
        milestone: MachineJoinMilestone,
        identity: &MachineJoinIdentity,
        accepted: &ValidatedMachineJoinAccepted,
    ) -> Result<(), FailureMessage>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineJoinOutcomeKind {
    Joined,
    Resumed,
    NoOp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineJoinOutcome {
    pub kind: MachineJoinOutcomeKind,
    pub machine_name: MachineName,
}

/// Redeems once, durably commits validated acceptance, and resumes local activation.
pub async fn execute_machine_join(
    state: &MachineJoinStateDirectory,
    prepared: &PreparedMachineJoin,
    door: &mut impl MachineJoinDoor,
    host: &mut impl MachineJoinHostEffects,
) -> Result<MachineJoinOutcome, MachineJoinFailure> {
    let lock = state.try_lock()?;
    execute_machine_join_locked(state, &lock, prepared, door, host).await
}

pub(crate) async fn execute_machine_join_locked(
    state: &MachineJoinStateDirectory,
    _lock: &MachineJoinLock,
    prepared: &PreparedMachineJoin,
    door: &mut impl MachineJoinDoor,
    host: &mut impl MachineJoinHostEffects,
) -> Result<MachineJoinOutcome, MachineJoinFailure> {
    let fingerprint = prepared.blob.door_cert_fingerprint();
    let inspection = state.inspect(fingerprint)?;
    let (identity, resumed) = match inspection {
        MachineJoinInspection::Clean => return Err(MachineJoinFailure::IdentityNotPrepared),
        MachineJoinInspection::ReadyToRedeem { identity } => (identity, false),
        MachineJoinInspection::ReadyToActivate { identity } => (identity, true),
        MachineJoinInspection::NoOp { .. } => {
            return Ok(MachineJoinOutcome {
                kind: MachineJoinOutcomeKind::NoOp,
                machine_name: prepared.request.name.clone(),
            });
        }
        MachineJoinInspection::Refused { refusal } => {
            return Err(MachineJoinFailure::Refused { refusal });
        }
    };
    validate_request_identity(&prepared.request, &identity)?;

    let accepted = match state.read_acceptance::<MachineJoinAccepted>()? {
        Some(accepted) => accepted
            .try_validate(&prepared.request, fingerprint)
            .map_err(MachineJoinFailure::InvalidAcceptance)?,
        None => {
            let accepted = door
                .redeem_machine(&prepared.blob, &prepared.request)
                .await
                .map_err(|message| MachineJoinFailure::Door { message })?
                .try_validate(&prepared.request, fingerprint)
                .map_err(MachineJoinFailure::InvalidAcceptance)?;
            state.persist_acceptance(accepted.accepted())?;
            accepted
        }
    };
    state.persist_cluster_anchor(&accepted.accepted().cluster.cluster_id)?;

    apply_activation(state, &identity, &accepted, host)?;
    state.mark_complete()?;
    Ok(MachineJoinOutcome {
        kind: if resumed {
            MachineJoinOutcomeKind::Resumed
        } else {
            MachineJoinOutcomeKind::Joined
        },
        machine_name: accepted.accepted().machine.name.clone(),
    })
}

fn apply_activation(
    state: &MachineJoinStateDirectory,
    identity: &MachineJoinIdentity,
    accepted: &ValidatedMachineJoinAccepted,
    host: &mut impl MachineJoinHostEffects,
) -> Result<(), MachineJoinFailure> {
    for milestone in MachineJoinMilestone::ORDERED {
        ensure_step(state, milestone, || {
            if milestone == MachineJoinMilestone::Configuration {
                state
                    .persist_join_substrate(&accepted.accepted().substrate)
                    .map_err(|error| failure(error.to_string()))?;
            }
            host.apply_milestone(milestone, identity, accepted)
        })?;
    }
    Ok(())
}

fn failure(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message.into()).expect("machine join generated a non-empty failure")
}

fn ensure_step(
    state: &MachineJoinStateDirectory,
    milestone: MachineJoinMilestone,
    effect: impl FnOnce() -> Result<(), FailureMessage>,
) -> Result<(), MachineJoinFailure> {
    if state.milestone_complete(milestone)? {
        return Ok(());
    }
    effect().map_err(|message| MachineJoinFailure::Activation { milestone, message })?;
    state.record_milestone(milestone)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum MachineJoinFailure {
    #[error("machine join refused: {refusal}")]
    Refused { refusal: MachineJoinLocalRefusal },
    #[error("machine join identity must be prepared before execution")]
    IdentityNotPrepared,
    #[error("persisted machine join request disagrees with retained identity")]
    PersistedRequestIdentityMismatch,
    #[error("machine join door failed: {message}")]
    Door { message: FailureMessage },
    #[error("machine join door returned invalid acceptance: {0}")]
    InvalidAcceptance(JoinAcceptanceValidationError),
    #[error("machine join host preflight failed: {message}")]
    HostPreflight { message: FailureMessage },
    #[error("machine join activation failed at {milestone:?}: {message}")]
    Activation {
        milestone: MachineJoinMilestone,
        message: FailureMessage,
    },
    #[error(transparent)]
    State(#[from] MachineJoinStateError),
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::SocketAddr;

    use ployz_core::corrosion::{
        AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp,
        MachineDocument, MachineStorageSelection, MachineStorageSelectionReason, MachineTransport,
        MeshProvider, OperationInitiator, OperatorWriteProvenance, StorageMode,
        derive_builtin_wireguard_member,
    };
    use ployz_core::ids::{ClusterName, MachineName, TokenName};
    use ployz_core::install::{
        AbsoluteInstallPath, ExactPloyzVersion, InstallArtifactSource, InstallArtifactSpec,
        InstallArtifactVersion, InstallSha256Digest,
    };
    use ployz_core::join::{
        CorrosionBootstrapFacts, JOIN_DOOR_PORT, JoinDoorCertFingerprint, JoinDoorCertificatePem,
        JoinDoorMaterial, JoinDoorPrivateKeyPem, JoinMachineSubstrate, JoinTokenSecret,
        ReachableSeedMachine,
    };
    use ployz_core::machine::MachineLifecycle;
    use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};

    use super::*;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const TOKEN: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const SEED: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

    struct RecordingDoor {
        accepted: MachineJoinAccepted,
        calls: usize,
    }

    impl MachineJoinDoor for RecordingDoor {
        async fn redeem_machine(
            &mut self,
            _blob: &JoinBlob,
            _request: &MachineJoinRequest,
        ) -> Result<MachineJoinAccepted, FailureMessage> {
            self.calls += 1;
            Ok(self.accepted.clone())
        }
    }

    struct RecordingHost {
        calls: Vec<MachineJoinMilestone>,
        fail_once: Option<MachineJoinMilestone>,
    }

    impl RecordingHost {
        fn pass() -> Self {
            Self {
                calls: Vec::new(),
                fail_once: None,
            }
        }

        fn fail_once(milestone: MachineJoinMilestone) -> Self {
            Self {
                calls: Vec::new(),
                fail_once: Some(milestone),
            }
        }

        fn call(&mut self, milestone: MachineJoinMilestone) -> Result<(), FailureMessage> {
            self.calls.push(milestone);
            if self.fail_once == Some(milestone) {
                self.fail_once = None;
                return Err(failure("injected activation crash"));
            }
            Ok(())
        }
    }

    impl MachineJoinHostEffects for RecordingHost {
        fn apply_milestone(
            &mut self,
            milestone: MachineJoinMilestone,
            _identity: &MachineJoinIdentity,
            _accepted: &ValidatedMachineJoinAccepted,
        ) -> Result<(), FailureMessage> {
            self.call(milestone)
        }
    }

    #[tokio::test]
    async fn persisted_acceptance_resumes_without_redeeming_and_complete_is_no_op() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        let prepared = prepared(&state);
        let mut door = RecordingDoor {
            accepted: accepted(prepared.request(), prepared.blob().door_cert_fingerprint()),
            calls: 0,
        };
        let mut crashing = RecordingHost::fail_once(MachineJoinMilestone::Artifacts);

        let error = execute_machine_join(&state, &prepared, &mut door, &mut crashing)
            .await
            .expect_err("activation crash");
        assert!(matches!(
            error,
            MachineJoinFailure::Activation {
                milestone: MachineJoinMilestone::Artifacts,
                ..
            }
        ));
        assert_eq!(door.calls, 1);
        assert!(matches!(
            state
                .inspect(prepared.blob().door_cert_fingerprint())
                .expect("inspection"),
            MachineJoinInspection::ReadyToActivate { .. }
        ));

        let mut resumed_host = RecordingHost::pass();
        let resumed = execute_machine_join(&state, &prepared, &mut door, &mut resumed_host)
            .await
            .expect("resume succeeds");
        assert_eq!(resumed.kind, MachineJoinOutcomeKind::Resumed);
        assert_eq!(door.calls, 1, "resume never needs the token door");
        assert_eq!(
            resumed_host.calls,
            vec![
                MachineJoinMilestone::Artifacts,
                MachineJoinMilestone::Storage,
                MachineJoinMilestone::Docker,
                MachineJoinMilestone::DoorMaterial,
                MachineJoinMilestone::Configuration,
                MachineJoinMilestone::BootstrapWireguard,
                MachineJoinMilestone::UnitsInstalled,
                MachineJoinMilestone::CorrosionStarted,
                MachineJoinMilestone::RosterConverged,
                MachineJoinMilestone::KeeperStarted,
                MachineJoinMilestone::ApiStarted,
                MachineJoinMilestone::EndpointNetworkReady,
                MachineJoinMilestone::GatewayStarted,
                MachineJoinMilestone::DnsStarted,
                MachineJoinMilestone::DnsReady,
                MachineJoinMilestone::Ready,
                MachineJoinMilestone::BootstrapCleaned,
            ]
        );

        let mut no_op_host = RecordingHost::pass();
        let no_op = execute_machine_join(&state, &prepared, &mut door, &mut no_op_host)
            .await
            .expect("complete matching join is a no-op");
        assert_eq!(no_op.kind, MachineJoinOutcomeKind::NoOp);
        assert_eq!(door.calls, 1);
        assert!(no_op_host.calls.is_empty());
    }

    #[tokio::test]
    async fn observed_activation_effects_are_skipped() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        let prepared = prepared(&state);
        state
            .record_milestone(MachineJoinMilestone::Artifacts)
            .expect("artifact observed");
        state
            .record_milestone(MachineJoinMilestone::Storage)
            .expect("storage observed");
        state
            .record_milestone(MachineJoinMilestone::Docker)
            .expect("Docker observed");
        let mut door = RecordingDoor {
            accepted: accepted(prepared.request(), prepared.blob().door_cert_fingerprint()),
            calls: 0,
        };
        let mut host = RecordingHost::pass();

        execute_machine_join(&state, &prepared, &mut door, &mut host)
            .await
            .expect("join");

        assert_eq!(
            host.calls.first(),
            Some(&MachineJoinMilestone::DoorMaterial)
        );
        assert!(!host.calls.contains(&MachineJoinMilestone::Artifacts));
        assert!(!host.calls.contains(&MachineJoinMilestone::Storage));
        assert!(!host.calls.contains(&MachineJoinMilestone::Docker));
    }

    #[tokio::test]
    async fn offline_foreign_refusal_has_no_door_or_host_effects() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        let prepared = prepared(&state);
        std::fs::write(directory.path().join("founding-complete"), b"complete\n")
            .expect("foreign evidence");
        let mut door = RecordingDoor {
            accepted: accepted(prepared.request(), prepared.blob().door_cert_fingerprint()),
            calls: 0,
        };
        let mut host = RecordingHost::pass();

        let error = execute_machine_join(&state, &prepared, &mut door, &mut host)
            .await
            .expect_err("foreign state refuses");

        assert!(matches!(error, MachineJoinFailure::Refused { .. }));
        assert_eq!(door.calls, 0);
        assert!(host.calls.is_empty());
    }

    #[tokio::test]
    async fn invalid_acceptance_is_rejected_before_persistence_or_host_work() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        let prepared = prepared(&state);
        let mut invalid = accepted(prepared.request(), prepared.blob().door_cert_fingerprint());
        invalid.machine.name = MachineName::try_new("other-machine").expect("machine name");
        let mut door = RecordingDoor {
            accepted: invalid,
            calls: 0,
        };
        let mut host = RecordingHost::pass();

        let error = execute_machine_join(&state, &prepared, &mut door, &mut host)
            .await
            .expect_err("invalid response");

        assert!(matches!(
            error,
            MachineJoinFailure::InvalidAcceptance(
                JoinAcceptanceValidationError::AcceptedNameMismatch
            )
        ));
        assert!(matches!(
            state
                .inspect(prepared.blob().door_cert_fingerprint())
                .expect("inspection"),
            MachineJoinInspection::ReadyToRedeem { .. }
        ));
        assert!(host.calls.is_empty());
    }

    #[test]
    fn preparation_lock_refuses_race_without_writes_then_resumes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        let lock = state.try_lock().expect("first invocation owns lock");

        let error = prepare_machine_join(&state, join_blob(), join_input())
            .expect_err("concurrent preparation refuses");

        assert!(matches!(
            error,
            MachineJoinFailure::State(MachineJoinStateError::JoinInProgress)
        ));
        assert!(!directory.path().join("join-identity.json").exists());
        assert!(!directory.path().join("join-request.json").exists());

        drop(lock);
        let prepared = prepare_machine_join(&state, join_blob(), join_input())
            .expect("same preparation resumes after lock release");
        assert!(directory.path().join("join-identity.json").is_file());
        assert!(directory.path().join("join-request.json").is_file());
        assert_eq!(prepared.request().name.as_str(), "edge-a");
    }

    fn prepared(state: &MachineJoinStateDirectory) -> PreparedMachineJoin {
        prepare_machine_join(state, join_blob(), join_input()).expect("prepared")
    }

    pub(crate) fn join_blob() -> JoinBlob {
        JoinBlob::try_new(
            TokenName::try_new(TOKEN).expect("token"),
            JoinTokenSecret::try_from_bytes([7; 32]),
            JoinDoorCertFingerprint::try_new("ab".repeat(32)).expect("fingerprint"),
            vec![SocketAddr::new(
                "198.51.100.10".parse().expect("IP"),
                JOIN_DOOR_PORT,
            )],
        )
        .expect("blob")
    }

    pub(crate) fn join_input() -> MachineJoinInput {
        MachineJoinInput {
            name: MachineName::try_new("edge-a").expect("name"),
            endpoint: None,
            storage_choice: JoinStorageChoice::Automatic,
            storage_facts: JoinStorageFacts {
                imported_zfs_pool: false,
                total_memory_bytes: 1024,
            },
        }
    }

    pub(crate) fn accepted(
        request: &MachineJoinRequest,
        fingerprint: &JoinDoorCertFingerprint,
    ) -> MachineJoinAccepted {
        let cluster_id = ClusterName::try_new(CLUSTER).expect("cluster");
        let seed_id = MachineName::try_new(SEED).expect("seed");
        let provenance = OperatorWriteProvenance {
            written_by: OperationInitiator::Machine {
                machine_id: seed_id.clone(),
            },
            written_at: CorrosionTimestamp::try_new("2026-08-05T12:00:00Z").expect("timestamp"),
        };
        let cluster = ClusterDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id.clone(),
            provenance: provenance.clone(),
            name: "acme".to_owned(),
            storage_default: StorageMode::Plain,
            hostname_mode: AutomaticHostnameMode::Disabled,
            prefix: MachineEndpointSupernet::try_new("10.210.0.0/16").expect("prefix"),
            provider: MeshProvider::BuiltinWireguard,
            acme_directory_url: "https://acme.invalid/directory".to_owned(),
            acme_contact: None,
        };
        let member_addr = derive_builtin_wireguard_member(&cluster_id, &request.public_key)
            .bind_address()
            .get();
        let machine = MachineDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id.clone(),
            provenance,
            name: request.name.clone(),
            lifecycle: MachineLifecycle::Active,
            transport: MachineTransport::Wireguard {
                pubkey: request.public_key.clone(),
                addr_v6: member_addr,
                endpoint: request.endpoint,
                subnet_v4: MachineEndpointSubnet::try_new("10.210.20.0/24").expect("subnet"),
            },
            storage: MachineStorageSelection {
                mode: StorageMode::Plain,
                reason: MachineStorageSelectionReason::Default,
            },
        };
        let seed_public_key =
            WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("seed key");
        let seed_addr = derive_builtin_wireguard_member(&cluster_id, &seed_public_key)
            .bind_address()
            .get();
        let seed_transport = MachineTransport::Wireguard {
            pubkey: seed_public_key,
            addr_v6: seed_addr,
            endpoint: Some(SocketAddr::new(
                "198.51.100.10".parse().expect("IP"),
                51_820,
            )),
            subnet_v4: MachineEndpointSubnet::try_new("10.210.1.0/24").expect("seed subnet"),
        };
        MachineJoinAccepted {
            cluster,
            machine,
            seed: ReachableSeedMachine::try_new(seed_id, seed_transport).expect("seed"),
            door: JoinDoorMaterial {
                certificate_pem: JoinDoorCertificatePem::try_new("certificate").expect("cert"),
                private_key_pem: JoinDoorPrivateKeyPem::try_new("private-key").expect("key"),
                fingerprint: fingerprint.clone(),
            },
            corrosion: CorrosionBootstrapFacts::try_new(SocketAddr::new(seed_addr.into(), 8_787))
                .expect("corrosion"),
            substrate: JoinMachineSubstrate::try_new(
                ExactPloyzVersion::try_new("0.1.0").expect("version"),
                "0.3.1",
                [
                    "/usr/local/bin/ployzd",
                    "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                    "/usr/local/bin/ployz-ebpf-ctl",
                    "/usr/local/bin/corrosion",
                    "/usr/local/lib/ployz/corrosion-schema-v1.sql",
                ]
                .into_iter()
                .enumerate()
                .map(|(index, path)| InstallArtifactSpec {
                    version: InstallArtifactVersion::try_new("0.1.0").expect("version"),
                    source: InstallArtifactSource::try_new(format!(
                        "https://example.invalid/artifact-{index}"
                    ))
                    .expect("source"),
                    sha256: InstallSha256Digest::try_new(format!("{index:02x}").repeat(32))
                        .expect("digest"),
                    install_path: AbsoluteInstallPath::try_new(path).expect("path"),
                })
                .collect(),
            )
            .expect("substrate"),
        }
    }
}
