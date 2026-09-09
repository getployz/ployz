//! CLI setup adapters invoking the shared Machine installer.

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use sha2::{Digest, Sha256};

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn daemon_archive() -> &'static str {
    match env::consts::ARCH {
        "x86_64" => "ployzd_linux_amd64.tar.gz",
        "aarch64" => "ployzd_linux_arm64.tar.gz",
        architecture => panic!("unsupported test architecture {architecture}"),
    }
}

fn release_fixture(root: &Path, log: &Path) -> PathBuf {
    let release = root.join("release");
    let payload = root.join("payload");
    fs::create_dir(&release).unwrap();
    fs::create_dir(&payload).unwrap();
    write_executable(
        &payload.join("ployzd"),
        &format!(
            "#!/bin/sh\nif [ \"${{1:-}}\" = version ]; then printf '%s\\n' '{}'; else printf '%s\\n' \"$*\" >> '{}'; exit \"${{PLOYZ_FAKE_INSTALL_STATUS:-0}}\"; fi\n",
            env!("CARGO_PKG_VERSION"),
            log.display()
        ),
    );
    let archive = release.join(daemon_archive());
    assert!(
        Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&payload)
            .arg("ployzd")
            .status()
            .unwrap()
            .success()
    );
    let hash = hex::encode(Sha256::digest(fs::read(&archive).unwrap()));
    fs::write(
        release.join("checksums.txt"),
        format!("{hash}  {}\n", daemon_archive()),
    )
    .unwrap();
    release
}

fn run(args: &[&str], root: &Path, release: &Path, bin: &Path) -> Output {
    let inherited = env::var_os("PATH").unwrap_or_default();
    let path =
        env::join_paths(std::iter::once(bin.to_path_buf()).chain(env::split_paths(&inherited)))
            .unwrap();
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .env("PATH", path)
        .env("PLOYZ_RELEASE_DIR", release)
        .env("PLOYZ_CONFIG", root.join("config.yaml"))
        .env("PLOYZ_FAKE_INSTALL_STATUS", "17")
        .env("LOG", root.join("commands.log"))
        .env("PLOYZ_TEST_PID", root.join("preflight.pid"))
        .env_remove("SUDO_USER")
        .output()
        .unwrap()
}

#[test]
fn local_setup_runs_the_verified_bootstrap_installer() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let release = release_fixture(fixture.path(), &log);
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("id"),
        "#!/bin/sh\nif [ \"$1\" = -u ]; then echo 0; else exec /usr/bin/id \"$@\"; fi\n",
    );

    let output = run(
        &[
            "machine",
            "init",
            "--yes",
            "--storage",
            "none",
            "--no-dns",
            "--accepts-ingress=false",
            "--context",
            "setup-local",
        ],
        fixture.path(),
        &release,
        &bin,
    );
    assert!(!output.status.success(), "fake daemon must not accept RPC");
    let log = fs::read_to_string(log).unwrap();
    assert!(
        log.contains(&format!(
            "install --version {} --storage none --release-dir",
            env!("CARGO_PKG_VERSION")
        )),
        "{log}"
    );
}

#[test]
fn corrupt_local_bootstrap_is_rejected_before_execution() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let release = release_fixture(fixture.path(), &log);
    fs::write(release.join(daemon_archive()), b"corrupt").unwrap();
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("id"),
        "#!/bin/sh\nif [ \"$1\" = -u ]; then echo 0; else exec /usr/bin/id \"$@\"; fi\n",
    );

    let output = run(
        &[
            "machine",
            "init",
            "--yes",
            "--storage",
            "none",
            "--no-dns",
            "--accepts-ingress=false",
            "--context",
            "setup-corrupt",
        ],
        fixture.path(),
        &release,
        &bin,
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("bootstrap verification"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!log.exists(), "corrupt bootstrap executed");
}

#[test]
fn ssh_setup_transfers_one_bootstrap_and_no_install_skips_it() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let release = release_fixture(fixture.path(), &log);
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("ssh"),
        &format!(
            r#"#!/bin/sh
printf 'ssh %s\n' "$*" >> "$LOG"
case "$*" in
  *' whoami') echo deploy ;;
  *' sudo true') ;;
  *'uname -s; uname -m') printf 'Linux\n{}\n' ;;
  *'mkdir -m 700 -- '*) ;;
  *'chmod 700 '*' version') printf '%s\n' '{}' ;;
  *" 'install' "*) ;;
  *'rm -rf -- '*) ;;
  *' -O exit '*) ;;
  *' true') exit 23 ;;
  *) echo "unexpected ssh invocation: $*" >&2; exit 24 ;;
