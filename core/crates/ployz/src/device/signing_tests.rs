use super::{DeviceSigning, key};

#[test]
fn no_key_file_is_no_signing() {
    let dir = tempfile::tempdir().unwrap();
    let path = key::path(&dir.path().join("config.yaml"));
    assert!(DeviceSigning::load(&path).unwrap().is_none());
}
