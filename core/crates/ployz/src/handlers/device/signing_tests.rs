use ployz_core::authority::{Signer, SignerKey, Timestamp, validity};

use super::{DeviceSigning, key};

const SEED: [u8; 32] = [7; 32];

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn signing(dir: &tempfile::TempDir) -> DeviceSigning {
    let path = key::path(&dir.path().join("config.yaml"));
    assert!(key::store(&path, &SEED).unwrap());
    DeviceSigning::load(&path).unwrap().unwrap()
}

#[test]
fn a_window_opens_at_its_time_and_lasts_the_longest_a_machine_takes() {
    let dir = tempfile::tempdir().unwrap();
    let window = signing(&dir).window_from(at(1_700_000_000_000));
    assert_eq!(window.issued_at, at(1_700_000_000_000));
    assert_eq!(window.expires_at, at(1_700_000_900_000));
    assert_eq!(
        window.expires_at,
        window.issued_at.plus(validity::MAX_LIFETIME_MILLIS)
    );
    assert_eq!(
        window.signer,
        SignerKey::Device(Signer::from_seed(&SEED).public().into())
    );
}

#[test]
fn no_key_file_is_no_signing() {
    let dir = tempfile::tempdir().unwrap();
    let path = key::path(&dir.path().join("config.yaml"));
    assert!(DeviceSigning::load(&path).unwrap().is_none());
}
