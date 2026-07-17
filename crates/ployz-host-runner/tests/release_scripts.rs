use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{env, fs};

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[cfg(unix)]
#[test]
fn dind_builder_modes_label_machine_image_and_cache_workload_tars() {
    let fixture = FakeDocker::new("ployz-dind-builder");
    seed_workload_tars(&fixture);

    let unknown = fixture.run_builder(&["unknown"]);
    assert!(!unknown.status.success());
    assert_stderr_contains(&unknown, "unknown build mode: unknown");

    let artifacts = fixture.run_builder(&["artifacts-only"]);
    assert_success(&artifacts);
    assert!(String::from_utf8_lossy(&artifacts.stdout).contains("Linux release artifacts built:"));
    assert_eq!(fixture.log(), "");

    let fingerprint = fixture.fingerprint();
    let full = fixture.run_builder(&[]);
    assert_success(&full);
    let log = fixture.log();
    assert_eq!(log.matches("pull --platform linux/amd64").count(), 4);
    assert!(!log.contains("save -o"));
    assert!(log.contains("--label dev.ployz.dind.managed=true"));
    assert!(log.contains(&format!("--label dev.ployz.dind.fingerprint={fingerprint}")));

    fixture.clear_log();
    fs::write(
        fixture.0.join("target/workload-image-stamps/nginx.stamp"),
        "stale\n",
    )
    .expect("stamp can be changed");
    fs::remove_file(fixture.context().join("registry.tar")).expect("tar can be removed");
    let changed = fixture.run_builder(&[]);
    assert_success(&changed);
    let log = fixture.log();
    assert!(
        log.lines()
            .any(|line| line.contains("save -o") && line.ends_with(" nginx:1.27-alpine"))
    );
    assert!(
        log.lines()
            .any(|line| line.contains("save -o") && line.ends_with(" registry:2.8.3"))
    );
    assert_eq!(log.matches("save -o").count(), 2);
}

#[cfg(unix)]
#[test]
fn dind_builder_exports_railpack_through_the_root_builder() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = FakeDocker::new("ployz-dind-railpack-export");
    let release = fixture.0.join("target/release");
    fs::remove_file(release.join("railpack")).expect("cached Railpack can be removed");
    fs::remove_file(release.join("railpack.source")).expect("Railpack stamp can be removed");
    fs::set_permissions(&release, fs::Permissions::from_mode(0o555))
        .expect("release directory can model root ownership");

    let output = fixture.run_builder(&["artifacts-only"]);
    fs::set_permissions(&release, fs::Permissions::from_mode(0o755))
        .expect("release directory permissions can be restored");
    assert_success(&output);
    let log = fixture.log();
    assert!(log.contains(&format!(
        "--volume {}:/target",
        fixture.0.join("target").display()
    )));
    assert!(log.contains(":/railpack-input:ro"));
    assert!(log.contains("install -m 0755 /railpack-input/railpack"));
    assert!(log.contains("install -m 0644 /railpack-input/railpack.source"));
}

#[cfg(unix)]
#[test]
fn filtered_dind_reuses_matching_substrate_and_unfiltered_is_full() {
    let fixture = FakeDocker::new("ployz-dind-filtered");
    seed_workload_tars(&fixture);
    let fingerprint = fixture.fingerprint();

    let matching = fixture.run_dind(
        Some("scenario_machine_add"),
        &format!("linux/amd64 {fingerprint}"),
        false,
    );
    assert_success(&matching);
    assert!(String::from_utf8_lossy(&matching.stdout).contains("Linux release artifacts built:"));
    let log = fixture.log();
    assert!(log.contains("index .Config.Labels \"dev.ployz.dind.fingerprint\""));
    assert!(log.contains("--env CARGO_INCREMENTAL=1"));
    assert!(!log.contains("pull --platform"));

    for identity in ["", "linux/arm64 wrong", "linux/amd64 stale"] {
        fixture.clear_log();
        let output = fixture.run_dind(Some("scenario_machine_add"), identity, true);
        assert_success(&output);
        assert!(fixture.log().contains("pull --platform linux/amd64"));
    }

    fixture.clear_log();
    let full = fixture.run_dind(None, &format!("linux/amd64 {fingerprint}"), true);
    assert_success(&full);
    let log = fixture.log();
    assert!(log.contains("pull --platform linux/amd64"));
    assert!(log.contains("group_ --skip acceptance::group_v1_acceptance"));
    assert!(log.contains("acceptance::group_v1_acceptance --exact"));
}

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
fn release_asset_verifier_rejects_missing_railpack_asset() {
    let root = temp_dir("ployz-release-verify-missing-railpack");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    fs::remove_file(assets_dir.join("railpack-linux-amd64"))
        .expect("Railpack asset can be removed");

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "references missing asset railpack-linux-amd64");
}

