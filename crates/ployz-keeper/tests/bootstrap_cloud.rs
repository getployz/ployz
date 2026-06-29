mod support;

use ployz_core::install::{
    MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTrustedNats,
};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::FailureMessage;
use ployz_core::security::NatsPrincipal;
use ployz_keeper::cloud_bootstrap::{
    CloudAttemptStateError, CloudBootstrapLocalState, cloud_joiner_connect_config,
    inspect_cloud_bootstrap_local_state, load_or_create_cloud_attempt,
    persist_cloud_terminal_callback, write_cloud_joiner_trusted_ca,
};
use ployz_keeper::cloud_client::{endpoint_url, validate_same_origin_url};
use ployz_keeper::join::{JOIN_MATERIAL_DIR, JOIN_NATS_CREDENTIALS_FILE};
use ployz_nats::connect::{NatsClientAuth, NatsTlsTrust};
use ployz_sdk_types::{
    CloudBootstrapAttemptId, CloudBootstrapCallbackRequest, CloudBootstrapCallbackToken,
    CloudBootstrapEnvelope, CloudBootstrapFailure, CloudBootstrapIntent, CloudBootstrapOutcome,
    CloudBootstrapRedemptionId, CloudJoinerBootstrap, MachineJoinToken,
};
use support::bootstrap::{test_ca_pem, unique_temp_path};

#[test]
fn cloud_client_keeps_callback_authority_on_the_cloud_origin() {
    assert_eq!(
        endpoint_url("https://cloud.example.com", "/api/bootstrap/sessions")
            .expect("endpoint builds"),
        "https://cloud.example.com/api/bootstrap/sessions"
    );
    assert!(
        validate_same_origin_url(
            "https://cloud.example.com",
            "https://cloud.example.com/api/bootstrap/redemptions/pcbr_1/callback"
        )
        .is_ok()
    );
    assert!(
        validate_same_origin_url(
            "https://cloud.example.com",
            "https://evil.example.com/api/bootstrap/redemptions/pcbr_1/callback"
        )
        .is_err()
    );
    assert!(
        validate_same_origin_url(
            "https://cloud.example.com",
            "https://cloud.example.com/api/bootstrap/redemptions/pcbr_1/callback?token=secret"
        )
        .is_err()
    );
}

#[test]
fn cloud_attempt_persists_one_terminal_callback_payload() {
    let root = temp_root("terminal-callback");
    let state_dir = root.join("state");
    let attempt = load_or_create_cloud_attempt(&state_dir).expect("attempt creates");
    let callback = failed_callback(
        attempt.attempt_id.clone(),
        CloudBootstrapRedemptionId::try_new("pcbr_123").expect("valid redemption id"),
        "install failed",
    );
    let envelope = cloud_joiner_envelope(&callback);

    let persisted = persist_cloud_terminal_callback(&state_dir, envelope.clone(), callback.clone())
        .expect("callback persists");
    let replayed =
        persist_cloud_terminal_callback(&state_dir, envelope, callback).expect("callback replays");
    let conflicting = failed_callback(
        attempt.attempt_id,
        CloudBootstrapRedemptionId::try_new("pcbr_456").expect("valid redemption id"),
        "install failed",
    );
    let conflicting_envelope = cloud_bootstrap_envelope(&conflicting);

    assert_eq!(persisted, replayed);
    assert!(persisted.terminal.is_some());
    let persisted_json = std::fs::read_to_string(state_dir.join("cloud-bootstrap-attempt.json"))
        .expect("state reads");
    assert!(!persisted_json.contains("join_once_123"));
    assert!(!persisted_json.contains(join_seed().secret()));
    assert!(matches!(
        persist_cloud_terminal_callback(&state_dir, conflicting_envelope, conflicting),
        Err(CloudAttemptStateError::ConflictingTerminalCallback { .. })
    ));
}

#[test]
fn cloud_attempt_rejects_terminal_callback_for_a_different_envelope() {
    let root = temp_root("terminal-envelope-mismatch");
    let state_dir = root.join("state");
    let callback = failed_callback(
        CloudBootstrapAttemptId::try_new("pcba_123").expect("valid attempt id"),
        CloudBootstrapRedemptionId::try_new("pcbr_123").expect("valid redemption id"),
        "install failed",
    );
    let mismatched_envelope = cloud_bootstrap_envelope(&failed_callback(
        CloudBootstrapAttemptId::try_new("pcba_123").expect("valid attempt id"),
        CloudBootstrapRedemptionId::try_new("pcbr_456").expect("valid redemption id"),
        "install failed",
    ));

    assert!(matches!(
        persist_cloud_terminal_callback(&state_dir, mismatched_envelope, callback),
        Err(CloudAttemptStateError::TerminalCallbackEnvelopeMismatch { .. })
    ));
}

