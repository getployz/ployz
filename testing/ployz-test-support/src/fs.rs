//! Unix file-mode helpers for tests that stage fake binaries and assert
//! secret-material permissions. No-ops on non-unix hosts.

use std::path::Path;

#[cfg(unix)]
pub fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .expect("staged file metadata can be read")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("staged file can be made executable");
}

#[cfg(not(unix))]
pub fn make_executable(_path: &Path) {}

#[cfg(unix)]
pub fn assert_file_mode(path: &Path, expected_mode: u32) {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .expect("file metadata is readable")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, expected_mode);
}

#[cfg(not(unix))]
pub fn assert_file_mode(_path: &Path, _expected_mode: u32) {}
