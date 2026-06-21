#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::{env, fs};

#[test]
fn bootstrap_script_file_installs_ployzctl_or_keeper_from_release_manifest() {
    let script = fs::read_to_string(bootstrap_script_path()).expect("script is readable");

    assert!(script.contains("PLOYZ_KEEPER_URL"));
    assert!(script.contains("PLOYZCTL_URL"));
    assert!(script.contains("PLOYZ_VERSION"));
    assert!(script.contains("PLOYZ_CHANNEL"));
    assert!(script.contains("PLOYZ_RELEASE_MANIFEST_URL"));
    assert!(script.contains("PLOYZ_RELEASE_PLATFORM"));
    assert!(script.contains("PLOYZ_NATS_URL"));
    assert!(script.contains("PLOYZD_URL"));
    assert!(script.contains("PLOYZ_EBPF_TC_URL"));
    assert!(script.contains("PLOYZ_MACHINE_JOIN_NATS_URL"));
    assert!(script.contains("PLOYZ_JOIN_TOKEN"));
    assert!(script.contains("join-token"));
    assert!(script.contains("installed $ployzctl_bin"));
    assert!(script.contains("\"$keeper_bin\" --join-token-file \"$join_token_file\""));
    assert!(script.contains("not both"));
    assert!(script.contains("--join-token <token>"));
    assert!(script.contains("--first-node"));
    assert!(script.contains("--version <version>"));
    assert!(script.contains("--channel <channel>"));
    assert!(script.contains("Darwin"));
    assert!(script.contains("shasum"));
    assert!(script.contains("ployz machine bootstrap requires Linux"));
    assert!(script.contains("ployz machine bootstrap must run as root"));
    assert!(script.contains("unknown ployz installer argument"));
    assert!(script.contains("install -d -m 0700"));
    assert!(script.contains("umask 077"));
    assert!(script.contains("uname -s"));
    assert!(script.contains("id -u"));
    assert!(script.contains("expected_nats_server_archive_sha256"));
    assert!(script.contains("b3e7b14eb10c895fd90c2dacdb6b65bd3208adcc9524dd7689ba2c1024e6b97a"));
    assert!(script.contains("15fd0c3438e7178e5316e63be68373ad581c8d78db26e649113aa303b74e5e58"));
    assert!(!script.contains("PLOYZ_INSTALL_DIR"));
    assert!(!script.contains("PLOYZ_SYSTEMD_DIR"));
    assert!(!script.contains("PLOYZ_KEEPER_STATE_DIR"));
    assert!(!script.contains("cat > \"$keeper_unit\""));
    assert!(!script.contains("systemctl enable ployz-keeper.service"));
    assert!(!script.contains("systemctl restart ployz-keeper.service"));
    assert!(!script.contains("ExecStart=${keeper_bin}${keeper_args}"));
    assert!(!script.contains("NATS_CREDS"));
    assert!(!script.contains("default_version="));
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

    let workflow = fs::read_to_string(repo_path(".github/workflows/ployz-sh.yml"))
        .expect("ployz.sh workflow is readable");
    assert!(workflow.contains("cloudflare/wrangler-action"));
    assert!(workflow.contains("pages deploy dist/ployz-sh-site --project-name=ployz-sh"));
    assert!(workflow.contains("CLOUDFLARE_API_TOKEN"));
    assert!(workflow.contains("CLOUDFLARE_ACCOUNT_ID"));
    assert!(workflow.contains("scripts/stage-ployz-sh-site.sh"));
    assert!(!workflow.contains("actions/deploy-pages"));
    assert!(!workflow.contains("actions/configure-pages"));
    assert!(!workflow.contains("upload-pages-artifact"));
    assert!(!workflow.contains("tags:"));
}

