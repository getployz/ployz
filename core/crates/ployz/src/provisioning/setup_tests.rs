//! Setup scenarios that run the shared installer path with a locally built bootstrap.

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

use super::*;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn daemon_archive() -> &'static str {
    bootstrap::daemon_archive(env::consts::ARCH).unwrap()
}

/// A release directory whose fake daemon logs installer arguments and exits with `status`.
fn release_fixture(root: &Path, log: &Path, status: u8) -> PathBuf {
    let release = root.join("release");
    let payload = root.join("payload");
    fs::create_dir(&release).unwrap();
    fs::create_dir(&payload).unwrap();
    write_executable(
        &payload.join("ployzd"),
        &format!(
            "#!/bin/sh\nif [ \"${{1:-}}\" = version ]; then printf '%s\\n' '{VERSION}'; else printf '%s\\n' \"$*\" >> '{}'; exit {status}; fi\n",
            log.display()
        ),
    );
    let archive = release.join(daemon_archive());
    assert!(
        Command::new("tar")
            .arg("-czf")
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

async fn bootstrap(release: &Path) -> Result<Bootstrap, ProvisionError> {
    Bootstrap::from_release_dir(release, env::consts::ARCH).await
}

/// A remote whose `ssh` runs `ssh_script` and whose `scp` logs and succeeds.
fn remote(root: &Path, destination: &str, log: &Path, ssh_script: &str) -> Remote {
    let matches = crate::cli::command()
        .try_get_matches_from(["ployz", "machine", "add", destination])
        .unwrap();
    let matches = matches
        .subcommand_matches("machine")
        .unwrap()
        .subcommand_matches("add")
        .unwrap();
    let mut remote = Remote::from_matches(matches).unwrap();
    let ssh = root.join("ssh");
    let scp = root.join("scp");
    write_executable(&ssh, ssh_script);
    write_executable(
        &scp,
        &format!(
            "#!/bin/sh\nprintf 'scp %s\\n' \"$*\" >> '{}'\n",
            log.display()
        ),
    );
    remote.ssh = ssh;
    remote.scp = scp;
    remote.control_path = Some(root.join("control-%C"));
    remote
}

async fn run_remote(remote: &Remote, release: &Path) -> Result<(), ProvisionError> {
    let host = remote.preflight().await?;
    let bootstrap = Bootstrap::from_release_dir(release, &host.architecture).await?;
    remote
        .install_bootstrap(&host, &bootstrap, VERSION, StorageChoice::None)
        .await
}

#[tokio::test]
async fn local_setup_runs_the_verified_bootstrap_installer() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let release = release_fixture(fixture.path(), &log, 17);

    let error = install_local(
        &bootstrap(&release).await.unwrap(),
        VERSION,
        Preparation::Host {
            storage: StorageChoice::None,
            group_user: None,
        },
    )
    .await
    .unwrap_err();

    assert!(
        matches!(error, ProvisionError::InstallFailed { .. }),
        "{error}"
    );
    let log = fs::read_to_string(log).unwrap();
    assert_eq!(
        log.trim(),
        format!("install --version {VERSION} --storage none")
    );
}

#[tokio::test]
async fn corrupt_local_bootstrap_is_rejected_before_execution() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let release = release_fixture(fixture.path(), &log, 0);
    fs::write(release.join(daemon_archive()), b"corrupt").unwrap();

    let Err(error) = bootstrap(&release).await else {
        panic!("corrupt bootstrap was accepted");
    };

    assert!(
        error.to_string().contains("bootstrap verification"),
        "{error}"
    );
    assert!(!log.exists(), "corrupt bootstrap executed");
}

