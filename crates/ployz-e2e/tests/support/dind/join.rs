//! Edge join (`scripts/ployz.sh` flow with the printed install material).

use std::path::PathBuf;

use ployz_core::nats_config::NatsUserSeed;
use ployz_e2e::dind::{ARTIFACTS_MOUNT_PATH, DindMachine, shell_quote, write_file_in_container};
use ployz_sdk_types::MachineAddAccepted;
use ployzctl::commands::machine::MachineAddOutput;

use super::formation::{CoreContext, sha256_of};

/// The env the product prints into the machine-add install command line.
#[derive(Debug)]
pub struct InstallLine {
    pub nats_url: String,
    pub nats_ca_b64: String,
    pub join_seed: String,
    pub join_token: String,
}

/// Renders the product's machine-add output (the same text `ployzctl
/// machine add` prints) and parses the install env + join token out of it.
#[must_use]
pub fn parse_install_line(core: &CoreContext, accepted: MachineAddAccepted) -> InstallLine {
    let join_seed =
        NatsUserSeed::try_new(core.material.join_seed.trim()).expect("valid cluster join seed");
    let rendered = MachineAddOutput::from_accepted(accepted, join_seed).render();
    let Some(install) = rendered
        .lines()
        .find_map(|line| line.strip_prefix("install "))
    else {
        panic!("machine-add output has no install line: {rendered}");
    };
    let Some(token) = rendered
        .lines()
        .find_map(|line| line.strip_prefix("join-token "))
    else {
        panic!("machine-add output has no join-token line: {rendered}");
    };
    InstallLine {
        nats_url: install_line_env(install, "PLOYZ_NATS_URL"),
        nats_ca_b64: install_line_ca_b64(install),
        join_seed: install_line_env(install, "PLOYZ_JOIN_NKEY_SEED"),
        join_token: token.trim().to_owned(),
    }
}

/// Reads one env assignment off the install command line (values may or may
/// not be shell-quoted) — the proof script's `install_line_env`.
fn install_line_env(line: &str, name: &str) -> String {
    let Some((_, rest)) = line.split_once(&format!("{name}=")) else {
        panic!("install line is missing {name}: {line}");
    };
    let value = rest.split_whitespace().next().unwrap_or_default();
    value
        .trim_start_matches('\'')
        .trim_end_matches('\'')
        .to_owned()
}

fn install_line_ca_b64(line: &str) -> String {
    let Some((_, rest)) = line.split_once("printf '%s' ") else {
        panic!("install line is missing CA material: {line}");
    };
    let value = rest.split_whitespace().next().unwrap_or_default();
    value
        .trim_start_matches('\'')
        .trim_end_matches('\'')
        .to_owned()
}

/// Where [`write_installer_wrapper`] leaves the wrapped installer inside a
/// machine; product commands run it via `--installer-script`.
pub const INSTALLER_WRAPPER_PATH: &str = "/tmp/ployz-install.sh";

/// Writes the real `scripts/ployz.sh` into the machine plus a wrapper that
/// pins the keeper artifact to the baked `file://` source, so the product
/// `machine init`/`machine add` commands can run the real installer without
/// fetching a release from the network.
pub async fn write_installer_wrapper(docker: &ployz_e2e::bollard::Docker, machine: &DindMachine) {
    let ployz_sh = std::fs::read_to_string(repo_path("scripts/ployz.sh"))
        .expect("read scripts/ployz.sh from the repo");
    write_file_in_container(
        docker,
        &machine.container_id,
        "/tmp/ployz.sh",
        &ployz_sh,
        "0755",
    )
    .await
    .expect("write ployz.sh into machine");

    let keeper_sha = {
        let outcome = ployz_e2e::dind::exec_in_container(
            docker,
            &machine.container_id,
            &["sha256sum", &format!("{ARTIFACTS_MOUNT_PATH}/ployz-keeper")],
        )
        .await
        .expect("exec sha256sum for keeper");
        assert!(outcome.success(), "keeper sha256sum failed: {outcome:?}");
        let Some(digest) = outcome.stdout.split_whitespace().next() else {
            panic!("empty keeper sha256sum output");
        };
        digest.to_owned()
    };
    let wrapper = format!(
        "#!/bin/sh\nexport PLOYZ_KEEPER_URL=file://{ARTIFACTS_MOUNT_PATH}/ployz-keeper\nexport PLOYZ_KEEPER_SHA256={keeper_sha}\nexec sh /tmp/ployz.sh \"$@\"\n"
    );
    write_file_in_container(
        docker,
        &machine.container_id,
        INSTALLER_WRAPPER_PATH,
        &wrapper,
        "0755",
    )
    .await
    .expect("write installer wrapper into machine");
}

/// Runs the real `scripts/ployz.sh` join flow on the edge machine with
/// exactly the material the product printed.
pub async fn run_edge_join(core: &CoreContext, edge: &DindMachine, install: &InstallLine) {
    let ployz_sh = std::fs::read_to_string(repo_path("scripts/ployz.sh"))
        .expect("read scripts/ployz.sh from the repo");
    write_file_in_container(
        &core.docker,
        &edge.container_id,
        "/tmp/ployz.sh",
        &ployz_sh,
        "0755",
    )
    .await
    .expect("write ployz.sh into edge");

    let keeper_sha = sha256_of(
        &core.docker,
        edge,
        &format!("{ARTIFACTS_MOUNT_PATH}/ployz-keeper"),
    )
    .await;
    let join = core
        .exec_sh(
            edge,
            &format!(
                "ca_file=\"$(mktemp)\"; \
                 trap 'rm -f \"$ca_file\"' EXIT; \
                 printf '%s' {} | base64 -d > \"$ca_file\"; \
                 PLOYZ_KEEPER_URL=file://{ARTIFACTS_MOUNT_PATH}/ployz-keeper \
                 PLOYZ_KEEPER_SHA256={keeper_sha} \
                 sh /tmp/ployz.sh && \
                 PLOYZ_NATS_URL={} PLOYZ_NATS_CA_FILE=\"$ca_file\" PLOYZ_JOIN_NKEY_SEED={} \
                 ployz-keeper bootstrap join --join-token {}",
                shell_quote(&install.nats_ca_b64),
                shell_quote(&install.nats_url),
                shell_quote(&install.join_seed),
                shell_quote(&install.join_token),
            ),
        )
        .await;
    assert!(
        join.success(),
        "edge join failed (exit {}): {}\n{}",
        join.exit_code,
        join.stdout,
        join.stderr
    );
}

fn repo_path(relative: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}
