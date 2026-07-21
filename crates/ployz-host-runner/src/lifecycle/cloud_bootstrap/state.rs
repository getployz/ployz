//! Local helpers for typed Cloud bootstrap envelopes.

use std::fs;
use std::path::{Path, PathBuf};

use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust};
use ployz_sdk_types::{
    CloudBootstrapAttemptId, CloudBootstrapCallbackRequest, CloudBootstrapCallbackToken,
    CloudBootstrapEnvelope, CloudBootstrapOutcome, CloudBootstrapRedemptionId,
    CloudJoinerBootstrap, CloudJoinerBootstrapResult, MachineJoinRedeemed,
};
use serde::{Deserialize, Serialize};

use super::super::machine_join::{
    JOIN_MATERIAL_DIR, JOIN_MATERIAL_FILE, JOIN_NATS_CREDENTIALS_FILE,
};
use crate::execution::{FileMode, write_durable_file};

const CLOUD_ATTEMPT_FILE: &str = "cloud-bootstrap-attempt.json";

pub fn write_cloud_joiner_trusted_ca(
    joiner: &CloudJoinerBootstrap,
    ca_file: &Path,
) -> Result<(), CloudJoinerEnvelopeError> {
    if let Some(parent) = ca_file.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CloudJoinerEnvelopeError::CreateTrustDir {
                path: parent.to_path_buf(),
                message: error.to_string(),
            }
        })?;
    }
    std::fs::write(ca_file, joiner.trusted_nats.ca_pem.as_str()).map_err(|error| {
        CloudJoinerEnvelopeError::WriteTrustedCa {
            path: ca_file.to_path_buf(),
            message: error.to_string(),
        }
    })
}

pub fn cloud_joiner_connect_config(
    joiner: &CloudJoinerBootstrap,
    ca_file: PathBuf,
) -> Result<NatsConnectConfig, CloudJoinerEnvelopeError> {
    let url = NatsClientUrl::try_new(joiner.runtime_nats_url.as_str()).map_err(|error| {
        CloudJoinerEnvelopeError::InvalidRuntimeNatsUrl {
            message: format!("{error:?}"),
        }
    })?;
    Ok(NatsConnectConfig {
        url,
        auth: NatsClientAuth::NkeySeed(joiner.join_secret_delivery.nats_credentials.clone()),
        trust: NatsTlsTrust::ClusterCa(ca_file),
        principal: NatsPrincipal::Join,
    })
}

