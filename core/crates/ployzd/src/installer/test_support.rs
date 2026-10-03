//! Fixtures for installer contract tests that re-run the test binary against fake host commands.

use std::{env, ffi::OsString, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use tempfile::TempDir;

/// Re-runs `test` (its full path, e.g. `installer::tests::x`) with `PATH` set to `root/commands`
/// and `key=value` naming the case, then checks the child wrote `completion`.
pub(super) fn run_contract_child_with_environment<const N: usize>(
    test: &str,
    root: &Path,
    key: OsString,
    value: OsString,
    completion: &str,
    extra: [(OsString, OsString); N],
) {
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture"])
        .env("PLOYZ_INSTALLER_CONTRACT_ROOT", root)
        .env("PATH", root.join("commands"))
        .env(key, value);
    for (key, value) in extra {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "contract child {test} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        fs::read_to_string(root.join("child-completed")).unwrap(),
        completion,
        "contract child {test} did not complete its fixture",
    );
}

pub(super) fn write_script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

pub(super) fn fixture(name: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(&format!("ployzd-installer-{name}-"))
        .tempdir()
        .unwrap()
}
