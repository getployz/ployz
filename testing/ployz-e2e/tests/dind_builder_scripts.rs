#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[test]
fn builder_modes_label_machine_image_and_cache_workload_tars() {
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

#[test]
fn fingerprint_failure_is_atomic_before_label_or_cache_trust() {
    let fixture = FakeDocker::new("ployz-dind-fingerprint-failure");
    seed_workload_tars(&fixture);
    let images = [
        "mirror.gcr.io/library/nginx:1.27-alpine",
        "mirror.gcr.io/library/registry:2.8.3",
        "ghcr.io/umami-software/umami:postgresql-latest@sha256:8edfe4beaef13f9d1300619fa264ef250a3688df9cc54d24ca830ca31cb475ec",
        "mirror.gcr.io/library/postgres:15-alpine@sha256:3d0f7584ed7d04e27fa050d6683a74746608faf21f202be78460d679cc56461f",
    ];

    for image in images {
        fixture.clear_log();
        let fingerprint = fixture.run_builder_with_inspect_failure(&["fingerprint"], image);
        assert!(!fingerprint.status.success());
        assert!(fingerprint.stdout.is_empty(), "partial fingerprint escaped");
        assert_stderr_contains(&fingerprint, "429 Too Many Requests");

        fixture.clear_log();
        let full = fixture.run_builder_with_inspect_failure(&[], image);
        assert!(!full.status.success());
        assert!(
            full.stdout.is_empty(),
            "failed full build printed a fingerprint"
        );
        let log = fixture.log();
        assert!(!log.contains("dev.ployz.dind.fingerprint="));
        assert!(!log.lines().any(|line| line.starts_with("build --platform")));
    }

    fixture.clear_log();
    let wrapper = fixture.run_dind_with_inspect_failure(
        "scenario_machine_add",
        images[3],
        "linux/amd64 apparently-matching",
    );
    assert!(!wrapper.status.success());
    let log = fixture.log();
    assert!(
        !log.contains("index .Config.Labels \"dev.ployz.dind.fingerprint\""),
        "wrapper must not inspect or trust a cached label after fingerprint failure"
    );
    assert!(!log.contains("--env CARGO_INCREMENTAL=1"));
}

#[test]
fn builder_routes_docker_hub_inputs_through_one_configured_mirror() {
    let fixture = FakeDocker::new("ployz-dind-registry-mirror");
    let output = fixture
        .command("scripts/build-dind-machine-image.sh")
        .env("PLOYZ_DIND_SKIP_BUILD", "1")
        .env("PLOYZ_DIND_DOCKER_HUB_MIRROR", "cache.example:5000")
        .env(
            "PLOYZ_DIND_WORKLOAD_IMAGE",
            "registry-1.docker.io/library/nginx:1.27-alpine",
        )
        .output()
        .expect("DinD builder can use a configured mirror");

    assert_success(&output);
    let log = fixture.log();
    for source in [
        "cache.example:5000/library/nginx:1.27-alpine",
        "cache.example:5000/library/registry:2.8.3",
        "cache.example:5000/library/postgres:15-alpine@sha256:3d0f7584ed7d04e27fa050d6683a74746608faf21f202be78460d679cc56461f",
    ] {
        assert!(log.contains(source), "missing mirrored source {source}");
    }
    assert!(
        log.contains(
            "ghcr.io/umami-software/umami:postgresql-latest@sha256:8edfe4beaef13f9d1300619fa264ef250a3688df9cc54d24ca830ca31cb475ec"
        ),
        "explicit non-Docker-Hub registries stay unchanged"
    );
    assert!(log.contains("--build-arg BASE_IMAGE=cache.example:5000/library/debian:bookworm"));
    assert!(log.contains("--build-arg DOCKER_HUB_MIRROR=cache.example:5000"));
    assert!(
        log.lines().any(|line| line.contains("save -o")
            && line.ends_with(" registry-1.docker.io/library/nginx:1.27-alpine")),
        "machine tarball preserves an explicitly qualified logical Docker Hub reference"
    );
    let dockerfile = fs::read_to_string(repo_path("docker/dind-machine/Dockerfile"))
        .expect("DinD Dockerfile reads");
    assert!(
        dockerfile.contains("DefaultEnvironment=PLOYZ_BUILD_REGISTRY_MIRROR=${DOCKER_HUB_MIRROR}")
    );
}

#[test]
fn builder_rejects_noncanonical_mirror_authorities() {
    let fixture = FakeDocker::new("ployz-dind-invalid-registry-mirror");
    let label = "a".repeat(63);
    let oversized_host = format!("{label}.{label}.{label}.{label}");
    for invalid in [
        oversized_host,
        "cache.example:05000".to_owned(),
        "cache.example:999999999999999999999".to_owned(),
    ] {
        let output = fixture
            .command("scripts/build-dind-machine-image.sh")
            .arg("fingerprint")
            .env("PLOYZ_DIND_DOCKER_HUB_MIRROR", &invalid)
            .output()
            .expect("DinD builder validates a mirror authority");
        assert!(
            !output.status.success(),
            "accepted invalid mirror {invalid:?}"
        );
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn builder_exports_railpack_through_the_root_builder() {
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

struct FakeDocker(PathBuf);

impl FakeDocker {
    fn new(prefix: &str) -> Self {
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
  buildx)
    if [ "${6}" = "${PLOYZ_FAKE_INSPECT_FAILURE:-}" ]; then
      printf '429 Too Many Requests for %s\n' "${6}" >&2
      exit 1
    fi
    printf 'sha256:%s\n' "${6}"
    ;;
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

    fn run_builder_with_inspect_failure(&self, args: &[&str], image: &str) -> Output {
        self.command("scripts/build-dind-machine-image.sh")
            .args(args)
            .env("PLOYZ_DIND_SKIP_BUILD", "1")
            .env("PLOYZ_FAKE_INSPECT_FAILURE", image)
            .output()
            .expect("DinD builder can model manifest failure")
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

    fn run_dind_with_inspect_failure(&self, filter: &str, image: &str, identity: &str) -> Output {
        self.command("scripts/dind-e2e.sh")
            .arg(filter)
            .env("PLOYZ_FAKE_INSPECT_FAILURE", image)
            .env("PLOYZ_FAKE_MACHINE_IDENTITY", identity)
            .env("PLOYZ_DIND_SKIP_BUILD", "1")
            .output()
            .expect("DinD wrapper can model manifest failure")
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

fn write_fake_railpack_download_tools(root: &Path) {
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
  *)
    if [ -x /usr/bin/sha256sum ]; then exec /usr/bin/sha256sum "$@"; fi
    exec /sbin/sha256sum "$@"
    ;;
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

fn seed_workload_tars(fake: &FakeDocker) {
    let stamps = fake.0.join("target/workload-image-stamps");
    fs::create_dir_all(&stamps).expect("stamp dir can be created");
    for (name, image, source) in [
        (
            "nginx",
            "nginx:1.27-alpine",
            "mirror.gcr.io/library/nginx:1.27-alpine",
        ),
        (
            "registry",
            "registry:2.8.3",
            "mirror.gcr.io/library/registry:2.8.3",
        ),
        (
            "umami",
            "ghcr.io/umami-software/umami:postgresql-latest@sha256:8edfe4beaef13f9d1300619fa264ef250a3688df9cc54d24ca830ca31cb475ec",
            "ghcr.io/umami-software/umami:postgresql-latest@sha256:8edfe4beaef13f9d1300619fa264ef250a3688df9cc54d24ca830ca31cb475ec",
        ),
        (
            "postgres",
            "postgres:15-alpine@sha256:3d0f7584ed7d04e27fa050d6683a74746608faf21f202be78460d679cc56461f",
            "mirror.gcr.io/library/postgres:15-alpine@sha256:3d0f7584ed7d04e27fa050d6683a74746608faf21f202be78460d679cc56461f",
        ),
    ] {
        let saved_image = image
            .split_once('@')
            .map_or(image, |(reference, _)| reference);
        fs::write(fake.context().join(format!("{name}.tar")), "tar")
            .expect("workload tar can be written");
        fs::write(
            stamps.join(format!("{name}.stamp")),
            format!("linux/amd64 {image} {source} sha256:{saved_image}\n"),
        )
        .expect("workload stamp can be written");
    }
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn temp_dir(prefix: &str) -> PathBuf {
    let unique = format!(
        "{}-{}",
        prefix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_nanos()
    );
    let path = env::temp_dir().join(unique);
    fs::create_dir_all(&path).expect("temp dir can be created");
    path
}

fn assert_stderr_contains(output: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "stderr should contain {expected:?}, got {stderr:?}"
    );
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
