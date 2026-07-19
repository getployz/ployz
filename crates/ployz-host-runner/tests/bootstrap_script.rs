#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(unix)]
use std::thread;
use std::{env, fs};

#[test]
fn bootstrap_script_file_is_unified_release_delivery() {
    let script = fs::read_to_string(bootstrap_script_path()).expect("script is readable");

    assert!(script.contains("PLOYZ_URL"));
    assert!(script.contains("PLOYZ_SHA256"));
    assert!(script.contains("PLOYZ_VERSION"));
    assert!(script.contains("PLOYZ_CHANNEL"));
    assert!(script.contains("PLOYZ_RELEASE_MANIFEST_URL"));
    assert!(script.contains("PLOYZ_RELEASE_ENV_FILE"));
    assert!(script.contains("PLOYZ_RELEASE_PLATFORM"));
    assert!(script.contains("installed $ployz_bin"));
    assert!(script.contains("run: sudo ployz host bootstrap"));
    assert!(script.contains("sudo ployz host substrate-update --version $release_tag"));
    assert!(script.contains("--version <version>"));
    assert!(script.contains("--channel <channel>"));
    assert!(script.contains("shasum"));
    assert!(script.contains("sudo install"));
    assert!(script.contains("unknown ployz installer argument"));

    assert!(!script.contains("PLOYZ_JOIN_TOKEN"));
    assert!(!script.contains("PLOYZ_NATS_URL"));
    assert!(!script.contains("PLOYZD_URL"));
    assert!(!script.contains("PLOYZ_EBPF_TC_URL"));
    assert!(!script.contains("PLOYZ_MACHINE_JOIN_NATS_URL"));
    assert!(!script.contains("--join-token"));
    assert!(!script.contains("--first-machine"));
    assert!(!script.contains("--join-token-file"));
    assert!(!script.contains("expected_nats_server_archive_sha256"));
    assert!(!script.contains("umask 077"));
}

