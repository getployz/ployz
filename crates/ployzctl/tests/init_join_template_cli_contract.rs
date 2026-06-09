use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ployz_core::install::MachineJoinTemplate;
use ployzctl::commands::{PloyzctlCommand, parse_command};

const PLOYZ_NEWLINE_SHA256: &str =
    "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e";
const FIRST_NODE_NATS_CONFIG_SHA256: &str =
    "5be25a6dfbc6a4b45598f1d128dd2230e5109575018b8826e36d2883102f6ec2";

#[test]
fn cli_init_can_render_machine_join_template_json() {
    let temp = temp_dir("ployzctl-join-template");
    let secret_delivery_file = write_secret_delivery_file(&temp);
    let command = parse_command(init_join_template_args(&secret_delivery_file))
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
    let secret_delivery_file = write_secret_delivery_file(&temp);
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .env_remove("PLOYZ_NATS_URL")
        .args(init_join_template_args(&secret_delivery_file))
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
        template
            .join_bundle
            .material
            .trusted_nats
            .server_id
            .as_str(),
        "core_1"
    );
    assert_eq!(
        template
            .join_bundle
            .material
            .trusted_nats
            .config_sha256
            .as_str(),
        FIRST_NODE_NATS_CONFIG_SHA256
    );
    assert_eq!(
        template.join_bundle.material.core_iroh.node_id.as_str(),
        "core_1"
    );
    assert_eq!(
        template.join_bundle.material.core_iroh.public_key.as_str(),
        "acceptance-core"
    );
    assert_eq!(
        template
            .join_bundle
            .material
            .core_iroh
            .direct_addresses
            .iter()
            .map(|address| address.as_str())
            .collect::<Vec<_>>(),
        vec!["203.0.113.10:4433"]
    );
    assert_eq!(
        template
            .join_bundle
            .material
            .core_iroh
            .relay_url
            .as_ref()
            .expect("relay url is configured")
            .as_str(),
        "https://relay.example.test"
    );
    assert_eq!(
        template.join_bundle.material.ployzd.install_path.as_str(),
        "/usr/local/bin/ployzd"
    );
    assert_eq!(
        template.secret_delivery.nats_credentials.secret(),
        "acceptance-node-creds"
    );
    assert_eq!(
        template.secret_delivery.core_iroh_ticket.secret(),
        "acceptance-core-ticket"
    );
}

fn init_join_template_args(secret_delivery_file: &Path) -> Vec<String> {
    [
        "init",
        "join-template",
        "--cluster",
        "acceptance-smoke",
        "--runtime-nats-url",
        "nats://127.0.0.1:7422",
        "--trusted-first-node",
        "core_1",
        "--core-iroh-public-key",
        "acceptance-core",
        "--core-iroh-direct-address",
        "203.0.113.10:4433",
        "--core-iroh-relay-url",
        "https://relay.example.test",
        "--ployzd-version",
        "acceptance",
        "--ployzd-source",
        "/tmp/ployzd",
        "--ployzd-sha256",
        PLOYZ_NEWLINE_SHA256,
        "--ployzd-install-path",
        "/usr/local/bin/ployzd",
        "--secret-delivery-file",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain([secret_delivery_file
        .to_str()
        .expect("secret delivery fixture path is utf-8")
        .to_owned()])
    .collect()
}

fn write_secret_delivery_file(dir: &Path) -> PathBuf {
    let path = dir.join("secret-delivery.json");
    fs::write(
        &path,
        r#"{"nats_credentials":"acceptance-node-creds","core_iroh_ticket":"acceptance-core-ticket"}"#,
    )
    .expect("secret delivery fixture can be written");
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