#[tokio::test]
async fn ssh_setup_transfers_one_bootstrap() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let release = release_fixture(fixture.path(), &fixture.path().join("installer.log"), 0);
    let remote = remote(
        fixture.path(),
        "deploy@2001:db8::1",
        &log,
        &format!(
            r#"#!/bin/sh
printf 'ssh %s\n' "$*" >> '{log}'
case "$*" in
  *' whoami') echo deploy ;;
  *' sudo true') ;;
  *'uname -s; uname -m') printf 'Linux\n{arch}\n' ;;
  *'mkdir -m 700 -- '*) ;;
  *'chmod 700 '*' version') printf '%s\n' '{VERSION}' ;;
  *" 'install' "*) ;;
  *'rm -rf -- '*) ;;
  *' -O exit '*) ;;
  *) echo "unexpected ssh invocation: $*" >&2; exit 24 ;;
esac
"#,
            log = log.display(),
            arch = env::consts::ARCH,
        ),
    );

    run_remote(&remote, &release).await.unwrap();

    let setup_log = fs::read_to_string(&log).unwrap();
    let transfers: Vec<_> = setup_log
        .lines()
        .filter(|line| line.starts_with("scp "))
        .collect();
    let [transfer] = transfers.as_slice() else {
        panic!("expected one transfer: {setup_log}");
    };
    assert!(
        transfer.contains("/ployzd deploy@[2001:db8::1]:/tmp/ployz-bootstrap-"),
        "{setup_log}"
    );
    assert!(!setup_log.contains(" -P 1 "), "{setup_log}");
    let install = setup_log
        .find(&format!(
            "'install' '--version' '{VERSION}' '--storage' 'none' '--group-user' 'deploy'"
        ))
        .expect(&setup_log);
    let cleanup = setup_log.find("rm -rf --").expect(&setup_log);
    let close = setup_log
        .find(" -O exit deploy@2001:db8::1")
        .expect(&setup_log);
    assert!(install < cleanup && cleanup < close, "{setup_log}");
}

#[tokio::test]
async fn remote_verify_timeout_kills_the_child_and_cleans_the_stage() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let pid = fixture.path().join("preflight.pid");
    let release = release_fixture(fixture.path(), &fixture.path().join("installer.log"), 0);
    let remote = remote(
        fixture.path(),
        "root@host",
        &log,
        &format!(
            r#"#!/bin/sh
printf 'ssh %s\n' "$*" >> '{log}'
case "$*" in
  *' whoami') echo root ;;
  *'uname -s; uname -m') printf 'Linux\n{arch}\n' ;;
  *'mkdir -m 700 -- '*) ;;
  *'chmod 700 '*' version') echo $$ > '{pid}'; exec sleep 30 ;;
  *'rm -rf -- '*) ;;
  *' -O exit '*) ;;
  *) exit 24 ;;
esac
"#,
            log = log.display(),
            pid = pid.display(),
            arch = env::consts::ARCH,
        ),
    );

    let error = run_remote(&remote, &release).await.unwrap_err();

    assert!(
        error.to_string().contains("timed out after 10 seconds"),
        "{error}"
    );
    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains("rm -rf --"), "{log}");
    let pid = fs::read_to_string(pid).unwrap().trim().to_owned();
    assert!(
        !Command::new("kill")
            .args(["-0", &pid])
            .status()
            .unwrap()
            .success(),
        "timed-out SSH child {pid} survived"
    );
}

#[tokio::test]
async fn remote_cleanup_failure_is_returned_after_successful_installation() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let release = release_fixture(fixture.path(), &fixture.path().join("installer.log"), 0);
    let remote = remote(
        fixture.path(),
        "root@host",
        &log,
        &format!(
            r#"#!/bin/sh
case "$*" in
  *' whoami') echo root ;;
  *'uname -s; uname -m') printf 'Linux\n{arch}\n' ;;
  *'mkdir -m 700 -- '*) ;;
  *'chmod 700 '*' version') printf '%s\n' '{VERSION}' ;;
  *" 'install' "*) ;;
  *'rm -rf -- '*) exit 29 ;;
  *' -O exit '*) ;;
  *) exit 24 ;;
esac
"#,
            arch = env::consts::ARCH,
        ),
    );

    let error = run_remote(&remote, &release).await.unwrap_err();

    assert_eq!(
        error.to_string(),
        "remote bootstrap cleanup exited with exit status: 29"
    );
}