#[cfg(unix)]
#[test]
fn release_asset_verifier_rejects_an_unpinned_railpack_version() {
    let root = temp_dir("ployz-release-verify-railpack-version");
    let assets_dir = root.join("assets");
    fs::create_dir_all(&assets_dir).expect("assets dir can be created");
    write_complete_release_assets(&assets_dir, "v0.0.2-alpha.1");
    replace_in_file(
        &assets_dir.join("ployz-release-linux-amd64.env"),
        "PLOYZ_RAILPACK_VERSION=v0.31.0",
        "PLOYZ_RAILPACK_VERSION=v0.32.0",
    );

    let output = run_verifier(&[
        "v0.0.2-alpha.1",
        "--assets-dir",
        assets_dir.to_str().expect("asset dir is utf8"),
    ]);

    assert!(!output.status.success());
    assert_stderr_contains(&output, "PLOYZ_RAILPACK_VERSION=v0.32.0, expected v0.31.0");
}

#[cfg(unix)]
#[test]
fn release_packager_stages_the_checksum_verified_native_railpack_helper() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_dir("ployz-release-package-railpack");
    let artifact_dir = root.join("artifacts");
    let dist_dir = root.join("dist");
    let fixture_dir = root.join("railpack-fixture");
    let bin_dir = root.join("bin");
    fs::create_dir_all(&artifact_dir).expect("artifact dir can be created");
    fs::create_dir_all(&fixture_dir).expect("fixture dir can be created");
    fs::create_dir_all(&bin_dir).expect("bin dir can be created");
    for artifact in ["ployz", "ployzd", "ployz-ebpf-ctl", "ployz-ebpf-tc"] {
        fs::write(artifact_dir.join(artifact), artifact).expect("artifact can be written");
    }
    fs::write(fixture_dir.join("railpack"), "railpack-amd64\n")
        .expect("Railpack fixture can be written");
    let archive = root.join("railpack.tar.gz");
    let tar = Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .args(["-C"])
        .arg(&fixture_dir)
        .arg("railpack")
        .output()
        .expect("fixture archive can be created");
    assert_success(&tar);

    let curl = bin_dir.join("curl");
    fs::write(
        &curl,
        r#"#!/usr/bin/env bash
set -euo pipefail
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    *) shift ;;
  esac
done
cp "${PLOYZ_TEST_RAILPACK_ARCHIVE}" "${output}"
"#,
    )
    .expect("fake curl can be written");
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o755))
        .expect("fake curl can be executable");
    let sha256sum = bin_dir.join("sha256sum");
    fs::write(
        &sha256sum,
        r#"#!/usr/bin/env bash
set -euo pipefail
case "${1##*/}" in
  railpack-v0.31.0-x86_64-unknown-linux-musl.tar.gz)
    printf 'f75416cf4c452db2841d864f54dbfd8e4d77f2d4a02b23b87561e7760fa278fd  %s\n' "$1"
    ;;
  *) exec /usr/bin/sha256sum "$@" ;;