esac
"#,
            env::consts::ARCH,
            env!("CARGO_PKG_VERSION")
        ),
    );
    write_executable(
        &bin.join("scp"),
        "#!/bin/sh\nprintf 'scp %s\\n' \"$*\" >> \"$LOG\"\n",
    );

    let common = [
        "machine",
        "init",
        "deploy@2001:db8::1",
        "--yes",
        "--storage",
        "none",
        "--no-dns",
        "--accepts-ingress=false",
        "--context",
        "setup-ssh",
    ];
    let inherited = env::var_os("PATH").unwrap_or_default();
    let path =
        env::join_paths(std::iter::once(bin.clone()).chain(env::split_paths(&inherited))).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(common)
        .env("PATH", &path)
        .env("LOG", &log)
        .env("PLOYZ_RELEASE_DIR", &release)
        .env("PLOYZ_CONFIG", fixture.path().join("ssh-config.yaml"))
        .output()
        .unwrap();
    assert!(!output.status.success(), "fake daemon must not accept RPC");
    let setup_log = fs::read_to_string(&log).unwrap();
    assert!(setup_log.contains("scp "), "{setup_log}");
    assert!(
        setup_log.contains("deploy@[2001:db8::1]:/tmp/ployz-bootstrap-"),
        "{setup_log}"
    );
    assert!(!setup_log.contains(" -P 1 "), "{setup_log}");
    assert!(setup_log.contains(daemon_archive()), "{setup_log}");
    assert!(setup_log.contains("checksums.txt"), "{setup_log}");
    assert!(
        setup_log.contains(&format!(
            "'install' '--version' '{}' '--storage' 'none' '--group-user' 'deploy'",
            env!("CARGO_PKG_VERSION")
        )),
        "{setup_log}"
    );
    assert!(setup_log.contains("'--release-dir' '/tmp/ployz-bootstrap-"));
    assert!(setup_log.contains("rm -rf --"), "{setup_log}");
    let close = setup_log.find(" -O exit deploy@2001:db8::1").unwrap();
    let reconnect = setup_log.rfind(" true").unwrap();
    assert!(close < reconnect, "{setup_log}");
    assert!(!setup_log.contains("base64"), "{setup_log}");

    fs::write(&log, "").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "machine",
            "init",
            "deploy@2001:db8::1",
            "--no-install",
            "--yes",
            "--storage",
            "none",
            "--no-dns",
            "--accepts-ingress=false",
            "--context",
            "setup-no-install",
        ])
        .env("PATH", path)
        .env("LOG", &log)
        .env("PLOYZ_RELEASE_DIR", release)
        .env(
            "PLOYZ_CONFIG",
            fixture.path().join("no-install-config.yaml"),
        )
        .output()
        .unwrap();
    assert!(!output.status.success(), "fake daemon must not accept RPC");
    let no_install_log = fs::read_to_string(log).unwrap();
    assert!(!no_install_log.contains("scp "), "{no_install_log}");
    assert!(!no_install_log.contains("'install'"), "{no_install_log}");
}

#[test]
fn remote_preflight_timeout_kills_the_child_and_cleans_the_stage() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let release = release_fixture(fixture.path(), &fixture.path().join("installer.log"));
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("ssh"),
        &format!(
            r#"#!/bin/sh
printf 'ssh %s\n' "$*" >> "$LOG"
case "$*" in
  *' whoami') echo root ;;
  *'uname -s; uname -m') printf 'Linux\n{}\n' ;;
  *'mkdir -m 700 -- '*) ;;
  *'chmod 700 '*' version') echo $$ > "$PLOYZ_TEST_PID"; exec sleep 30 ;;
  *'rm -rf -- '*) ;;
  *) exit 24 ;;
esac
"#,
            env::consts::ARCH
        ),
    );
    write_executable(&bin.join("scp"), "#!/bin/sh\nexit 0\n");

    let output = run(
        &[
            "machine",
            "init",
            "root@host",
            "--yes",
            "--storage",
            "none",
            "--no-dns",
            "--accepts-ingress=false",
            "--context",
            "setup-timeout",
        ],
        fixture.path(),
        &release,
        &bin,
    );

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("timed out after 10 seconds"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains("rm -rf --"), "{log}");
    let pid = fs::read_to_string(fixture.path().join("preflight.pid"))
        .unwrap()
        .trim()
        .to_owned();
    assert!(
        !Command::new("kill")
            .args(["-0", &pid])
            .status()
            .unwrap()
            .success(),
        "timed-out SSH child {pid} survived"
    );
}

#[test]
fn remote_cleanup_failure_is_returned_after_successful_installation() {
    let fixture = tempfile::tempdir().unwrap();
    let release = release_fixture(fixture.path(), &fixture.path().join("installer.log"));
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("ssh"),
        &format!(
            r#"#!/bin/sh
case "$*" in
  *' whoami') echo root ;;
  *'uname -s; uname -m') printf 'Linux\n{}\n' ;;
  *'mkdir -m 700 -- '*) ;;
  *'chmod 700 '*' version') printf '%s\n' '{}' ;;
  *" 'install' "*) ;;
  *'rm -rf -- '*) exit 29 ;;
  *) exit 24 ;;
esac
"#,
            env::consts::ARCH,
            env!("CARGO_PKG_VERSION")
        ),
    );
    write_executable(&bin.join("scp"), "#!/bin/sh\nexit 0\n");

    let output = run(
        &[
            "machine",
            "init",
            "root@host",
            "--yes",
            "--storage",
            "none",
            "--no-dns",
            "--accepts-ingress=false",
            "--context",
            "setup-cleanup",
        ],
        fixture.path(),
        &release,
        &bin,
    );

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("remote bootstrap cleanup exited with exit status: 29"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
