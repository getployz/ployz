use ployz_core::authority::{
    RestartApproval, Signed, Signer, SignerKey, Timestamp, WorkflowKind, restart,
};
use ployz_core::{ContainerId, MachineId, Namespace, OperationId, SealingPublicKey, ServiceName};

use super::{ApprovalWindow, DeviceSigning, key};

const SEED: [u8; 32] = [7; 32];

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn signing(dir: &tempfile::TempDir) -> DeviceSigning {
    let path = key::path(&dir.path().join("config.yaml"));
    assert!(key::store(&path, &SEED).unwrap());
    DeviceSigning::load(&path).unwrap().unwrap()
}

fn body(window: ApprovalWindow) -> RestartApproval {
    RestartApproval {
        cluster: SealingPublicKey::from_bytes([1; 32]),
        operation: OperationId::from_uuid("0f8fad5b-d9cb-469f-a165-70867728950e").unwrap(),
        kind: WorkflowKind::Restart,
        namespace: Namespace::parse("app").unwrap(),
        service: ServiceName::parse("web").unwrap(),
        machines: vec![MachineId::parse("a".repeat(32)).unwrap()],
        container: ContainerId::parse("b".repeat(64)).unwrap(),
        issued_at: window.issued_at,
        expires_at: window.expires_at,
        signer: window.signer,
        proposal: None,
    }
}

#[test]
fn a_window_opens_at_its_time_and_lasts_the_longest_a_machine_takes() {
    let dir = tempfile::tempdir().unwrap();
    let window = signing(&dir).window_from(at(1_700_000_000_000));
    assert_eq!(window.issued_at, at(1_700_000_000_000));
    assert_eq!(window.expires_at, at(1_700_000_900_000));
    assert_eq!(
        window.expires_at,
        window.issued_at.plus(restart::MAX_LIFETIME_MILLIS)
    );
    assert_eq!(
        window.signer,
        SignerKey::Device(Signer::from_seed(&SEED).public().into())
    );
}

#[test]
fn a_device_signs_the_same_bytes_as_a_body_written_out_by_hand() {
    let dir = tempfile::tempdir().unwrap();
    let signing = signing(&dir);
    let by_hand = {
        let signer = Signer::from_seed(&SEED);
        let issued_at = at(1_700_000_000_000);
        let hand = body(ApprovalWindow {
            issued_at,
            expires_at: issued_at.plus(restart::MAX_LIFETIME_MILLIS),
            signer: SignerKey::Device(signer.public().into()),
        });
        Signed::sign(&hand, &signer).unwrap()
    };
    let signed = signing
        .sign(&body(signing.window_from(at(1_700_000_000_000))))
        .unwrap();
    assert_eq!(signed, by_hand);
    assert_eq!(serde_json::to_string(&signed).unwrap(), GOLDEN);
}

#[test]
fn no_key_file_is_no_signing() {
    let dir = tempfile::tempdir().unwrap();
    let path = key::path(&dir.path().join("config.yaml"));
    assert!(DeviceSigning::load(&path).unwrap().is_none());
}

const GOLDEN: &str = r#"{"body":"{\"cluster\":\"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=\",\"container\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"expires_at\":1700000900000,\"issued_at\":1700000000000,\"kind\":\"restart\",\"machines\":[\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"],\"namespace\":\"app\",\"operation\":\"0f8fad5bd9cb469fa16570867728950e\",\"service\":\"web\",\"signer\":\"ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c\"}","signatures":[{"key":"ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c","signature":"648a24652dc46cfe5a8b53091ac365dea554dd9195ff5cca364b36424ab619e79b6785199ca37f88044923144babadec4b6a21f9ad4fed715eeaf55202505e07"}]}"#;