#[must_use]
pub fn cloud_joiner_success_callback(
    attempt_id: CloudBootstrapAttemptId,
    redemption_id: CloudBootstrapRedemptionId,
    redeemed: &MachineJoinRedeemed,
) -> CloudBootstrapCallbackRequest {
    CloudBootstrapCallbackRequest {
        attempt_id,
        redemption_id,
        outcome: CloudBootstrapOutcome::JoinerSucceeded {
            result: CloudJoinerBootstrapResult {
                operation_id: redeemed.operation_id.clone(),
                machine_id: redeemed.machine_id.clone(),
                name: redeemed.name.clone(),
                last_event_sequence: redeemed.last_event_sequence,
                result: redeemed.result,
            },
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudBootstrapAttemptState {
    pub attempt_id: CloudBootstrapAttemptId,
    #[serde(default)]
    pub terminal: Option<CloudBootstrapTerminalCallback>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudBootstrapTerminalCallback {
    pub target: CloudBootstrapCallbackTarget,
    pub callback: CloudBootstrapCallbackRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudBootstrapCallbackTarget {
    pub attempt_id: CloudBootstrapAttemptId,
    pub redemption_id: CloudBootstrapRedemptionId,
    pub callback_url: String,
    pub callback_token: CloudBootstrapCallbackToken,
}

impl CloudBootstrapCallbackTarget {
    #[must_use]
    pub fn from_envelope(envelope: &CloudBootstrapEnvelope) -> Self {
        Self {
            attempt_id: envelope.attempt_id.clone(),
            redemption_id: envelope.redemption_id.clone(),
            callback_url: envelope.callback_url.clone(),
            callback_token: envelope.callback_token.clone(),
        }
    }
}

pub fn load_existing_cloud_attempt(
    state_dir: &Path,
) -> Result<Option<CloudBootstrapAttemptState>, CloudAttemptStateError> {
    let path = state_dir.join(CLOUD_ATTEMPT_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|error| CloudAttemptStateError::Read {
        path: path.clone(),
        message: error.to_string(),
    })?;
    let state: CloudBootstrapAttemptState =
        serde_json::from_slice(&bytes).map_err(|error| CloudAttemptStateError::Parse {
            path: path.clone(),
            message: error.to_string(),
        })?;
    validate_cloud_attempt_state(&path, &state)?;
    Ok(Some(state))
}

pub fn load_or_create_cloud_attempt(
    state_dir: &Path,
) -> Result<CloudBootstrapAttemptState, CloudAttemptStateError> {
    if let Some(state) = load_existing_cloud_attempt(state_dir)? {
        return Ok(state);
    }

    create_cloud_state_dir(state_dir)?;
    let state = CloudBootstrapAttemptState {
        attempt_id: new_attempt_id()?,
        terminal: None,
    };
    write_cloud_attempt_state(state_dir, &state)?;
    Ok(state)
}

pub fn reset_cloud_attempt(
    state_dir: &Path,
) -> Result<CloudBootstrapAttemptState, CloudAttemptStateError> {
    create_cloud_state_dir(state_dir)?;
    let state = CloudBootstrapAttemptState {
        attempt_id: new_attempt_id()?,
        terminal: None,
    };
    write_cloud_attempt_state(state_dir, &state)?;
    Ok(state)
}

fn create_cloud_state_dir(state_dir: &Path) -> Result<(), CloudAttemptStateError> {
    fs::create_dir_all(state_dir).map_err(|error| CloudAttemptStateError::CreateDir {
        path: state_dir.to_path_buf(),
        message: error.to_string(),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700)).map_err(|error| {
            CloudAttemptStateError::Write {
                path: state_dir.to_path_buf(),
                message: error.to_string(),
            }
        })?;
    }
    Ok(())
}

pub fn persist_cloud_terminal_callback(
    state_dir: &Path,
    envelope: CloudBootstrapEnvelope,
    callback: CloudBootstrapCallbackRequest,
) -> Result<CloudBootstrapAttemptState, CloudAttemptStateError> {
    let mut state = load_or_create_cloud_attempt(state_dir)?;
    let target = CloudBootstrapCallbackTarget::from_envelope(&envelope);
    if target.attempt_id != callback.attempt_id
        || target.redemption_id != callback.redemption_id
        || target.attempt_id != state.attempt_id
    {
        return Err(CloudAttemptStateError::TerminalCallbackEnvelopeMismatch {
            path: state_dir.join(CLOUD_ATTEMPT_FILE),
        });
    }
    if let Some(existing) = &state.terminal {
        if existing.callback == callback {
            if existing.target != target {
                return Err(CloudAttemptStateError::ConflictingTerminalEnvelope {
                    path: state_dir.join(CLOUD_ATTEMPT_FILE),
                });
            }
            return Ok(state);
        }
        return Err(CloudAttemptStateError::ConflictingTerminalCallback {
            path: state_dir.join(CLOUD_ATTEMPT_FILE),
        });
    }
    state.terminal = Some(CloudBootstrapTerminalCallback { target, callback });
    write_cloud_attempt_state(state_dir, &state)?;
    Ok(state)
}

fn validate_cloud_attempt_state(
    path: &Path,
    state: &CloudBootstrapAttemptState,
) -> Result<(), CloudAttemptStateError> {
    let Some(terminal) = &state.terminal else {
        return Ok(());
    };
    if terminal.target.attempt_id != state.attempt_id
        || terminal.callback.attempt_id != state.attempt_id
        || terminal.target.redemption_id != terminal.callback.redemption_id
    {
        return Err(CloudAttemptStateError::TerminalCallbackEnvelopeMismatch {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn write_cloud_attempt_state(
    state_dir: &Path,
    state: &CloudBootstrapAttemptState,
) -> Result<(), CloudAttemptStateError> {
    let path = state_dir.join(CLOUD_ATTEMPT_FILE);
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|error| CloudAttemptStateError::Serialize {
            message: error.to_string(),
        })?;
    write_durable_file(state_dir, CLOUD_ATTEMPT_FILE, FileMode::Secret0600, &bytes).map_err(
        |message| CloudAttemptStateError::Write {
            path,
            message: message.as_str().to_owned(),
        },
    )?;
    Ok(())
}

pub fn inspect_cloud_bootstrap_local_state(
    state_dir: &Path,
    supervisor_dir: &Path,
) -> Result<CloudBootstrapLocalState, CloudBootstrapLocalStateError> {
    for path in accepted_machine_evidence_paths(state_dir) {
        if path.exists() {
            return Ok(CloudBootstrapLocalState::AlreadyBootstrapped { evidence: path });
        }
    }
    if supervisor_dir.exists() {
        for entry in fs::read_dir(supervisor_dir).map_err(|error| {
            CloudBootstrapLocalStateError::ReadSupervisorServices {
                path: supervisor_dir.to_path_buf(),
                message: error.to_string(),
            }
        })? {
            let entry =
                entry.map_err(
                    |error| CloudBootstrapLocalStateError::ReadSupervisorServices {
                        path: supervisor_dir.to_path_buf(),
                        message: error.to_string(),
                    },
                )?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("ployzd-") && (name.ends_with(".service") || !name.contains('.')) {
                return Ok(CloudBootstrapLocalState::AlreadyBootstrapped {
                    evidence: entry.path(),
                });
            }
        }
    }

    if state_dir.join(CLOUD_ATTEMPT_FILE).exists() {
        return Ok(CloudBootstrapLocalState::PartialSameAttempt);
    }
    Ok(CloudBootstrapLocalState::Fresh)
}

fn accepted_machine_evidence_paths(state_dir: &Path) -> [PathBuf; 7] {
    [
        state_dir.join(JOIN_MATERIAL_DIR).join(JOIN_MATERIAL_FILE),
        state_dir
            .join(JOIN_MATERIAL_DIR)
            .join(JOIN_NATS_CREDENTIALS_FILE),
        state_dir.join("nats/ca.pem"),
        state_dir.join("nats/controller.seed"),
        state_dir.join("nats/operator.seed"),
        state_dir.join("nats/join.seed"),
        state_dir.join("nats/machine.seed"),
    ]
}

fn new_attempt_id() -> Result<CloudBootstrapAttemptId, CloudAttemptStateError> {
    CloudBootstrapAttemptId::try_new(format!("pcba_{}", nuid::next())).map_err(|error| {
        CloudAttemptStateError::InvalidGeneratedAttempt {
            message: format!("{error:?}"),
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudBootstrapLocalState {
    Fresh,
    PartialSameAttempt,
    AlreadyBootstrapped { evidence: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CloudAttemptStateError {
    #[error("failed to create cloud bootstrap state directory {}: {message}", path.display())]
    CreateDir { path: PathBuf, message: String },
    #[error("failed to read cloud bootstrap attempt {}: {message}", path.display())]
    Read { path: PathBuf, message: String },
    #[error("failed to parse cloud bootstrap attempt {}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("failed to serialize cloud bootstrap attempt: {message}")]
    Serialize { message: String },
    #[error("failed to write cloud bootstrap attempt {}: {message}", path.display())]
    Write { path: PathBuf, message: String },
    #[error("failed to create cloud bootstrap attempt id: {message}")]
    InvalidGeneratedAttempt { message: String },
    #[error(
        "cloud bootstrap attempt {} cannot persist a terminal callback for a different envelope",
        path.display()
    )]
    TerminalCallbackEnvelopeMismatch { path: PathBuf },
    #[error(
        "cloud bootstrap attempt {} already has a different terminal callback payload",
        path.display()
    )]
    ConflictingTerminalCallback { path: PathBuf },
    #[error(
        "cloud bootstrap attempt {} already has a different terminal callback envelope",
        path.display()
    )]
    ConflictingTerminalEnvelope { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CloudBootstrapLocalStateError {
    #[error("failed to read supervisor service directory {}: {message}", path.display())]
    ReadSupervisorServices { path: PathBuf, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CloudJoinerEnvelopeError {
    #[error("cloud joiner runtime NATS URL is invalid: {message}")]
    InvalidRuntimeNatsUrl { message: String },
    #[error("failed to create cloud joiner trust directory {}: {message}", path.display())]
    CreateTrustDir { path: PathBuf, message: String },
    #[error("failed to write cloud joiner trusted CA {}: {message}", path.display())]
    WriteTrustedCa { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        CloudBootstrapLocalState, JOIN_MATERIAL_DIR, JOIN_NATS_CREDENTIALS_FILE,
        cloud_joiner_connect_config, cloud_joiner_success_callback, write_cloud_joiner_trusted_ca,
    };
    use ployz_core::ids::{MachineId, OperationId};
    use ployz_core::install::{
        ExactPloyzVersion, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
        MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinSubstrateRelease,
        MachineJoinTrustedNats,
    };
    use ployz_core::machine::{JoinTokenRedeemedAt, MachineName};
    use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
    use ployz_core::roles::InstallRolePolicy;
    use ployz_core::security::NatsPrincipal;
    use ployz_nats::connect::{NatsClientAuth, NatsTlsTrust};
    use ployz_sdk_types::{
        CloudBootstrapAttemptId, CloudBootstrapCallbackRequest, CloudBootstrapCallbackToken,
        CloudBootstrapEnvelope, CloudBootstrapIntent, CloudBootstrapRedemptionId,
        CloudJoinerBootstrap, MachineJoinRedeemResult, MachineJoinRedeemed, MachineJoinToken,
    };

    #[test]
    fn cloud_joiner_envelope_writes_trust_and_connects_as_join_principal() {
        let root = std::env::temp_dir().join(format!(
            "ployz-cloud-joiner-envelope-{}",
            std::process::id()
        ));
        let ca_file = root.join("trust/ca.pem");
        let joiner = cloud_joiner_bootstrap();

        write_cloud_joiner_trusted_ca(&joiner, &ca_file).expect("trusted CA writes");
        let config = cloud_joiner_connect_config(&joiner, ca_file.clone()).expect("config builds");

        assert_eq!(
            std::fs::read_to_string(&ca_file).expect("ca reads"),
            joiner.trusted_nats.ca_pem.as_str()
        );
        assert_eq!(config.url.as_str(), "tls://203.0.113.10:4222");
        assert_eq!(config.principal, NatsPrincipal::Join);
        assert_eq!(config.trust, NatsTlsTrust::ClusterCa(ca_file));
        let NatsClientAuth::NkeySeed(seed) = config.auth;
        assert_eq!(
            seed.secret(),
            joiner.join_secret_delivery.nats_credentials.secret()
        );
    }

    #[test]
    fn cloud_joiner_success_callback_omits_join_token_and_nats_seed() {
        let redeemed = machine_join_redeemed();
        let callback = cloud_joiner_success_callback(
            CloudBootstrapAttemptId::try_new("pcba_123").expect("valid attempt id"),
            CloudBootstrapRedemptionId::try_new("pcbr_123").expect("valid redemption id"),
            &redeemed,
        );
        let serialized = serde_json::to_string(&callback).expect("callback serializes");

        assert!(serialized.contains("joiner_succeeded"));
        assert!(serialized.contains("op_machine"));
        assert!(serialized.contains("edge_2"));
        assert!(!serialized.contains("join_once_123"));
        assert!(!serialized.contains("SUAAAAAAAA"));
    }

    #[test]
    fn cloud_attempt_state_persists_stable_attempt_id() {
        let root = temp_root("attempt-state");
        let state_dir = root.join("state");

        let first = super::load_or_create_cloud_attempt(&state_dir).expect("attempt creates");
        let second = super::load_or_create_cloud_attempt(&state_dir).expect("attempt reloads");

        assert_eq!(first, second);
        assert!(first.attempt_id.as_str().starts_with("pcba_"));
    }

    #[test]
    fn cloud_preflight_refuses_existing_machine_material_before_cloud() {
        let root = temp_root("preflight-material");
        let state_dir = root.join("state");
        let systemd_dir = root.join("systemd");
        let material_dir = state_dir.join(JOIN_MATERIAL_DIR);
        std::fs::create_dir_all(&material_dir).expect("material dir");
        std::fs::write(material_dir.join(JOIN_NATS_CREDENTIALS_FILE), "seed").expect("seed writes");

        let state = super::inspect_cloud_bootstrap_local_state(&state_dir, &systemd_dir)
            .expect("preflight inspects");

        assert!(matches!(
            state,
            super::CloudBootstrapLocalState::AlreadyBootstrapped { .. }
        ));
    }

    #[test]
    fn cloud_preflight_detects_partial_same_attempt() {
        let root = temp_root("preflight-partial");
        let state_dir = root.join("state");
        let systemd_dir = root.join("systemd");
        super::load_or_create_cloud_attempt(&state_dir).expect("attempt creates");

        assert_eq!(
            super::inspect_cloud_bootstrap_local_state(&state_dir, &systemd_dir)
                .expect("preflight inspects"),
            super::CloudBootstrapLocalState::PartialSameAttempt
        );
    }

    #[test]
    fn cloud_preflight_detects_existing_openrc_service() {
        let root = tempfile::tempdir().expect("tempdir");
        let state_dir = root.path().join("state");
        let openrc_dir = root.path().join("openrc");
        fs::create_dir_all(&state_dir).expect("state dir");
        fs::create_dir_all(&openrc_dir).expect("OpenRC dir");
        fs::write(openrc_dir.join("ployzd-dns"), "#!/sbin/openrc-run\n").expect("OpenRC service");

        assert!(matches!(
            super::inspect_cloud_bootstrap_local_state(&state_dir, &openrc_dir)
                .expect("local state"),
            CloudBootstrapLocalState::AlreadyBootstrapped { .. }
        ));
    }

    #[test]
    fn cloud_terminal_callback_persistence_accepts_identical_replay_only() {
        let root = temp_root("terminal-callback");
        let state_dir = root.join("state");
        let first = super::load_or_create_cloud_attempt(&state_dir).expect("attempt creates");
        let callback = cloud_joiner_success_callback(
            first.attempt_id.clone(),
            CloudBootstrapRedemptionId::try_new("pcbr_123").expect("valid redemption id"),
            &machine_join_redeemed(),
        );
        let envelope = cloud_bootstrap_envelope(&callback);

        let persisted =
            super::persist_cloud_terminal_callback(&state_dir, envelope.clone(), callback.clone())
                .expect("callback persists");
        let replayed = super::persist_cloud_terminal_callback(&state_dir, envelope, callback)
            .expect("same callback replays");
        let conflicting = cloud_joiner_success_callback(
            first.attempt_id,
            CloudBootstrapRedemptionId::try_new("pcbr_456").expect("valid redemption id"),
            &machine_join_redeemed(),
        );
        let conflicting_envelope = cloud_bootstrap_envelope(&conflicting);

        assert_eq!(persisted, replayed);
        assert!(persisted.terminal.is_some());
        assert!(matches!(
            super::persist_cloud_terminal_callback(&state_dir, conflicting_envelope, conflicting),
            Err(super::CloudAttemptStateError::ConflictingTerminalCallback { .. })
        ));
    }

    fn cloud_joiner_bootstrap() -> CloudJoinerBootstrap {
        CloudJoinerBootstrap {
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222")
                .expect("valid runtime nats url"),
            trusted_nats: MachineJoinTrustedNats {
                ca_pem: NatsCaCertificatePem::try_new(
                    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                )
                .expect("valid ca pem"),
            },
            join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
            join_secret_delivery: machine_join_secret_delivery(),
        }
    }

    fn machine_join_redeemed() -> MachineJoinRedeemed {
        MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
            endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new("10.198.2.0/24")
                .expect("valid subnet"),
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::operation::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        }
    }

    fn machine_join_bundle() -> MachineJoinBundle {
        MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
                dataplane_endpoint_supernet:
                    ployz_core::network::MachineEndpointSupernet::default_v1(),
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                    .expect("valid runtime nats url"),
                trusted_nats: MachineJoinTrustedNats {
                    ca_pem: NatsCaCertificatePem::try_new(
                        "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                    )
                    .expect("valid ca pem"),
                },
                recovery_key_wrapped: ployz_core::install::WrappedCaKey::new(vec![1, 2, 3]),
                core_seeds_wrapped: ployz_core::install::WrappedCoreSeeds::new(vec![4, 5, 6]),
                substrate_release: MachineJoinSubstrateRelease {
                    version: ExactPloyzVersion::try_new("0.1.0").expect("exact release"),
                },
            },
        }
    }

    fn machine_join_secret_delivery() -> MachineJoinSecretDelivery {
        MachineJoinSecretDelivery {
            nats_credentials: NatsUserSeed::try_new(
                "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM",
            )
            .expect("valid nats credentials"),
        }
    }

    fn cloud_bootstrap_envelope(
        callback: &CloudBootstrapCallbackRequest,
    ) -> CloudBootstrapEnvelope {
        CloudBootstrapEnvelope {
            attempt_id: callback.attempt_id.clone(),
            redemption_id: callback.redemption_id.clone(),
            callback_url: format!(
                "https://cloud.example.com/api/bootstrap/redemptions/{}/callback",
                callback.redemption_id.as_str()
            ),
            callback_token: CloudBootstrapCallbackToken::try_new("pcbc_123")
                .expect("valid callback token"),
            intent: CloudBootstrapIntent::WaitForFounder {
                retry_after_seconds: 2,
            },
        }
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ployz-cloud-bootstrap-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root creates");
        root
    }
}
