use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ployz::commands::{PloyzctlCommand, parse_command};
use ployz_core::install::MachineJoinTemplate;

const TRUSTED_NATS_CA_PEM: &str =
    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n";

#[test]
fn cli_init_can_render_machine_join_template_json() {
    let temp = temp_dir("ployz-join-template");
    let trusted_nats_ca_file = write_trusted_nats_ca_file(&temp);
    let recovery_key_file = write_recovery_key_file(&temp);
    let core_seeds_file = write_core_seeds_file(&temp);
    let command = parse_command(init_join_template_args(
        &trusted_nats_ca_file,
        &recovery_key_file,
        &core_seeds_file,
    ))
    .expect("join template command parses");

    let PloyzctlCommand::InitJoinTemplate(command) = command else {
        panic!("expected join template command");
    };
    let template: MachineJoinTemplate =
        serde_json::from_str(&command.render_json()).expect("join template renders valid json");

    assert_join_template(template);
}

#[test]
fn binary_init_can_print_machine_join_template_without_nats() {
    let temp = temp_dir("ployz-join-template-binary");
    let trusted_nats_ca_file = write_trusted_nats_ca_file(&temp);
    let recovery_key_file = write_recovery_key_file(&temp);
    let core_seeds_file = write_core_seeds_file(&temp);
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env("DO_NOT_TRACK", "1")
        .env_remove("PLOYZ_NATS_URL")
        .args(init_join_template_args(
            &trusted_nats_ca_file,
            &recovery_key_file,
            &core_seeds_file,
        ))
        .output()
        .expect("ployz binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let template: MachineJoinTemplate =
        serde_json::from_str(&stdout(&output)).expect("join template renders valid json");
    assert_join_template(template);
    assert_eq!(stderr(&output), "");
}

fn assert_join_template(template: MachineJoinTemplate) {
    assert_eq!(
        template.join_bundle.material.cluster_name.as_str(),
        "acceptance-smoke"
    );
    assert_eq!(
        template.join_bundle.material.runtime_nats_url.as_str(),
        "nats://127.0.0.1:7422"
    );
    assert_eq!(
        template.join_bundle.material.trusted_nats.ca_pem.as_str(),
        TRUSTED_NATS_CA_PEM
    );
    assert_eq!(
        template
            .join_bundle
            .material
            .substrate_release
            .version
            .as_str(),
        "0.0.2-alpha.87"
    );
}

fn init_join_template_args(
    trusted_nats_ca_file: &Path,
    recovery_key_file: &Path,
    core_seeds_file: &Path,
) -> Vec<String> {
    [
        "internal",
        "init",
        "join-template",
        "--cluster",
        "acceptance-smoke",
        "--runtime-nats-url",
        "nats://127.0.0.1:7422",
        "--trusted-nats-ca-file",
        trusted_nats_ca_file
            .to_str()
            .expect("trusted CA fixture path is utf-8"),
        "--recovery-key-file",
        recovery_key_file
            .to_str()
            .expect("recovery key fixture path is utf-8"),
        "--core-seeds-file",
        core_seeds_file
            .to_str()
            .expect("core seeds fixture path is utf-8"),
        "--release-version",
        "v0.0.2-alpha.87",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn write_recovery_key_file(dir: &Path) -> PathBuf {
    let path = dir.join("ca-recovery.key");
    fs::write(&path, b"wrapped-ca-key").expect("recovery key fixture can be written");
    path
}

fn write_core_seeds_file(dir: &Path) -> PathBuf {
    let path = dir.join("core-seeds.key");
    fs::write(&path, b"wrapped-core-seeds").expect("core seeds fixture can be written");
    path
}

fn write_trusted_nats_ca_file(dir: &Path) -> PathBuf {
    let path = dir.join("trusted-nats-ca.pem");
    fs::write(&path, TRUSTED_NATS_CA_PEM).expect("trusted CA fixture can be written");
    path
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn temp_dir(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{}-{}-{unique}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir can be created");
    dir
}