#[cfg(unix)]
#[test]
fn bootstrap_script_installs_ployzctl_by_default_from_alpha_channel() {
    let root = temp_dir("ployz-bootstrap-script-ployzctl");
    let ployzctl_source = root.join("ployzctl-source");
    let channel = root.join("alpha.env");
    let manifest = root.join("ployz-release-linux-amd64.env");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    let curl_log = root.join("curl.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(&ployzctl_source, "#!/bin/sh\nexit 0\n")
        .expect("fake ployzctl source can be written");
    fs::write(
        &channel,
        "PLOYZ_CHANNEL=alpha\nPLOYZ_RELEASE_TAG=v0.0.2-alpha.1\nPLOYZ_VERSION=0.0.2-alpha.1\nPLOYZ_RELEASE_BASE_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1\n",
    )
    .expect("channel can be written");
    fs::write(
        &manifest,
        format!(
            "PLOYZ_VERSION=0.0.2-alpha.1\nPLOYZ_RELEASE_TAG=v0.0.2-alpha.1\nPLOYZ_RELEASE_PLATFORM=linux-amd64\nPLOYZCTL_URL=file://{}\nPLOYZCTL_SHA256={}\n",
            ployzctl_source.display(),
            KEEPER_DIGEST
        ),
    )
    .expect("manifest can be written");
    write_executable(&fake_bin.join("curl"), fake_release_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("PLOYZ_TEST_ALPHA_CHANNEL", &channel)
        .env("PLOYZ_TEST_RELEASE_MANIFEST", &manifest)
        .env("PLOYZ_TEST_CURL_LOG", &curl_log)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_VERSION")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(install_dir.join("ployzctl").exists());
    assert!(!install_dir.join("ployz-keeper").exists());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("resolved ployz channel alpha -> v0.0.2-alpha.1"));
    assert!(stdout.contains(&format!(
        "installed {}",
        install_dir.join("ployzctl").display()
    )));
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
    let ployzctl_source = root.join("ployzctl-source");
    let manifest = root.join("ployz-release-linux-amd64.env");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    let curl_log = root.join("curl.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(&ployzctl_source, "#!/bin/sh\nexit 0\n")
        .expect("fake ployzctl source can be written");
    fs::write(
        &manifest,
        format!(
            "PLOYZ_VERSION=0.0.2-alpha.1\nPLOYZ_RELEASE_TAG=v0.0.2-alpha.1\nPLOYZ_RELEASE_PLATFORM=linux-amd64\nPLOYZCTL_URL=file://{}\nPLOYZCTL_SHA256={}\n",
            ployzctl_source.display(),
            KEEPER_DIGEST
        ),
    )
    .expect("manifest can be written");
    write_executable(&fake_bin.join("curl"), fake_release_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--version", "v0.0.2-alpha.1"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("PLOYZ_TEST_RELEASE_MANIFEST", &manifest)
        .env("PLOYZ_TEST_CURL_LOG", &curl_log)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_CHANNEL")
        .output()
        .expect("bootstrap script can run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(install_dir.join("ployzctl").exists());
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
    let ployzctl_source = root.join("ployzctl-source");
    let channel = root.join("beta.env");
    let manifest = root.join("ployz-release-linux-amd64.env");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    let curl_log = root.join("curl.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(&ployzctl_source, "#!/bin/sh\nexit 0\n")
        .expect("fake ployzctl source can be written");
    fs::write(
        &channel,
        "PLOYZ_CHANNEL=beta\nPLOYZ_RELEASE_TAG=v0.0.3-beta.1\nPLOYZ_VERSION=0.0.3-beta.1\nPLOYZ_RELEASE_BASE_URL=https://github.com/getployz/ployz/releases/download/v0.0.3-beta.1\n",
    )
    .expect("channel can be written");
    fs::write(
        &manifest,
        format!(
            "PLOYZ_VERSION=0.0.3-beta.1\nPLOYZ_RELEASE_TAG=v0.0.3-beta.1\nPLOYZ_RELEASE_PLATFORM=linux-amd64\nPLOYZCTL_URL=file://{}\nPLOYZCTL_SHA256={}\n",
            ployzctl_source.display(),
            KEEPER_DIGEST
        ),
    )
    .expect("manifest can be written");
    write_executable(&fake_bin.join("curl"), fake_release_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--channel", "beta"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("PLOYZ_TEST_BETA_CHANNEL", &channel)
        .env("PLOYZ_TEST_RELEASE_MANIFEST", &manifest)
        .env("PLOYZ_TEST_CURL_LOG", &curl_log)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_VERSION")
        .output()
        .expect("bootstrap script can run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
fn bootstrap_script_rejects_channel_release_base_url_mismatch() {
    let root = temp_dir("ployz-bootstrap-script-channel-base-mismatch");
    let channel = root.join("alpha.env");
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(
        &channel,
        "PLOYZ_CHANNEL=alpha\nPLOYZ_RELEASE_TAG=v0.0.2-alpha.1\nPLOYZ_VERSION=0.0.2-alpha.1\nPLOYZ_RELEASE_BASE_URL=https://example.invalid/releases/v0.0.2-alpha.1\n",
    )
    .expect("channel can be written");
    write_executable(&fake_bin.join("curl"), fake_release_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(bootstrap_script_path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("PLOYZ_TEST_ALPHA_CHANNEL", &channel)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_VERSION")
        .env_remove("PLOYZ_CHANNEL")
        .env_remove("PLOYZ_JOIN_TOKEN")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "PLOYZ_RELEASE_BASE_URL=https://example.invalid/releases/v0.0.2-alpha.1, expected https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1",
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_rejects_release_manifest_identity_mismatch() {
    let root = temp_dir("ployz-bootstrap-script-manifest-identity-mismatch");
    let manifest = root.join("ployz-release-linux-amd64.env");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(
        &manifest,
        "PLOYZ_VERSION=0.0.2-alpha.1\nPLOYZ_RELEASE_TAG=v0.0.3-alpha.1\nPLOYZ_RELEASE_PLATFORM=linux-amd64\n",
    )
    .expect("manifest can be written");
    write_executable(&fake_bin.join("curl"), fake_release_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--version", "v0.0.2-alpha.1"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("PLOYZ_TEST_RELEASE_MANIFEST", &manifest)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_CHANNEL")
        .env_remove("PLOYZ_JOIN_TOKEN")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "has PLOYZ_RELEASE_TAG=v0.0.3-alpha.1, expected v0.0.2-alpha.1",
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_manifest_override_bypasses_channel_resolution() {
    let root = temp_dir("ployz-bootstrap-script-manifest-override");
    let ployzctl_source = root.join("ployzctl-source");
    let manifest = root.join("ployz-release-linux-amd64.env");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    let curl_log = root.join("curl.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(&ployzctl_source, "#!/bin/sh\nexit 0\n")
        .expect("fake ployzctl source can be written");
    fs::write(
        &manifest,
        format!(
            "PLOYZCTL_URL=file://{}\nPLOYZCTL_SHA256={}\n",
            ployzctl_source.display(),
            KEEPER_DIGEST
        ),
    )
    .expect("manifest can be written");
    write_executable(&fake_bin.join("curl"), fake_release_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("PLOYZ_CHANNEL", "beta")
        .env(
            "PLOYZ_RELEASE_MANIFEST_URL",
            format!("file://{}", manifest.display()),
        )
        .env("PLOYZ_TEST_CURL_LOG", &curl_log)
        .env_remove("PLOYZ_VERSION")
        .output()
        .expect("bootstrap script can run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let curl_log = fs::read_to_string(&curl_log).expect("curl log is recorded");
    assert!(!curl_log.contains("/channels/"));
    assert!(install_dir.join("ployzctl").exists());
}

#[cfg(unix)]
#[test]
fn bootstrap_script_runs_join_directly_and_does_not_start_noop_service() {
    let root = temp_dir("ployz-bootstrap-script-join");
    let keeper_source = root.join("ployz-keeper-source");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    let keeper_args_log = root.join("keeper-args.log");
    let keeper_token_log = root.join("keeper-token.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(
        &keeper_source,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$PLOYZ_TEST_KEEPER_ARGS_LOG\"\ncat \"$2\" > \"$PLOYZ_TEST_KEEPER_TOKEN_LOG\"\nexit \"${PLOYZ_TEST_KEEPER_EXIT:-0}\"\n",
    )
    .expect("fake keeper source can be written");
    write_executable(&fake_bin.join("curl"), fake_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--join-token", "join_once"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env(
            "PLOYZ_KEEPER_URL",
            format!("file://{}", keeper_source.display()),
        )
        .env("PLOYZ_KEEPER_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_VERSION", "0.0.1-alpha.1")
        .env("PLOYZ_NATS_URL", "nats://127.0.0.1:4222")
        .env("PLOYZ_TEST_KEEPER_ARGS_LOG", &keeper_args_log)
        .env("PLOYZ_TEST_KEEPER_TOKEN_LOG", &keeper_token_log)
        .output()
        .expect("bootstrap script can run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected_args = format!(
        "--join-token-file {}\n",
        state_dir.join("join-token").display()
    );
    assert_eq!(
        fs::read_to_string(&keeper_args_log).expect("keeper args are recorded"),
        expected_args
    );
    assert_eq!(
        fs::read_to_string(&keeper_token_log).expect("keeper token is recorded"),
        "join_once\n"
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_join_failure_is_not_masked_by_service_start() {
    let root = temp_dir("ployz-bootstrap-script-join-failure");
    let keeper_source = root.join("ployz-keeper-source");
    let script_path = test_bootstrap_script_path(&root, &root.join("bin"), &root.join("state"));
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(&keeper_source, "#!/bin/sh\nexit 42\n").expect("fake keeper source can be written");
    write_executable(&fake_bin.join("curl"), fake_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--join-token", "join_once"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env(
            "PLOYZ_KEEPER_URL",
            format!("file://{}", keeper_source.display()),
        )
        .env("PLOYZ_KEEPER_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_NATS_URL", "nats://127.0.0.1:4222")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
}

#[test]
fn bootstrap_script_rejects_positional_join_token() {
    let output = run_bootstrap_script(&["join_once"], None);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "--join-token <token>");
}

#[test]
fn bootstrap_script_rejects_unknown_join_token_flag() {
    let output = run_bootstrap_script(&["--token", "join_once"], None);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "unknown ployz installer argument: --token");
}

#[test]
fn bootstrap_script_rejects_channel_and_version_together() {
    let output = run_bootstrap_script(&["--channel", "alpha", "--version", "v0.0.2-alpha.1"], None);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "pass either --version/PLOYZ_VERSION or --channel/PLOYZ_CHANNEL, not both",
    );
}

#[test]
fn bootstrap_script_rejects_join_token_from_flag_and_env() {
    let output = run_bootstrap_script(&["--join-token", "join_once"], Some("join_env"));

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "set join token as either --join-token or PLOYZ_JOIN_TOKEN, not both",
    );
}

#[test]
fn bootstrap_script_rejects_join_token_and_first_node_together() {
    let output = run_bootstrap_script(&["--join-token", "join_once", "--first-node"], None);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "pass either --join-token or --first-node, not both",
    );
}

#[test]
fn bootstrap_script_first_node_requires_node_id() {
    let output = run_bootstrap_script(&["--first-node"], None);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "set PLOYZ_NODE_ID when bootstrapping a first node");
}

#[cfg(unix)]
#[test]
fn bootstrap_script_reports_missing_channel_key_and_url() {
    let root = temp_dir("ployz-bootstrap-script-missing-channel-key");
    let channel = root.join("alpha.env");
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(
        &channel,
        "PLOYZ_CHANNEL=alpha\nPLOYZ_VERSION=0.0.2-alpha.1\nPLOYZ_RELEASE_BASE_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1\n",
    )
    .expect("channel can be written");
    write_executable(&fake_bin.join("curl"), fake_release_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(bootstrap_script_path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("PLOYZ_TEST_ALPHA_CHANNEL", &channel)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_VERSION")
        .env_remove("PLOYZ_CHANNEL")
        .env_remove("PLOYZ_JOIN_TOKEN")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "release channel https://ployz.sh/channels/alpha.env is missing PLOYZ_RELEASE_TAG",
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_local_install_on_macos_needs_no_root_and_uses_home_bin() {
    let root = temp_dir("ployz-bootstrap-script-macos-local");
    let home = root.join("home");
    let ployzctl_source = root.join("ployzctl-source");
    let manifest = root.join("ployz-release-darwin-arm64.env");
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::create_dir_all(&home).expect("fake home can be created");
    fs::write(&ployzctl_source, "#!/bin/sh\nexit 0\n")
        .expect("fake ployzctl source can be written");
    fs::write(
        &manifest,
        format!(
            "PLOYZCTL_URL=file://{}\nPLOYZCTL_SHA256={}\n",
            ployzctl_source.display(),
            KEEPER_DIGEST
        ),
    )
    .expect("manifest can be written");
    write_executable(&fake_bin.join("curl"), fake_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(
        &fake_bin.join("uname"),
        &fake_uname_script_for("Darwin", "arm64"),
    );
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '501\\n'\n");

    let output = Command::new("sh")
        .arg(bootstrap_script_path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOME", &home)
        .env(
            "PLOYZ_RELEASE_MANIFEST_URL",
            format!("file://{}", manifest.display()),
        )
        .env_remove("PLOYZ_JOIN_TOKEN")
        .env_remove("PLOYZ_NATS_URL")
        .output()
        .expect("bootstrap script can run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = home.join(".local/bin/ployzctl");
    assert!(installed.exists());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!("installed {}", installed.display())));
    assert!(stdout.contains(&format!(
        "add {} to your PATH",
        home.join(".local/bin").display()
    )));
}

#[cfg(unix)]
#[test]
fn bootstrap_script_local_install_resolves_platform_named_manifest() {
    let root = temp_dir("ployz-bootstrap-script-manifest-name");
    let home = root.join("home");
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::create_dir_all(&home).expect("fake home can be created");
    write_executable(&fake_bin.join("curl"), "#!/bin/sh\nexit 22\n");
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(
        &fake_bin.join("uname"),
        &fake_uname_script_for("Darwin", "arm64"),
    );
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '501\\n'\n");

    let output = Command::new("sh")
        .arg(bootstrap_script_path())
        .args(["--version", "v0.0.2-alpha.1"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOME", &home)
        .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
        .env_remove("PLOYZ_JOIN_TOKEN")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(&output, "failed to download release manifest");
    assert_stderr_contains(&output, "ployz-release-darwin-arm64.env");
}

#[cfg(unix)]
#[test]
fn bootstrap_script_rejects_unsupported_operating_system() {
    let root = temp_dir("ployz-bootstrap-script-unsupported-os");
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_executable(
        &fake_bin.join("uname"),
        &fake_uname_script_for("SunOS", "x86_64"),
    );

    let output = Command::new("sh")
        .arg(bootstrap_script_path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("PLOYZ_JOIN_TOKEN")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "unsupported operating system: SunOS (ployz supports Linux and macOS)",
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_rejects_unsupported_architecture() {
    let root = temp_dir("ployz-bootstrap-script-unsupported-arch");
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_executable(
        &fake_bin.join("uname"),
        &fake_uname_script_for("Linux", "riscv64"),
    );

    let output = Command::new("sh")
        .arg(bootstrap_script_path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("PLOYZ_JOIN_TOKEN")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "unsupported architecture: riscv64 (ployz supports amd64 and arm64)",
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_join_mode_requires_linux() {
    let root = temp_dir("ployz-bootstrap-script-join-needs-linux");
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_executable(
        &fake_bin.join("uname"),
        &fake_uname_script_for("Darwin", "arm64"),
    );

    let output = Command::new("sh")
        .arg(bootstrap_script_path())
        .args(["--join-token", "join_once"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("PLOYZ_NATS_URL", "nats://127.0.0.1:4222")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "ployz machine bootstrap requires Linux; this machine is Darwin",
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_join_mode_requires_root() {
    let root = temp_dir("ployz-bootstrap-script-join-needs-root");
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '1000\\n'\n");

    let output = Command::new("sh")
        .arg(bootstrap_script_path())
        .args(["--join-token", "join_once"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("PLOYZ_NATS_URL", "nats://127.0.0.1:4222")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(&output, "ployz machine bootstrap must run as root");
}

#[cfg(unix)]
#[test]
fn bootstrap_script_reports_missing_manifest_key_and_url() {
    let root = temp_dir("ployz-bootstrap-script-missing-key");
    let manifest = root.join("ployz-release-linux-amd64.env");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(&manifest, "PLOYZCTL_URL=file:///tmp/ployzctl\n").expect("manifest can be written");
    write_executable(&fake_bin.join("curl"), fake_curl_script());
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\ncat >/dev/null\n");
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");
    let manifest_url = format!("file://{}", manifest.display());

    let output = Command::new("sh")
        .arg(script_path)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env("PLOYZ_RELEASE_MANIFEST_URL", &manifest_url)
        .env_remove("PLOYZ_JOIN_TOKEN")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        &format!("release manifest {manifest_url} is missing PLOYZCTL_SHA256"),
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_first_node_mode_runs_keeper_install_with_spec() {
    let root = temp_dir("ployz-bootstrap-script-first-node");
    let keeper_source = root.join("ployz-keeper-source");
    let nats_server = root.join("bin/nats-server");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    let keeper_args_log = root.join("keeper-args.log");
    let first_node_spec_log = root.join("first-node-spec.json");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::create_dir_all(nats_server.parent().expect("nats server has a parent"))
        .expect("nats dir exists");
    fs::write(&nats_server, "#!/bin/sh\nexit 0\n").expect("fake nats-server writes");
    write_executable(&nats_server, "#!/bin/sh\nexit 0\n");
    fs::write(
        &keeper_source,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$PLOYZ_TEST_KEEPER_ARGS_LOG\"\ncat \"$3\" > \"$PLOYZ_TEST_FIRST_NODE_SPEC_LOG\"\nexit 0\n",
    )
    .expect("fake keeper source can be written");
    write_executable(&fake_bin.join("curl"), fake_curl_script());
    write_executable(
        &fake_bin.join("sha256sum"),
        "#!/bin/sh\ncase \"${1:-}\" in\n  -c) read -r line; set -- $line; printf '%s: OK\\n' \"$2\"; exit 0 ;;\n  *) printf '%s  %s\\n' \"$PLOYZ_TEST_EXISTING_SHA\" \"$1\" ;;\nesac\n",
    );
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--first-node"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env(
            "PLOYZ_KEEPER_URL",
            format!("file://{}", keeper_source.display()),
        )
        .env("PLOYZ_KEEPER_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_VERSION", "0.0.2-alpha.1")
        .env("PLOYZ_NODE_ID", "node_1")
        .env("PLOYZ_MACHINE_JOIN_NATS_URL", "tls://203.0.113.10:4222")
        .env("PLOYZD_URL", "https://example.invalid/ployzd")
        .env("PLOYZD_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_EBPF_TC_URL", "https://example.invalid/ployz-ebpf-tc")
        .env("PLOYZ_EBPF_TC_SHA256", KEEPER_DIGEST)
        .env(
            "PLOYZ_EBPF_CTL_URL",
            "https://example.invalid/ployz-ebpf-ctl",
        )
        .env("PLOYZ_EBPF_CTL_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_TEST_KEEPER_ARGS_LOG", &keeper_args_log)
        .env("PLOYZ_TEST_FIRST_NODE_SPEC_LOG", &first_node_spec_log)
        .env("PLOYZ_TEST_EXISTING_SHA", KEEPER_DIGEST)
        .env_remove("PLOYZ_JOIN_TOKEN")
        .env_remove("PLOYZ_NATS_URL")
        .output()
        .expect("bootstrap script can run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(install_dir.join("ployz-keeper").exists());
    assert!(!install_dir.join("ployzctl").exists());
    let keeper_args = fs::read_to_string(&keeper_args_log).expect("keeper args are recorded");
    assert!(
        keeper_args.starts_with("first-node-install --spec "),
        "{keeper_args}"
    );
    let spec = fs::read_to_string(&first_node_spec_log).expect("first-node spec is recorded");
    serde_json::from_str::<serde_json::Value>(&spec).expect("first-node spec is valid JSON");
    assert!(!spec.contains(": OK"));
}

#[cfg(unix)]
#[test]
fn bootstrap_script_verifies_nats_archive_before_extracting() {
    let root = temp_dir("ployz-bootstrap-script-nats-archive");
    let keeper_source = root.join("ployz-keeper-source");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    let curl_log = root.join("curl.log");
    let sha_log = root.join("sha.log");
    let tar_log = root.join("tar.log");
    let install_log = root.join("install.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(
        &keeper_source,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$PLOYZ_TEST_KEEPER_ARGS_LOG\"\nexit 0\n",
    )
    .expect("fake keeper source can be written");
    write_executable(
        &fake_bin.join("curl"),
        "#!/bin/sh\nurl=\ndest=\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o) dest=\"$2\"; shift 2 ;;\n    -*) shift ;;\n    *) url=\"$1\"; shift ;;\n  esac\ndone\nprintf '%s\\n' \"$url\" >> \"$PLOYZ_TEST_CURL_LOG\"\ncase \"$url\" in\n  file://*) cp \"${url#file://}\" \"$dest\" ;;\n  https://github.com/nats-io/nats-server/releases/download/v2.14.2/nats-server-v2.14.2-linux-amd64.tar.gz) printf 'archive\\n' > \"$dest\" ;;\n  *) exit 2 ;;\nesac\n",
    );
    write_executable(
        &fake_bin.join("sha256sum"),
        "#!/bin/sh\ncase \"${1:-}\" in\n  -c) cat > \"$PLOYZ_TEST_SHA_LOG\"; exit \"${PLOYZ_TEST_SHA_EXIT:-0}\" ;;\n  *) printf '%s  %s\\n' \"$PLOYZ_TEST_EXISTING_SHA\" \"$1\" ;;\nesac\n",
    );
    write_executable(
        &fake_bin.join("tar"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PLOYZ_TEST_TAR_LOG\"\nmkdir -p /tmp/nats-server-v2.14.2-linux-amd64\nprintf '#!/bin/sh\\nexit 0\\n' > /tmp/nats-server-v2.14.2-linux-amd64/nats-server\n",
    );
    write_executable(
        &fake_bin.join("install"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PLOYZ_TEST_INSTALL_LOG\"\nif [ \"$1\" = \"-d\" ]; then\n  shift\n  while [ \"$#\" -gt 0 ]; do\n    case \"$1\" in -m) shift 2 ;; *) mkdir -p \"$1\"; shift ;; esac\n  done\nelse\n  mode=\n  if [ \"$1\" = \"-m\" ]; then mode=\"$2\"; shift 2; fi\n  cp \"$1\" \"$2\"\n  [ -n \"$mode\" ] && chmod \"$mode\" \"$2\"\nfi\n",
    );
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--first-node"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env(
            "PLOYZ_KEEPER_URL",
            format!("file://{}", keeper_source.display()),
        )
        .env("PLOYZ_KEEPER_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_VERSION", "0.0.2-alpha.1")
        .env("PLOYZ_NODE_ID", "node_1")
        .env("PLOYZ_MACHINE_JOIN_NATS_URL", "tls://203.0.113.10:4222")
        .env("PLOYZD_URL", "https://example.invalid/ployzd")
        .env("PLOYZD_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_EBPF_TC_URL", "https://example.invalid/ployz-ebpf-tc")
        .env("PLOYZ_EBPF_TC_SHA256", KEEPER_DIGEST)
        .env(
            "PLOYZ_EBPF_CTL_URL",
            "https://example.invalid/ployz-ebpf-ctl",
        )
        .env("PLOYZ_EBPF_CTL_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_TEST_CURL_LOG", &curl_log)
        .env("PLOYZ_TEST_SHA_LOG", &sha_log)
        .env("PLOYZ_TEST_TAR_LOG", &tar_log)
        .env("PLOYZ_TEST_INSTALL_LOG", &install_log)
        .env("PLOYZ_TEST_EXISTING_SHA", KEEPER_DIGEST)
        .env("PLOYZ_TEST_KEEPER_ARGS_LOG", root.join("keeper-args.log"))
        .env_remove("PLOYZ_JOIN_TOKEN")
        .env_remove("PLOYZ_NATS_URL")
        .output()
        .expect("bootstrap script can run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fs::read_to_string(&curl_log).expect("curl log is recorded").contains(
        "https://github.com/nats-io/nats-server/releases/download/v2.14.2/nats-server-v2.14.2-linux-amd64.tar.gz"
    ));
    assert!(
        fs::read_to_string(&sha_log)
            .expect("sha log is recorded")
            .contains("b3e7b14eb10c895fd90c2dacdb6b65bd3208adcc9524dd7689ba2c1024e6b97a")
    );
    assert!(
        fs::read_to_string(&tar_log)
            .expect("tar log is recorded")
            .contains("ployz-nats-server.tar.gz")
    );
    assert!(
        fs::read_to_string(&install_log)
            .expect("install log is recorded")
            .contains("nats-server")
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_does_not_extract_nats_archive_after_checksum_failure() {
    let root = temp_dir("ployz-bootstrap-script-nats-archive-failure");
    let keeper_source = root.join("ployz-keeper-source");
    let install_dir = root.join("bin");
    let state_dir = root.join("state");
    let script_path = test_bootstrap_script_path(&root, &install_dir, &state_dir);
    let fake_bin = root.join("fake-bin");
    let tar_log = root.join("tar.log");
    fs::create_dir_all(&fake_bin).expect("fake bin can be created");
    fs::write(&keeper_source, "#!/bin/sh\nexit 0\n").expect("fake keeper source can be written");
    write_executable(
        &fake_bin.join("curl"),
        "#!/bin/sh\nurl=\ndest=\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o) dest=\"$2\"; shift 2 ;;\n    -*) shift ;;\n    *) url=\"$1\"; shift ;;\n  esac\ndone\ncase \"$url\" in\n  file://*) cp \"${url#file://}\" \"$dest\" ;;\n  https://github.com/nats-io/nats-server/releases/download/v2.14.2/nats-server-v2.14.2-linux-amd64.tar.gz) printf 'archive\\n' > \"$dest\" ;;\n  *) exit 2 ;;\nesac\n",
    );
    write_executable(
        &fake_bin.join("sha256sum"),
        "#!/bin/sh\ncase \"${1:-}\" in\n  -c) cat >/dev/null; exit 1 ;;\n  *) printf '%s  %s\\n' \"$PLOYZ_TEST_EXISTING_SHA\" \"$1\" ;;\nesac\n",
    );
    write_executable(
        &fake_bin.join("tar"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PLOYZ_TEST_TAR_LOG\"\nexit 0\n",
    );
    write_executable(
        &fake_bin.join("install"),
        "#!/bin/sh\nif [ \"$1\" = \"-d\" ]; then\n  shift\n  while [ \"$#\" -gt 0 ]; do\n    case \"$1\" in -m) shift 2 ;; *) mkdir -p \"$1\"; shift ;; esac\n  done\nelse\n  exit 9\nfi\n",
    );
    write_executable(&fake_bin.join("uname"), fake_uname_script());
    write_executable(&fake_bin.join("id"), "#!/bin/sh\nprintf '0\\n'\n");

    let output = Command::new("sh")
        .arg(script_path)
        .args(["--first-node"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        )
        .env(
            "PLOYZ_KEEPER_URL",
            format!("file://{}", keeper_source.display()),
        )
        .env("PLOYZ_KEEPER_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_VERSION", "0.0.2-alpha.1")
        .env("PLOYZ_NODE_ID", "node_1")
        .env("PLOYZ_MACHINE_JOIN_NATS_URL", "tls://203.0.113.10:4222")
        .env("PLOYZD_URL", "https://example.invalid/ployzd")
        .env("PLOYZD_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_EBPF_TC_URL", "https://example.invalid/ployz-ebpf-tc")
        .env("PLOYZ_EBPF_TC_SHA256", KEEPER_DIGEST)
        .env(
            "PLOYZ_EBPF_CTL_URL",
            "https://example.invalid/ployz-ebpf-ctl",
        )
        .env("PLOYZ_EBPF_CTL_SHA256", KEEPER_DIGEST)
        .env("PLOYZ_TEST_EXISTING_SHA", KEEPER_DIGEST)
        .env("PLOYZ_TEST_TAR_LOG", &tar_log)
        .env_remove("PLOYZ_JOIN_TOKEN")
        .env_remove("PLOYZ_NATS_URL")
        .output()
        .expect("bootstrap script can run");

    assert!(!output.status.success());
    assert!(
        !tar_log.exists(),
        "tar must not run after NATS archive checksum failure"
    );
    assert!(!install_dir.join("nats-server").exists());
}

fn unique_temp_path(prefix: &str) -> PathBuf {
    let unique = format!(
        "{}-{}",
        prefix,
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

fn fake_curl_script() -> &'static str {
    "#!/bin/sh\nurl=\ndest=\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o)\n      dest=\"$2\"\n      shift 2\n      ;;\n    -*)\n      shift\n      ;;\n    *)\n      url=\"$1\"\n      shift\n      ;;\n  esac\ndone\ncase \"$url\" in\n  file://*) cp \"${url#file://}\" \"$dest\" ;;\n  *) exit 2 ;;\nesac\n"
}

fn fake_release_curl_script() -> &'static str {
    "#!/bin/sh\nurl=\ndest=\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o)\n      dest=\"$2\"\n      shift 2\n      ;;\n    -*)\n      shift\n      ;;\n    *)\n      url=\"$1\"\n      shift\n      ;;\n  esac\ndone\nif [ -n \"${PLOYZ_TEST_CURL_LOG:-}\" ]; then\n  printf '%s\\n' \"$url\" >> \"$PLOYZ_TEST_CURL_LOG\"\nfi\ncase \"$url\" in\n  file://*) cp \"${url#file://}\" \"$dest\" ;;\n  https://ployz.sh/channels/alpha.env) cp \"$PLOYZ_TEST_ALPHA_CHANNEL\" \"$dest\" ;;\n  https://ployz.sh/channels/beta.env) cp \"$PLOYZ_TEST_BETA_CHANNEL\" \"$dest\" ;;\n  https://github.com/getployz/ployz/releases/download/*/ployz-release-*.env) cp \"$PLOYZ_TEST_RELEASE_MANIFEST\" \"$dest\" ;;\n  *) exit 2 ;;\nesac\n"
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

fn test_bootstrap_script_path(
    root: &std::path::Path,
    install_dir: &std::path::Path,
    state_dir: &std::path::Path,
) -> PathBuf {
    let source = fs::read_to_string(bootstrap_script_path()).expect("bootstrap script is readable");
    let rewritten = replace_required(
        source,
        "install_dir=\"/usr/local/bin\"",
        &format!("install_dir=\"{}\"", install_dir.display()),
    );
    let rewritten = replace_required(
        rewritten,
        "state_dir=\"/var/lib/ployz/keeper\"",
        &format!("state_dir=\"{}\"", state_dir.display()),
    );
    let rewritten = replace_required(
        rewritten,
        "nats_binary=\"/usr/local/bin/nats-server\"",
        &format!(
            "nats_binary=\"{}\"",
            install_dir.join("nats-server").display()
        ),
    );
    assert!(!rewritten.contains("install_dir=\"/usr/local/bin\""));
    assert!(!rewritten.contains("state_dir=\"/var/lib/ployz/keeper\""));
    assert!(!rewritten.contains("nats_binary=\"/usr/local/bin/nats-server\""));
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

fn run_bootstrap_script(args: &[&str], join_token_env: Option<&str>) -> Output {
    let mut command = Command::new("sh");
    command
        .arg(bootstrap_script_path())
        .args(args)
        .env("PLOYZ_KEEPER_URL", "https://example.invalid/ployz-keeper")
        .env("PLOYZ_KEEPER_SHA256", KEEPER_DIGEST);

    match join_token_env {
        Some(token) => {
            command.env("PLOYZ_JOIN_TOKEN", token);
        }
        None => {
            command.env_remove("PLOYZ_JOIN_TOKEN");
        }
    }

    command.output().expect("bootstrap script can run")
}

fn assert_stderr_contains(output: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "stderr should contain {expected:?}, got {stderr:?}"
    );
}

const KEEPER_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
