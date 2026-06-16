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
    fs::remove_file(assets_dir.join("ployzctl-darwin-arm64")).expect("asset can be removed");

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "references missing asset ployzctl-darwin-arm64");
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
            ("PLOYZCTL", "ployzctl-linux-amd64"),
            ("PLOYZ_KEEPER", "ployz-keeper-linux-amd64"),
            ("PLOYZD", "ployzd-linux-amd64"),
            ("PLOYZ_EBPF_CTL", "ployz-ebpf-ctl-linux-amd64"),
            ("PLOYZ_EBPF_TC", "ployz-ebpf-tc-linux-amd64"),
        ],
    );
    write_platform(
        assets_dir,
        release_tag,
        semver,
        "linux-arm64",
        &[
            ("PLOYZCTL", "ployzctl-linux-arm64"),
            ("PLOYZ_KEEPER", "ployz-keeper-linux-arm64"),
            ("PLOYZD", "ployzd-linux-arm64"),
            ("PLOYZ_EBPF_CTL", "ployz-ebpf-ctl-linux-arm64"),
            ("PLOYZ_EBPF_TC", "ployz-ebpf-tc-linux-arm64"),
        ],
    );
    write_platform(
        assets_dir,
        release_tag,
        semver,
        "darwin-amd64",
        &[("PLOYZCTL", "ployzctl-darwin-amd64")],
    );
    write_platform(
        assets_dir,
        release_tag,
        semver,
        "darwin-arm64",
        &[("PLOYZCTL", "ployzctl-darwin-arm64")],
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