#[test]
fn cloud_preflight_allows_same_attempt_but_refuses_existing_machine_evidence() {
    let root = temp_root("preflight");
    let state_dir = root.join("state");
    let systemd_dir = root.join("systemd");

    assert_eq!(
        inspect_cloud_bootstrap_local_state(&state_dir, &systemd_dir)
            .expect("fresh state inspects"),
        CloudBootstrapLocalState::Fresh
    );

    load_or_create_cloud_attempt(&state_dir).expect("attempt creates");
    assert_eq!(
        inspect_cloud_bootstrap_local_state(&state_dir, &systemd_dir)
            .expect("partial state inspects"),
        CloudBootstrapLocalState::PartialSameAttempt
    );

    let material_dir = state_dir.join(JOIN_MATERIAL_DIR);
    std::fs::create_dir_all(&material_dir).expect("material dir creates");
    std::fs::write(material_dir.join(JOIN_NATS_CREDENTIALS_FILE), "seed")
        .expect("machine evidence writes");

    assert!(matches!(
        inspect_cloud_bootstrap_local_state(&state_dir, &systemd_dir)
            .expect("machine evidence inspects"),
        CloudBootstrapLocalState::AlreadyBootstrapped { .. }
    ));
}

#[test]
fn cloud_joiner_envelope_writes_trust_and_uses_join_principal_credentials() {
    let root = temp_root("joiner-envelope");
    let ca_file = root.join("trust/ca.pem");
    let joiner = cloud_joiner_bootstrap();

    write_cloud_joiner_trusted_ca(&joiner, &ca_file).expect("trusted CA writes");
    let connect = cloud_joiner_connect_config(&joiner, ca_file.clone()).expect("config builds");

    assert_eq!(
        std::fs::read_to_string(&ca_file).expect("ca reads"),
        test_ca_pem().as_str()
    );
    assert_eq!(connect.url.as_str(), "tls://203.0.113.10:4222");
    assert_eq!(connect.principal, NatsPrincipal::Join);
    assert_eq!(connect.trust, NatsTlsTrust::ClusterCa(ca_file));
    let NatsClientAuth::NkeySeed(seed) = connect.auth;
    assert_eq!(seed.secret(), join_seed().secret());
}

fn failed_callback(
    attempt_id: CloudBootstrapAttemptId,
    redemption_id: CloudBootstrapRedemptionId,
    message: &str,
) -> CloudBootstrapCallbackRequest {
    CloudBootstrapCallbackRequest {
        attempt_id,
        redemption_id,
        outcome: CloudBootstrapOutcome::Failed {
            failure: CloudBootstrapFailure::BootstrapFailed {
                message: FailureMessage::try_new(message).expect("valid failure message"),
            },
        },
    }
}

fn cloud_bootstrap_envelope(callback: &CloudBootstrapCallbackRequest) -> CloudBootstrapEnvelope {
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

fn cloud_joiner_envelope(callback: &CloudBootstrapCallbackRequest) -> CloudBootstrapEnvelope {
    CloudBootstrapEnvelope {
        attempt_id: callback.attempt_id.clone(),
        redemption_id: callback.redemption_id.clone(),
        callback_url: format!(
            "https://cloud.example.com/api/bootstrap/redemptions/{}/callback",
            callback.redemption_id.as_str()
        ),
        callback_token: CloudBootstrapCallbackToken::try_new("pcbc_123")
            .expect("valid callback token"),
        intent: CloudBootstrapIntent::Joiner {
            joiner: Box::new(cloud_joiner_bootstrap()),
        },
    }
}

fn cloud_joiner_bootstrap() -> CloudJoinerBootstrap {
    CloudJoinerBootstrap {
        runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222")
            .expect("valid runtime NATS URL"),
        trusted_nats: MachineJoinTrustedNats {
            ca_pem: test_ca_pem(),
        },
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
        join_secret_delivery: MachineJoinSecretDelivery {
            nats_credentials: join_seed(),
        },
    }
}

fn join_seed() -> NatsUserSeed {
    NatsUserSeed::try_new("SUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        .expect("valid NATS user seed")
}

fn temp_root(label: &str) -> std::path::PathBuf {
    let root = unique_temp_path(&format!("ployz-keeper-bootstrap-cloud-{label}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp root creates");
    root
}