esac
"#,
    )
    .expect("fake sha256sum can be written");
    fs::set_permissions(&sha256sum, fs::Permissions::from_mode(0o755))
        .expect("fake sha256sum can be executable");

    let output = Command::new("bash")
        .arg(repo_path("scripts/package-release.sh"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .env("PLOYZ_RELEASE_SKIP_BUILD", "1")
        .env("PLOYZ_RELEASE_VERSION", "v0.0.2-alpha.1")
        .env("PLOYZ_RELEASE_PLATFORM", "linux/amd64")
        .env("PLOYZ_RELEASE_ARTIFACT_DIR", &artifact_dir)
        .env("PLOYZ_RELEASE_DIST_DIR", &dist_dir)
        .env("PLOYZ_NATS_SERVER_VERSION", "2.14.2")
        .env("PLOYZ_NATS_SERVER_URL", "https://example.invalid/nats.tar.gz")
        .env("PLOYZ_NATS_SERVER_SHA256", EMPTY_SHA256)
        .env("PLOYZ_RAILPACK_VERSION", "v0.31.0")
        .env(
            "PLOYZ_RAILPACK_ARCHIVE_URL",
            "https://github.com/railwayapp/railpack/releases/download/v0.31.0/railpack-v0.31.0-x86_64-unknown-linux-musl.tar.gz",
        )
        .env(
            "PLOYZ_RAILPACK_ARCHIVE_SHA256",
            "f75416cf4c452db2841d864f54dbfd8e4d77f2d4a02b23b87561e7760fa278fd",
        )
        .env("PLOYZ_TEST_RAILPACK_ARCHIVE", &archive)
        .output()
        .expect("release packager can run");
    assert_success(&output);

    let railpack = dist_dir.join("railpack-linux-amd64");
    assert_eq!(
        fs::read_to_string(&railpack).expect("staged Railpack is readable"),
        "railpack-amd64\n"
    );
    assert_ne!(
        fs::metadata(&railpack)
            .expect("staged Railpack metadata is readable")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    let manifest = fs::read_to_string(dist_dir.join("ployz-release-linux-amd64.env"))
        .expect("release manifest is readable");
    assert!(manifest.contains("PLOYZ_RAILPACK_VERSION=v0.31.0\n"));
    assert!(manifest.contains(
        "PLOYZ_RAILPACK_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1/railpack-linux-amd64\n"
    ));
}

#[cfg(unix)]
#[test]
fn release_packager_rejects_a_non_official_railpack_archive() {
    let root = temp_dir("ployz-release-package-wrong-railpack");
    let artifact_dir = root.join("artifacts");
    fs::create_dir_all(&artifact_dir).expect("artifact dir can be created");
    for artifact in ["ployz", "ployzd", "ployz-ebpf-ctl", "ployz-ebpf-tc"] {
        fs::write(artifact_dir.join(artifact), artifact).expect("artifact can be written");
    }

    let output = Command::new("bash")
        .arg(repo_path("scripts/package-release.sh"))
        .env("PLOYZ_RELEASE_SKIP_BUILD", "1")
        .env("PLOYZ_RELEASE_PLATFORM", "linux/amd64")
        .env("PLOYZ_RELEASE_ARTIFACT_DIR", &artifact_dir)
        .env("PLOYZ_RELEASE_DIST_DIR", root.join("dist"))
        .env("PLOYZ_RAILPACK_VERSION", "v0.31.0")
        .env(
            "PLOYZ_RAILPACK_ARCHIVE_URL",
            "https://example.invalid/railpack.tar.gz",
        )
        .env(
            "PLOYZ_RAILPACK_ARCHIVE_SHA256",
            "f75416cf4c452db2841d864f54dbfd8e4d77f2d4a02b23b87561e7760fa278fd",
        )
        .output()
        .expect("release packager can run");

    assert!(!output.status.success());
    assert_stderr_contains(&output, "unexpected Railpack release URL");
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
            ("PLOYZ_RAILPACK", "railpack-linux-amd64"),
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
            ("PLOYZ_RAILPACK", "railpack-linux-arm64"),
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
        manifest.push_str("PLOYZ_RAILPACK_VERSION=v0.31.0\n");
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

#[cfg(unix)]
struct FakeDocker(PathBuf);

#[cfg(unix)]
impl FakeDocker {
    fn new(prefix: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir(prefix);
        fs::create_dir(root.join("bin")).expect("fake bin can be created");
        fs::create_dir(root.join("context")).expect("fake context can be created");
        fs::create_dir_all(root.join("target/release")).expect("fake target can be created");
        for file in ["Dockerfile", "daemon.json", "ployz-dind-images.service"] {
            fs::copy(
                repo_path(&format!("docker/dind-machine/{file}")),
                root.join("context").join(file),
            )
            .expect("fingerprint input can be copied");
        }

        let docker = root.join("bin/docker");
        fs::write(
            &docker,
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${PLOYZ_FAKE_LOG}"
if [ "${0##*/}" = cargo ]; then exit 0; fi
case "${1:-}" in
  info) printf 'amd64\n' ;;
  buildx) printf 'sha256:%s\n' "${6}" ;;
  image) case "${4:-}" in
    *dev.ployz.dind.fingerprint*) printf '%s\n' "${PLOYZ_FAKE_MACHINE_IDENTITY:-}" ;;
    *Architecture*) printf 'linux/amd64\n' ;;
    *Id*) printf 'sha256:%s\n' "${5}" ;;
  esac ;;
  save) printf 'saved\n' > "${3}" ;;
