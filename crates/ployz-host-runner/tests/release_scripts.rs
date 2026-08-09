use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{env, fs};

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[cfg(unix)]
#[test]
fn release_asset_verifier_accepts_complete_assets_and_prints_channel() {
    let root = temp_dir("ployz-release-verify-complete");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
        "--channel",
        "alpha",
        "--print-channel",
    ]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PLOYZ_CHANNEL=alpha"));
    assert!(stdout.contains("PLOYZ_RELEASE_TAG=v0.0.2-alpha.1"));
    assert!(stdout.contains("PLOYZ_VERSION=0.0.2-alpha.1"));
    assert!(stdout.contains(
        "PLOYZ_RELEASE_BASE_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1"
    ));
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_writes_channel_file() {
    let root = temp_dir("ployz-release-verify-write-channel");
    let assets_dir = root.join("assets");
    let channel_path = root.join("site/channels/beta.env");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
        "--channel",
        "beta",
        "--write-channel",
        channel_path.to_str().expect("channel path is utf8"),
    ]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&channel_path).expect("channel file is written"),
        "PLOYZ_CHANNEL=beta\nPLOYZ_RELEASE_TAG=v0.0.2-alpha.1\nPLOYZ_VERSION=0.0.2-alpha.1\nPLOYZ_RELEASE_BASE_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1\n"
    );
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_missing_platform_manifest() {
    let root = temp_dir("ployz-release-verify-missing-manifest");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    fs::remove_file(assets_dir.join("ployz-release-linux-arm64.env"))
        .expect("manifest can be removed");

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "missing release manifest: ployz-release-linux-arm64.env",
    );
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_manifest_referencing_missing_asset() {
    let root = temp_dir("ployz-release-verify-missing-asset");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    fs::remove_file(assets_dir.join("ployz-darwin-arm64")).expect("asset can be removed");

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "references missing asset ployz-darwin-arm64");
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_missing_corrosion_asset() {
    let root = temp_dir("ployz-release-verify-missing-corrosion");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    fs::remove_file(assets_dir.join("corrosion-linux-arm64"))
        .expect("Corrosion asset can be removed");

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "references missing asset corrosion-linux-arm64");
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_missing_corrosion_schema_asset() {
    let root = temp_dir("ployz-release-verify-missing-corrosion-schema");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    fs::remove_file(assets_dir.join("corrosion-schema-v1-linux-amd64.sql"))
        .expect("Corrosion schema asset can be removed");

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "references missing asset corrosion-schema-v1-linux-amd64.sql",
    );
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_wrong_corrosion_embedded_version() {
    let root = temp_dir("ployz-release-verify-corrosion-version");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    replace_in_file(
        &assets_dir.join("ployz-release-linux-amd64.env"),
        "PLOYZ_CORROSION_EMBEDDED_VERSION=corrosion 0.2.0-beta.0",
        "PLOYZ_CORROSION_EMBEDDED_VERSION=corrosion 1.0.0",
    );

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "PLOYZ_CORROSION_EMBEDDED_VERSION=corrosion 1.0.0, expected corrosion 0.2.0-beta.0",
    );
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_manifest_identity_mismatch() {
    let root = temp_dir("ployz-release-verify-identity-mismatch");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    replace_in_file(
        &assets_dir.join("ployz-release-linux-amd64.env"),
        "PLOYZ_RELEASE_TAG=v0.0.2-alpha.1",
        "PLOYZ_RELEASE_TAG=v0.0.3-alpha.1",
    );

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "has PLOYZ_RELEASE_TAG=v0.0.3-alpha.1, expected v0.0.2-alpha.1",
    );
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_asset_url_outside_release() {
    let root = temp_dir("ployz-release-verify-url-authority");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    replace_in_file(
        &assets_dir.join("ployz-release-linux-amd64.env"),
        "PLOYZ_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1/ployz-linux-amd64",
        "PLOYZ_URL=https://github.com/getployz/ployz/releases/download/v0.0.3-alpha.1/ployz-linux-amd64",
    );

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "expected https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1/ployz-linux-amd64",
    );
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_swapped_asset_url_inside_release() {
    let root = temp_dir("ployz-release-verify-swapped-asset");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    replace_in_file(
        &assets_dir.join("ployz-release-linux-amd64.env"),
        "PLOYZ_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1/ployz-linux-amd64",
        "PLOYZ_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1/ployzd-linux-amd64",
    );

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "expected https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1/ployz-linux-amd64",
    );
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_asset_sha_mismatch() {
    let root = temp_dir("ployz-release-verify-sha-mismatch");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    replace_in_file(
        &assets_dir.join("ployz-release-darwin-arm64.env"),
        &format!("PLOYZ_SHA256={EMPTY_SHA256}"),
        "PLOYZ_SHA256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "has SHA-256");
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_bare_v_tag() {
    let root = temp_dir("ployz-release-verify-bare-v");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");

    let output = run_verifier(&[
        "v",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "release tag must include a version after v: v");
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_tag_without_version_core() {
    let root = temp_dir("ployz-release-verify-invalid-version-core");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");

    let output = run_verifier(&[
        "valpha",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "release tag must look like vX.Y.Z or vX.Y.Z-suffix: valpha",
    );
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_non_numeric_version_core() {
    let root = temp_dir("ployz-release-verify-nonnumeric-version-core");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");

    let output = run_verifier(&[
        "v1.x.3",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(
        &output,
        "release tag must look like vX.Y.Z or vX.Y.Z-suffix: v1.x.3",
    );
}

#[test]
fn public_installer_points_successful_cli_installs_to_init() {
    let installer =
        fs::read_to_string(repo_path("scripts/ployz.sh")).expect("public installer is readable");

    assert!(installer.contains("default install next step: sudo ployz init"));
    assert!(installer.contains("run: sudo ployz init"));
    assert!(!installer.contains("sudo ployz host bootstrap"));
}

fn write_complete_release_assets(assets_dir: &Path, release_tag: &str) {
    let semver = release_tag
        .strip_prefix('v')
        .expect("test release tag starts with v");
    write_platform(
        assets_dir,
        release_tag,
        semver,
        "linux-amd64",
        &[
            ("PLOYZ", "ployz-linux-amd64"),
            ("PLOYZD", "ployzd-linux-amd64"),
            ("PLOYZ_EBPF_CTL", "ployz-ebpf-ctl-linux-amd64"),
            ("PLOYZ_EBPF_TC", "ployz-ebpf-tc-linux-amd64"),
            ("PLOYZ_CORROSION", "corrosion-linux-amd64"),
            (
                "PLOYZ_CORROSION_SCHEMA",
                "corrosion-schema-v1-linux-amd64.sql",
            ),
        ],
    );
    write_platform(
        assets_dir,
        release_tag,
        semver,
        "linux-arm64",
        &[
            ("PLOYZ", "ployz-linux-arm64"),
            ("PLOYZD", "ployzd-linux-arm64"),
            ("PLOYZ_EBPF_CTL", "ployz-ebpf-ctl-linux-arm64"),
            ("PLOYZ_EBPF_TC", "ployz-ebpf-tc-linux-arm64"),
            ("PLOYZ_CORROSION", "corrosion-linux-arm64"),
            (
                "PLOYZ_CORROSION_SCHEMA",
                "corrosion-schema-v1-linux-arm64.sql",
            ),
        ],
    );
    write_platform(
        assets_dir,
        release_tag,
        semver,
        "darwin-amd64",
        &[("PLOYZ", "ployz-darwin-amd64")],
    );
    write_platform(
        assets_dir,
        release_tag,
        semver,
        "darwin-arm64",
        &[("PLOYZ", "ployz-darwin-arm64")],
    );
}

fn write_platform(
    assets_dir: &Path,
    release_tag: &str,
    semver: &str,
    platform: &str,
    assets: &[(&str, &str)],
) {
    let mut manifest = format!(
        "PLOYZ_VERSION={semver}\nPLOYZ_RELEASE_TAG={release_tag}\nPLOYZ_RELEASE_PLATFORM={platform}\n"
    );
    if platform.starts_with("linux-") {
        manifest.push_str("PLOYZ_CORROSION_EMBEDDED_VERSION=corrosion 0.2.0-beta.0\n");
    }
    for (key, asset) in assets {
        fs::write(assets_dir.join(asset), "").expect("asset can be written");
        manifest.push_str(&format!(
            "{key}_URL=https://github.com/getployz/ployz/releases/download/{release_tag}/{asset}\n{key}_SHA256={EMPTY_SHA256}\n"
        ));
    }
    fs::write(
        assets_dir.join(format!("ployz-release-{platform}.env")),
        manifest,
    )
    .expect("manifest can be written");
}

fn replace_in_file(path: &Path, from: &str, to: &str) {
    let source = fs::read_to_string(path).expect("file is readable");
    assert!(
        source.contains(from),
        "test fixture missing replacement needle {from:?}"
    );
    fs::write(path, source.replace(from, to)).expect("file can be rewritten");
}

fn run_verifier(args: &[&str]) -> Output {
    Command::new("bash")
        .arg(repo_path("scripts/verify-release-assets.sh"))
        .args(args)
        .output()
        .expect("release verifier can run")
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
    env::temp_dir().join(unique)
}

fn temp_dir(prefix: &str) -> PathBuf {
    let path = unique_temp_path(prefix);
    fs::create_dir_all(&path).expect("temp dir can be created");
    path
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn assert_stderr_contains(output: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "stderr should contain {expected:?}, got {stderr:?}"
    );
}
