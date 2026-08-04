use std::path::PathBuf;
use std::process::{Command, Output};

const PLATFORM: &str = "linux/amd64";
const WORK_DIR: &str = "/tmp/ployz-corrosion-probe-test";
const RUNTIME_IMAGE: &str = "mirror.gcr.io/library/debian:trixie";
const EMBEDDED_VERSION: &str = "corrosion 0.2.0-beta.0";

#[test]
fn corrosion_version_probe_accepts_the_exact_embedded_version() {
    let output = run_corrosion_version_probe(EMBEDDED_VERSION, EMBEDDED_VERSION);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        docker_arguments(&output),
        [
            "run",
            "--rm",
            "--platform",
            PLATFORM,
            "--volume",
            &format!("{WORK_DIR}:/corrosion-input:ro"),
            RUNTIME_IMAGE,
            "/corrosion-input/corrosion",
            "--version",
        ]
    );
}

#[test]
fn corrosion_version_probe_rejects_a_mismatched_embedded_version() {
    let actual_version = "corrosion unexpected-version";
    let output = run_corrosion_version_probe(actual_version, EMBEDDED_VERSION);

    assert!(
        !output.status.success(),
        "mismatched version must fail closed"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&format!(
            "Corrosion archive reports {actual_version}, expected {EMBEDDED_VERSION}"
        )),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_corrosion_version_probe(actual_version: &str, expected_version: &str) -> Output {
    Command::new("bash")
        .arg("-c")
        .arg(
            r#"set -euo pipefail
source "$1"
reported_version="$6"
docker() {
  local argument
  for argument in "$@"; do
    printf 'docker-arg:%s\n' "${argument}" >&2
  done
  printf '%s\n' "${reported_version}"
}
verify_corrosion_embedded_version "$2" "$3" "$4" "$5"
"#,
        )
        .arg("corrosion-version-probe-test")
        .arg(repo_path("scripts/lib.sh"))
        .arg(PLATFORM)
        .arg(WORK_DIR)
        .arg(RUNTIME_IMAGE)
        .arg(expected_version)
        .arg(actual_version)
        .output()
        .expect("Corrosion version probe can run")
}

fn docker_arguments(output: &Output) -> Vec<&str> {
    std::str::from_utf8(&output.stderr)
        .expect("probe stderr is utf8")
        .lines()
        .filter_map(|line| line.strip_prefix("docker-arg:"))
        .collect()
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}
