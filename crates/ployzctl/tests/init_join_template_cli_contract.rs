use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ployz_core::install::MachineJoinTemplate;
use ployzctl::commands::{PloyzctlCommand, parse_command};

const PLOYZ_NEWLINE_SHA256: &str =
    "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e";
const TRUSTED_NATS_CA_PEM: &str =
    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n";

#[test]
fn cli_init_can_render_machine_join_template_json() {
    let temp = temp_dir("ployzctl-join-template");
    let artifact_spec = write_artifact_spec_file(&temp);
    let trusted_nats_ca_file = write_trusted_nats_ca_file(&temp);
    let recovery_key_file = write_recovery_key_file(&temp);
    let core_seeds_file = write_core_seeds_file(&temp);
    let command = parse_command(init_join_template_args(
        &artifact_spec,
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
    let temp = temp_dir("ployzctl-join-template-binary");
    let artifact_spec = write_artifact_spec_file(&temp);
    let trusted_nats_ca_file = write_trusted_nats_ca_file(&temp);
    let recovery_key_file = write_recovery_key_file(&temp);
    let core_seeds_file = write_core_seeds_file(&temp);
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .env_remove("PLOYZ_NATS_URL")
        .args(init_join_template_args(
            &artifact_spec,
            &trusted_nats_ca_file,
            &recovery_key_file,
            &core_seeds_file,
        ))
        .output()
        .expect("ployzctl binary runs");

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
        template.join_bundle.material.ployzd.install_path.as_str(),
        "/usr/local/bin/ployzd"
    );
    assert_eq!(
        template
            .join_bundle
            .material
            .ebpf_bytecode
            .install_path
            .as_str(),
        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
    );
    assert_eq!(
        template.join_bundle.material.ebpf_ctl.install_path.as_str(),
        "/usr/local/bin/ployz-ebpf-ctl"
    );
}

fn init_join_template_args(
    artifact_spec: &Path,
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
        "--artifact-spec",
        artifact_spec
            .to_str()
            .expect("artifact spec fixture path is utf-8"),
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

fn write_artifact_spec_file(dir: &Path) -> PathBuf {
    let path = dir.join("artifact-spec.json");
    fs::write(
        &path,
        format!(
            r#"{{
                "ployzd": {{
                    "version": "acceptance",
                    "source": "/tmp/ployzd",
                    "sha256": "{PLOYZ_NEWLINE_SHA256}",
                    "install_path": "/usr/local/bin/ployzd"
                }},
                "ebpf_bytecode": {{
                    "version": "acceptance",
                    "source": "/tmp/ployz-ebpf-tc",
                    "sha256": "{PLOYZ_NEWLINE_SHA256}",
                    "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
                }},
                "ebpf_ctl": {{
                    "version": "acceptance",
                    "source": "/tmp/ployz-ebpf-ctl",
                    "sha256": "{PLOYZ_NEWLINE_SHA256}",
                    "install_path": "/usr/local/bin/ployz-ebpf-ctl"
                }}
            }}"#
        ),
    )
    .expect("artifact spec fixture can be written");
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