#[cfg(unix)]
#[test]
fn ployz_sh_site_staging_copies_installer_and_channels() {
    let root = temp_dir("ployz-sh-site-stage");
    let out_dir = root.join("site-out");

    let output = Command::new("bash")
        .arg(repo_path("scripts/stage-ployz-sh-site.sh"))
        .env("PLOYZ_SH_SITE_DIR", &out_dir)
        .output()
        .expect("site staging script can run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let source_installer =
        fs::read_to_string(repo_path("scripts/ployz.sh")).expect("installer is readable");
    assert_eq!(
        fs::read_to_string(out_dir.join("index.html")).expect("root installer is staged"),
        source_installer
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("install.sh")).expect("named installer is staged"),
        source_installer
    );
    assert!(out_dir.join(".nojekyll").exists());
    assert_eq!(
        fs::read_to_string(out_dir.join("_headers")).expect("headers are staged"),
        fs::read_to_string(repo_path("site/_headers")).expect("headers are readable")
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("channels/alpha.env")).expect("channel is staged"),
        fs::read_to_string(repo_path("site/channels/alpha.env")).expect("channel is readable")
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_installs_host_runner_by_default_from_alpha_channel() {
    let root = temp_dir("ployz-bootstrap-script-host-runner");
    let host_runner_source = write_fake_host_runner(&root);
    let channel = root.join("alpha.env");
    let manifest = root.join("ployz-release-linux-amd64.env");
    let install_dir = root.join("bin");
    let script_path = test_bootstrap_script_path(&root, &install_dir);
    let fake_bin = root.join("fake-bin");
    let curl_log = root.join("curl.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_channel(&channel, "alpha", "v0.0.2-alpha.1", "0.0.2-alpha.1");
    write_host_runner_manifest(
        &manifest,
        "linux-amd64",
        "v0.0.2-alpha.1",
        "0.0.2-alpha.1",
        &host_runner_source,
    );
    write_fake_tools(&fake_bin);

    let output = Command::new("sh")
        .arg(script_path)
        .env("PATH", test_path(&fake_bin))
        .env("PLOYZ_TEST_ALPHA_CHANNEL", &channel)
        .env("PLOYZ_TEST_RELEASE_MANIFEST", &manifest)
        .env("PLOYZ_TEST_CURL_LOG", &curl_log)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_VERSION")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run");

    assert_success(&output);
    assert!(install_dir.join("ployz").exists());
    assert_eq!(
        fs::read_to_string(root.join("release.env")).expect("release env is written"),
        "PLOYZ_RELEASE_MANIFEST_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1/ployz-release-linux-amd64.env\nPLOYZ_VERSION=0.0.2-alpha.1\nPLOYZ_RELEASE_TAG=v0.0.2-alpha.1\nPLOYZ_RELEASE_PLATFORM=linux-amd64\n"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("resolved ployz channel alpha -> v0.0.2-alpha.1"));
    assert!(stdout.contains(&format!(
        "installed {}",
        install_dir.join("ployz").display()
    )));
    assert!(stdout.contains("run: sudo ployz host bootstrap"));
    let curl_log = fs::read_to_string(&curl_log).expect("curl log is recorded");
    assert!(curl_log.contains("https://ployz.sh/channels/alpha.env"));
    assert!(curl_log.contains(
        "https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1/ployz-release-linux-amd64.env"
    ));
}

#[cfg(unix)]
#[test]
fn bootstrap_script_exact_version_bypasses_channel_lookup() {
    let root = temp_dir("ployz-bootstrap-script-exact-version");
    let host_runner_source = write_fake_host_runner(&root);
    let manifest = root.join("ployz-release-linux-amd64.env");
    let install_dir = root.join("bin");
    let script_path = test_bootstrap_script_path(&root, &install_dir);
    let fake_bin = root.join("fake-bin");
    let curl_log = root.join("curl.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_host_runner_manifest(
        &manifest,
        "linux-amd64",
        "v0.0.2-alpha.1",
        "0.0.2-alpha.1",
        &host_runner_source,
    );
    write_fake_tools(&fake_bin);

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--version", "v0.0.2-alpha.1"])
        .env("PATH", test_path(&fake_bin))
        .env("PLOYZ_TEST_RELEASE_MANIFEST", &manifest)
        .env("PLOYZ_TEST_CURL_LOG", &curl_log)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run");

    assert_success(&output);
    assert!(install_dir.join("ployz").exists());
    let curl_log = fs::read_to_string(&curl_log).expect("curl log is recorded");
    assert!(!curl_log.contains("/channels/"));
    assert!(curl_log.contains(
        "https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1/ployz-release-linux-amd64.env"
    ));
}

#[cfg(unix)]
#[test]
fn bootstrap_script_named_channel_selects_channel_file() {
    let root = temp_dir("ployz-bootstrap-script-beta-channel");
    let host_runner_source = write_fake_host_runner(&root);
    let channel = root.join("beta.env");
    let manifest = root.join("ployz-release-linux-amd64.env");
    let install_dir = root.join("bin");
    let script_path = test_bootstrap_script_path(&root, &install_dir);
    let fake_bin = root.join("fake-bin");
    let curl_log = root.join("curl.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_channel(&channel, "beta", "v0.0.3-beta.1", "0.0.3-beta.1");
    write_host_runner_manifest(
        &manifest,
        "linux-amd64",
        "v0.0.3-beta.1",
        "0.0.3-beta.1",
        &host_runner_source,
    );
    write_fake_tools(&fake_bin);

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--channel", "beta"])
        .env("PATH", test_path(&fake_bin))
        .env("PLOYZ_TEST_BETA_CHANNEL", &channel)
        .env("PLOYZ_TEST_RELEASE_MANIFEST", &manifest)
        .env("PLOYZ_TEST_CURL_LOG", &curl_log)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_VERSION")
        .output()
        .expect("bootstrap script can run");

    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("resolved ployz channel beta -> v0.0.3-beta.1"));
    let curl_log = fs::read_to_string(&curl_log).expect("curl log is recorded");
    assert!(curl_log.contains("https://ployz.sh/channels/beta.env"));
    assert!(curl_log.contains(
        "https://github.com/getployz/ployz/releases/download/v0.0.3-beta.1/ployz-release-linux-amd64.env"
    ));
}

#[cfg(unix)]
#[test]
fn bootstrap_script_manifest_override_bypasses_channel_resolution() {
    let root = temp_dir("ployz-bootstrap-script-manifest-override");
    let host_runner_source = write_fake_host_runner(&root);
    let manifest = root.join("manifest.env");
    let install_dir = root.join("bin");
    let script_path = test_bootstrap_script_path(&root, &install_dir);
    let fake_bin = root.join("fake-bin");
    let curl_log = root.join("curl.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_host_runner_manifest(
        &manifest,
        "linux-amd64",
        "v0.0.2-alpha.1",
        "0.0.2-alpha.1",
        &host_runner_source,
    );
    write_fake_tools(&fake_bin);

    let output = Command::new("sh")
        .arg(script_path)
        .env("PATH", test_path(&fake_bin))
        .env(
            "PLOYZ_RELEASE_MANIFEST_URL",
            format!("file://{}", manifest.display()),
        )
        .env("PLOYZ_TEST_CURL_LOG", &curl_log)
        .env_remove("PLOYZ_VERSION")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run");

    assert_success(&output);
    assert!(install_dir.join("ployz").exists());
    assert!(
        !curl_log.exists(),
        "a local manifest and local artifact should not invoke curl"
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_retries_a_transient_manifest_failure() {
    let (output, requests) =
        run_bootstrap_with_http_manifest("ployz-bootstrap-script-transient-manifest", &[503, 200]);

    assert_success(&output);
    assert_eq!(
        requests, 3,
        "one retry plus the installer's existing second manifest read"
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_stops_after_manifest_retries_are_exhausted() {
    let (output, requests) = run_bootstrap_with_http_manifest(
        "ployz-bootstrap-script-exhausted-manifest",
        &[503, 503, 503, 503],
    );

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "failed to download release manifest http://127.0.0.1:",
    );
    assert_eq!(requests, 4);
}

#[cfg(unix)]
#[test]
fn bootstrap_script_does_not_retry_a_missing_manifest() {
    let (output, requests) =
        run_bootstrap_with_http_manifest("ployz-bootstrap-script-missing-manifest", &[404]);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "failed to download release manifest http://127.0.0.1:",
    );
    assert_eq!(requests, 1);
}

#[cfg(unix)]
#[test]
fn bootstrap_script_uses_sudo_install_when_not_root() {
    let root = temp_dir("ployz-bootstrap-script-sudo");
    let host_runner_source = write_fake_host_runner(&root);
    let manifest = root.join("manifest.env");
    let install_dir = root.join("bin");
    let script_path = test_bootstrap_script_path(&root, &install_dir);
    let fake_bin = root.join("fake-bin");
    let sudo_log = root.join("sudo.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_host_runner_manifest(
        &manifest,
        "linux-amd64",
        "v0.0.2-alpha.1",
        "0.0.2-alpha.1",
        &host_runner_source,
    );
    write_fake_tools(&fake_bin);
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '501\\n'\n");
    write_executable(
        &fake_bin.join("sudo"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PLOYZ_TEST_SUDO_LOG\"\nexec \"$@\"\n",
    );

    let output = Command::new("sh")
        .arg(script_path)
        .env("PATH", test_path(&fake_bin))
        .env(
            "PLOYZ_RELEASE_MANIFEST_URL",
            format!("file://{}", manifest.display()),
        )
        .env("PLOYZ_TEST_SUDO_LOG", &sudo_log)
        .env_remove("PLOYZ_VERSION")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run");

    assert_success(&output);
    assert!(install_dir.join("ployz").exists());
    let sudo_log = fs::read_to_string(&sudo_log).expect("sudo log is recorded");
    assert!(sudo_log.contains("install -d -m 0755"));
    assert!(sudo_log.contains("install -m 0755"));
}

#[cfg(unix)]
#[test]
fn bootstrap_script_rejects_old_public_machine_modes() {
    for flag in ["--join-token", "--first-machine", "--cloud-token"] {
        let output = run_bootstrap_script(&[flag]);

        assert!(!output.status.success());
        assert_stderr_contains(
            &output,
            &format!("unknown ployz installer argument: {flag}"),
        );
    }
}

#[cfg(unix)]
#[test]
fn bootstrap_script_rejects_channel_and_version_together() {
    let output = run_bootstrap_script(&["--channel", "alpha", "--version", "v0.0.2-alpha.1"]);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "pass either --version/PLOYZ_VERSION or --channel/PLOYZ_CHANNEL, not both",
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_reports_missing_manifest_key_and_url() {
    let root = temp_dir("ployz-bootstrap-script-missing-manifest-key");
    let manifest = root.join("manifest.env");
    let install_dir = root.join("bin");
    let script_path = test_bootstrap_script_path(&root, &install_dir);
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(
        &manifest,
        "PLOYZ_VERSION=0.0.2-alpha.1\nPLOYZ_RELEASE_TAG=v0.0.2-alpha.1\nPLOYZ_RELEASE_PLATFORM=linux-amd64\n",
    )
    .expect("manifest can be written");
    write_fake_tools(&fake_bin);

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--version", "v0.0.2-alpha.1"])
        .env("PATH", test_path(&fake_bin))
        .env("PLOYZ_TEST_RELEASE_MANIFEST", &manifest)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "release manifest https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1/ployz-release-linux-amd64.env is missing PLOYZ_URL",
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_rejects_unsupported_operating_system() {
    let root = temp_dir("ployz-bootstrap-script-unsupported-os");
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_fake_tools(&fake_bin);
    write_executable(
        &fake_bin.join("uname"),
        &fake_uname_script_for("FreeBSD", "x86_64"),
    );

    let output = Command::new("sh")
        .arg(bootstrap_script_path())
        .env("PATH", test_path(&fake_bin))
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_VERSION")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "unsupported operating system: FreeBSD (ployz bootstrap delivery requires Linux)",
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_rejects_macos() {
    let root = temp_dir("ployz-bootstrap-script-macos");
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_fake_tools(&fake_bin);
    write_executable(
        &fake_bin.join("uname"),
        &fake_uname_script_for("Darwin", "arm64"),
    );

    let output = Command::new("sh")
        .arg(bootstrap_script_path())
        .env("PATH", test_path(&fake_bin))
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_VERSION")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "unsupported operating system: Darwin (ployz bootstrap delivery requires Linux)",
    );
}

fn write_fake_host_runner(root: &std::path::Path) -> PathBuf {
    let host_runner = root.join("ployz-source");
    fs::write(&host_runner, "#!/bin/sh\nexit 0\n").expect("fake Host Runner source can be written");
    host_runner
}

fn write_channel(path: &std::path::Path, channel: &str, tag: &str, version: &str) {
    fs::write(
        path,
        format!(
            "PLOYZ_CHANNEL={channel}\nPLOYZ_RELEASE_TAG={tag}\nPLOYZ_VERSION={version}\nPLOYZ_RELEASE_BASE_URL=https://github.com/getployz/ployz/releases/download/{tag}\n"
        ),
    )
    .expect("channel can be written");
}

fn write_host_runner_manifest(
    path: &std::path::Path,
    platform: &str,
    tag: &str,
    version: &str,
    host_runner_source: &std::path::Path,
) {
    fs::write(
        path,
        format!(
            "PLOYZ_VERSION={version}\nPLOYZ_RELEASE_TAG={tag}\nPLOYZ_RELEASE_PLATFORM={platform}\nPLOYZ_URL=file://{}\nPLOYZ_SHA256={HOST_RUNNER_DIGEST}\n",
            host_runner_source.display()
        ),
    )
    .expect("manifest can be written");
}

#[cfg(unix)]
fn write_fake_tools(fake_bin: &std::path::Path) {
    write_executable(&fake_bin.join("curl"), fake_release_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");
}

#[cfg(unix)]
fn run_bootstrap_with_http_manifest(prefix: &str, statuses: &[u16]) -> (Output, usize) {
    let root = temp_dir(prefix);
    let host_runner_source = write_fake_host_runner(&root);
    let install_dir = root.join("bin");
    let script_path = test_bootstrap_script_path(&root, &install_dir);
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let manifest = format!(
        "PLOYZ_VERSION=0.0.2-alpha.1\nPLOYZ_RELEASE_TAG=v0.0.2-alpha.1\nPLOYZ_RELEASE_PLATFORM=linux-amd64\nPLOYZ_URL=file://{}\nPLOYZ_SHA256={HOST_RUNNER_DIGEST}\n",
        host_runner_source.display()
    );
    let server = TestHttpServer::start(statuses, &manifest);
    let output = Command::new("sh")
        .arg(script_path)
        .env("PATH", test_path(&fake_bin))
        .env("PLOYZ_RELEASE_MANIFEST_URL", server.url())
        .env_remove("PLOYZ_VERSION")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run");
    let requests = server.request_count();
    (output, requests)
}

#[cfg(unix)]
struct TestHttpServer {
    url: String,
    request_count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl TestHttpServer {
    fn start(statuses: &[u16], manifest: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test HTTP server can bind");
        listener
            .set_nonblocking(true)
            .expect("test HTTP server can be nonblocking");
        let address = listener
            .local_addr()
            .expect("test HTTP address is available");
        let responses = statuses
            .iter()
            .map(|status| (*status, manifest.to_owned()))
            .collect::<Vec<_>>();
        let request_count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_request_count = Arc::clone(&request_count);
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let index = thread_request_count.fetch_add(1, Ordering::SeqCst);
                        let mut request = [0_u8; 1024];
                        let _ = stream.read(&mut request);
                        let (status, body) = responses
                            .get(index)
                            .or_else(|| responses.last())
                            .expect("test HTTP server has a response");
                        let reason = if *status == 200 {
                            "OK"
                        } else if *status == 404 {
                            "Not Found"
                        } else {
                            "Service Unavailable"
                        };
                        let response = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        stream
                            .write_all(response.as_bytes())
                            .expect("test HTTP response can be written");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("test HTTP server failed: {error}"),
                }
            }
        });
        Self {
            url: format!("http://{address}/manifest.env"),
            request_count,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }
}

#[cfg(unix)]
impl Drop for TestHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("test HTTP server can stop");
        }
    }
}

fn unique_temp_path(prefix: &str) -> PathBuf {
    let unique = format!(
        "{}-{}-{}",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

fn temp_dir(prefix: &str) -> PathBuf {
    let path = unique_temp_path(prefix);
    fs::create_dir_all(&path).expect("temp dir can be created");
    path
}

#[cfg(unix)]
fn write_executable(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).expect("executable can be written");
    let mut permissions = fs::metadata(path)
        .expect("executable metadata is readable")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("executable permissions can be set");
}

fn fake_release_curl_script() -> &'static str {
    "#!/bin/sh\nurl=\ndest=\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o)\n      dest=\"$2\"\n      shift 2\n      ;;\n    --retry|--retry-delay|--retry-max-time|--connect-timeout|--max-time)\n      shift 2\n      ;;\n    --retry-connrefused)\n      shift\n      ;;\n    -*)\n      shift\n      ;;\n    *)\n      url=\"$1\"\n      shift\n      ;;\n  esac\ndone\nif [ -n \"${PLOYZ_TEST_CURL_LOG:-}\" ]; then\n  printf '%s\\n' \"$url\" >> \"$PLOYZ_TEST_CURL_LOG\"\nfi\ncase \"$url\" in\n  file://*) cp \"${url#file://}\" \"$dest\" ;;\n  https://ployz.sh/channels/alpha.env) cp \"$PLOYZ_TEST_ALPHA_CHANNEL\" \"$dest\" ;;\n  https://ployz.sh/channels/beta.env) cp \"$PLOYZ_TEST_BETA_CHANNEL\" \"$dest\" ;;\n  https://github.com/getployz/ployz/releases/download/*/ployz-release-*.env) cp \"$PLOYZ_TEST_RELEASE_MANIFEST\" \"$dest\" ;;\n  *) exit 2 ;;\nesac\n"
}

fn fake_uname_script() -> &'static str {
    "#!/bin/sh\ncase \"${1:-}\" in\n  -m) printf 'x86_64\\n' ;;\n  *) printf 'Linux\\n' ;;\nesac\n"
}

fn fake_uname_script_for(os: &str, arch: &str) -> String {
    format!(
        "#!/bin/sh\ncase \"${{1:-}}\" in\n  -m) printf '{arch}\\n' ;;\n  *) printf '{os}\\n' ;;\nesac\n"
    )
}

fn bootstrap_script_path() -> PathBuf {
    repo_path("scripts/ployz.sh")
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn test_bootstrap_script_path(root: &std::path::Path, install_dir: &std::path::Path) -> PathBuf {
    let source = fs::read_to_string(bootstrap_script_path()).expect("bootstrap script is readable");
    let rewritten = replace_required(
        source,
        "install_dir=\"/usr/local/bin\"",
        &format!("install_dir=\"{}\"", install_dir.display()),
    );
    let rewritten = replace_required(
        rewritten,
        "release_env_file=\"${PLOYZ_RELEASE_ENV_FILE:-/etc/ployz/release.env}\"",
        &format!(
            "release_env_file=\"${{PLOYZ_RELEASE_ENV_FILE:-{}}}\"",
            root.join("release.env").display()
        ),
    );
    assert!(!rewritten.contains("install_dir=\"/usr/local/bin\""));
    assert!(!rewritten.contains("/etc/ployz/release.env"));
    let path = root.join("ployz.sh");
    fs::write(&path, rewritten).expect("test bootstrap script can be written");
    path
}

fn replace_required(source: String, needle: &str, replacement: &str) -> String {
    assert!(
        source.contains(needle),
        "bootstrap script missing replacement needle {needle:?}"
    );
    source.replace(needle, replacement)
}

fn run_bootstrap_script(args: &[&str]) -> Output {
    Command::new("sh")
        .arg(bootstrap_script_path())
        .args(args)
        .env("PLOYZ_URL", "https://example.invalid/ployz")
        .env("PLOYZ_SHA256", HOST_RUNNER_DIGEST)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_VERSION")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run")
}

fn test_path(fake_bin: &std::path::Path) -> String {
    format!(
        "{}:{}",
        fake_bin.display(),
        env::var("PATH").unwrap_or_default()
    )
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_stderr_contains(output: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "stderr should contain {expected:?}, got {stderr:?}"
    );
}

const HOST_RUNNER_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