esac
"#,
        )
        .expect("fake docker can be written");
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o755))
            .expect("fake docker can be executable");
        write_fake_railpack_download_tools(&root);
        let railpack = root.join("target/release/railpack");
        fs::write(&railpack, "fake railpack\n").expect("fake Railpack can be written");
        fs::set_permissions(&railpack, fs::Permissions::from_mode(0o755))
            .expect("fake Railpack can be executable");
        fs::write(
            root.join("target/release/railpack.source"),
            "linux/amd64 v0.31.0 f75416cf4c452db2841d864f54dbfd8e4d77f2d4a02b23b87561e7760fa278fd\n",
        )
        .expect("fake Railpack stamp can be written");
        fs::hard_link(&docker, root.join("bin/cargo")).expect("fake cargo can be linked");
        Self(root)
    }

    fn command(&self, script: &str) -> Command {
        let mut command = Command::new("bash");
        command
            .arg(repo_path(script))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.0.join("bin").display()),
            )
            .env("PLOYZ_FAKE_LOG", self.0.join("commands.log"))
            .env("PLOYZ_DIND_CONTEXT_DIR", self.context())
            .env("PLOYZ_DIND_TARGET_DIR", self.0.join("target"))
            .env("PLOYZ_DIND_EBPF_TARGET_DIR", self.0.join("ebpf"))
            .env("PLOYZ_DIND_CARGO_REGISTRY_DIR", self.0.join("registry"))
            .env("PLOYZ_DIND_CARGO_GIT_DIR", self.0.join("git"))
            .env("PLOYZ_DIND_PLATFORM", "linux/amd64");
        command
    }

    fn fingerprint(&self) -> String {
        let output = self.run_builder(&["fingerprint"]);
        assert_success(&output);
        String::from_utf8(output.stdout)
            .expect("fingerprint is utf8")
            .trim()
            .into()
    }

    fn run_builder(&self, args: &[&str]) -> Output {
        self.command("scripts/build-dind-machine-image.sh")
            .args(args)
            .env("PLOYZ_DIND_SKIP_BUILD", "1")
            .output()
            .expect("DinD builder can run")
    }

    fn run_dind(&self, filter: Option<&str>, identity: &str, skip: bool) -> Output {
        let mut command = self.command("scripts/dind-e2e.sh");
        command.args(filter);
        command
            .env("PLOYZ_FAKE_MACHINE_IDENTITY", identity)
            .env("PLOYZ_DIND_SKIP_BUILD", if skip { "1" } else { "0" })
            .output()
            .expect("DinD wrapper can run")
    }

    fn context(&self) -> PathBuf {
        self.0.join("context")
    }
    fn log(&self) -> String {
        fs::read_to_string(self.0.join("commands.log")).unwrap_or_default()
    }
    fn clear_log(&self) {
        fs::write(self.0.join("commands.log"), "").expect("log can be cleared");
    }
}

#[cfg(unix)]
fn write_fake_railpack_download_tools(root: &Path) {
    use std::os::unix::fs::PermissionsExt;

    for (name, script) in [
        (
            "curl",
            r#"#!/bin/sh
set -eu
output=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --output ]; then output=$2; shift 2; else shift; fi
done
: > "${output}"
"#,
        ),
        (
            "sha256sum",
            r#"#!/bin/sh
case "$1" in
  */ployz-dind-railpack.*/*)
    printf '%s  %s\n' f75416cf4c452db2841d864f54dbfd8e4d77f2d4a02b23b87561e7760fa278fd "$1"
    ;;
  *) exec /usr/bin/sha256sum "$@" ;;
esac
"#,
        ),
        (
            "tar",
            r#"#!/bin/sh
set -eu
destination=
while [ "$#" -gt 0 ]; do
  if [ "$1" = -C ]; then destination=$2; shift 2; else shift; fi
done
printf '%s\n' '#!/bin/sh' 'exit 0' > "${destination}/railpack"
chmod 0755 "${destination}/railpack"
"#,
        ),
    ] {
        let path = root.join("bin").join(name);
        fs::write(&path, script).expect("fake Railpack download tool can be written");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("fake Railpack download tool can be executable");
    }
}

#[cfg(unix)]
fn seed_workload_tars(fake: &FakeDocker) {
    let stamps = fake.0.join("target/workload-image-stamps");
    fs::create_dir_all(&stamps).expect("stamp dir can be created");
    for (name, image) in [
        ("nginx", "nginx:1.27-alpine"),
        ("registry", "registry:2.8.3"),
        ("umami", "ghcr.io/umami-software/umami:postgresql-latest"),
        ("postgres", "postgres:15-alpine"),
    ] {
        fs::write(fake.context().join(format!("{name}.tar")), "tar")
            .expect("workload tar can be written");
        fs::write(
            stamps.join(format!("{name}.stamp")),
            format!("linux/amd64 {image} sha256:{image}\n"),
        )
        .expect("workload stamp can be written");
    }
}

#[cfg(unix)]
fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
