//! CLI setup adapters around the shared Machine installer.
//!
//! Bootstrap scenarios run in-process in `provisioning::setup_tests`.

use std::{env, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn ssh_setup_with_no_install_skips_the_bootstrap() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("ssh"),
        "#!/bin/sh\nprintf 'ssh %s\\n' \"$*\" >> \"$LOG\"\nexit 23\n",
    );
    write_executable(
        &bin.join("scp"),
        "#!/bin/sh\nprintf 'scp %s\\n' \"$*\" >> \"$LOG\"\n",
    );
    let inherited = env::var_os("PATH").unwrap_or_default();
    let path = env::join_paths(std::iter::once(bin).chain(env::split_paths(&inherited))).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "machine",
            "init",
            "deploy@2001:db8::1",
            "--no-install",
            "--yes",
            "--storage",
            "none",
            "--accepts-ingress=false",
            "--context",
            "setup-no-install",
        ])
        .env("PATH", path)
        .env("LOG", &log)
        .env("PLOYZ_CONFIG", fixture.path().join("config.yaml"))
        .output()
        .unwrap();

    assert!(!output.status.success(), "fake daemon must not accept RPC");
    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains("ssh "), "{log}");
    assert!(!log.contains("scp "), "{log}");
    assert!(!log.contains("'install'"), "{log}");
}
