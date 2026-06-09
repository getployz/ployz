use ployz_core::ids::NodeId;
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
    KeeperFirstNodeInstall, MachineBootstrapUrl, MachineJoinBundle, MachineJoinClusterName,
    MachineJoinCoreIrohEndpoint, MachineJoinIrohPublicKey, MachineJoinIrohTicket,
    MachineJoinMaterial, MachineJoinNatsCredentials, MachineJoinPloyzdArtifact,
    MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTrustedNats,
    MachineJoinTrustedNatsServerId,
};
use ployz_core::roles::FirstNodeGateway;

#[test]
fn keeper_first_node_install_renders_shell_command() {
    let install = keeper_install(FirstNodeGateway::Install);

    assert_eq!(
        install.render_command(),
        "ployz-keeper first-node-install --node 'node_1' --ployzd-version '0.1.0' --ployzd-source '/tmp/ployzd' --ployzd-sha256 '0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e' --ployzd-install-path '/usr/local/bin/ployzd' --nats-version '2.12.0' --nats-source '/tmp/nats-server' --nats-sha256 '0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e' --nats-binary '/usr/local/bin/nats-server' --nats-config '/etc/nats/nats-server.conf' --gateway"
    );
}

#[test]
fn keeper_first_node_install_omits_gateway_when_skipped() {
    let install = keeper_install(FirstNodeGateway::Skip);

    assert!(!install.render_command().contains(" --gateway"));
}

#[test]
fn keeper_first_node_install_can_carry_machine_bootstrap_url() {
    let mut install = keeper_install(FirstNodeGateway::Skip);
    install.machine_bootstrap_url = Some(
        MachineBootstrapUrl::try_new("https://example.test/ployz.sh").expect("valid bootstrap url"),
    );

    assert!(
        install
            .render_command()
            .contains("--machine-bootstrap-url 'https://example.test/ployz.sh'")
    );
}

#[test]
fn keeper_first_node_install_can_carry_machine_join_template_file() {
    let mut install = keeper_install(FirstNodeGateway::Skip);
    install.machine_join_template_file = Some(
        AbsoluteInstallPath::try_new("/etc/ployz/machine-join-template.json")
            .expect("valid template file path"),
    );

    assert!(
        install
            .render_command()
            .contains("--machine-join-template-file '/etc/ployz/machine-join-template.json'")
    );
}

#[test]
fn keeper_install_contract_validates_artifact_inputs() {
    assert!(MachineJoinClusterName::try_new("").is_err());
    assert!(MachineJoinClusterName::try_new("prod\nother").is_err());
    assert!(MachineJoinClusterName::try_new("prod=west").is_err());
    assert!(MachineBootstrapUrl::try_new("").is_err());
    assert!(MachineBootstrapUrl::try_new("http://example.test/ployz.sh").is_err());
    assert!(MachineBootstrapUrl::try_new("https://example.test/ployz.sh").is_ok());
    assert!(MachineJoinRuntimeNatsUrl::try_new("").is_err());
    assert!(MachineJoinRuntimeNatsUrl::try_new("http://127.0.0.1:7422").is_err());
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422\n").is_err());
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422").is_ok());
    assert!(MachineJoinNatsCredentials::try_new("").is_err());
    assert!(MachineJoinNatsCredentials::try_new("creds\0bad").is_err());
    assert!(MachineJoinNatsCredentials::try_new("user-jwt-and-seed").is_ok());
    assert!(MachineJoinTrustedNatsServerId::try_new("").is_err());
    assert!(MachineJoinTrustedNatsServerId::try_new("server one").is_err());
    assert!(MachineJoinTrustedNatsServerId::try_new("server_1").is_ok());
    assert!(MachineJoinIrohPublicKey::try_new("").is_err());
    assert!(MachineJoinIrohPublicKey::try_new("key one").is_err());
    assert!(MachineJoinIrohPublicKey::try_new("core-public-key").is_ok());
    assert!(MachineJoinIrohTicket::try_new("").is_err());
    assert!(MachineJoinIrohTicket::try_new("ticket one").is_err());
    assert!(MachineJoinIrohTicket::try_new("iroh-ticket").is_ok());
    assert!(InstallArtifactVersion::try_new("").is_err());
    assert!(InstallArtifactSource::try_new("").is_err());
    assert!(InstallArtifactSource::try_new("relative/ployzd").is_err());
    assert!(InstallArtifactSource::try_new("/tmp/ployzd").is_ok());
    assert!(InstallArtifactSource::try_new("https://example.test/ployzd").is_ok());
    assert!(InstallSha256Digest::try_new("").is_err());
    assert!(InstallSha256Digest::try_new("not-a-digest").is_err());
    assert!(AbsoluteInstallPath::try_new("").is_err());
    assert!(AbsoluteInstallPath::try_new("relative/path").is_err());
    assert!(AbsoluteInstallPath::try_new("/").is_err());
    assert!(AbsoluteInstallPath::try_new("/usr/local/bin/").is_err());
    assert!(AbsoluteInstallPath::try_new("/usr/local/bin/ployzd").is_ok());
}

#[test]
fn machine_join_bundle_rejects_invalid_wire_artifact_before_storage() {
    let value = serde_json::json!({
        "material": {
            "cluster_name": "prod",
            "runtime_nats_url": "nats://127.0.0.1:7422",
            "trusted_nats": {
                "server_id": "server_1",
                "config_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            },
            "core_iroh": {
                "public_key": "core-public-key"
            },
            "ployzd": {
                "version": "0.1.0",
                "source": "relative/ployzd",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "install_path": "/usr/local/bin/ployzd"
            }
        }
    });

    assert!(serde_json::from_value::<MachineJoinBundle>(value).is_err());
}

#[test]
fn machine_join_bundle_wire_shape_stays_plain_json() {
    let value = serde_json::to_value(machine_join_bundle()).expect("bundle serializes");

    assert_eq!(
        value,
        serde_json::json!({
            "material": {
                "cluster_name": "prod",
                "runtime_nats_url": "nats://127.0.0.1:7422",
                "trusted_nats": {
                    "server_id": "server_1",
                    "config_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                },
                "core_iroh": {
                    "public_key": "core-public-key"
                },
                "ployzd": {
                    "version": "0.1.0",
                    "source": "/tmp/ployzd",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "install_path": "/usr/local/bin/ployzd"
                }
            }
        })
    );
}

#[test]
fn machine_join_bundle_debug_redacts_secrets() {
    let rendered = format!("{:?}", machine_join_secret_delivery());

    assert!(!rendered.contains("user-jwt-and-seed"));
    assert!(!rendered.contains("core-ticket"));
}

fn keeper_install(gateway: FirstNodeGateway) -> KeeperFirstNodeInstall {
    KeeperFirstNodeInstall {
        node_id: NodeId::try_new("node_1").expect("valid node id"),
        gateway,
        machine_bootstrap_url: None,
        machine_join_template_file: None,
        ployzd_version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
        ployzd_source: InstallArtifactSource::try_new("/tmp/ployzd").expect("valid source"),
        ployzd_sha256: InstallSha256Digest::try_new(
            "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
        )
        .expect("valid digest"),
        ployzd_install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployzd")
            .expect("valid install path"),
        nats_version: InstallArtifactVersion::try_new("2.12.0").expect("valid nats version"),
        nats_source: InstallArtifactSource::try_new("/tmp/nats-server").expect("valid nats source"),
        nats_sha256: InstallSha256Digest::try_new(
            "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
        )
        .expect("valid nats digest"),
        nats_binary: AbsoluteInstallPath::try_new("/usr/local/bin/nats-server")
            .expect("valid nats binary path"),
        nats_config: AbsoluteInstallPath::try_new("/etc/nats/nats-server.conf")
            .expect("valid nats config path"),
    }
}

fn machine_join_bundle() -> MachineJoinBundle {
    MachineJoinBundle {
        material: MachineJoinMaterial {
            cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                .expect("valid runtime nats url"),
            trusted_nats: MachineJoinTrustedNats {
                server_id: MachineJoinTrustedNatsServerId::try_new("server_1")
                    .expect("valid nats server id"),
                config_sha256: InstallSha256Digest::try_new(
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                )
                .expect("valid nats config digest"),
            },
            core_iroh: MachineJoinCoreIrohEndpoint {
                public_key: MachineJoinIrohPublicKey::try_new("core-public-key")
                    .expect("valid core iroh public key"),
            },
            ployzd: MachineJoinPloyzdArtifact {
                version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                source: InstallArtifactSource::try_new("/tmp/ployzd").expect("valid source"),
                sha256: InstallSha256Digest::try_new(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("valid digest"),
                install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployzd")
                    .expect("valid install path"),
            },
        },
    }
}

fn machine_join_secret_delivery() -> MachineJoinSecretDelivery {
    MachineJoinSecretDelivery {
        nats_credentials: MachineJoinNatsCredentials::try_new("user-jwt-and-seed")
            .expect("valid nats credentials"),
        core_iroh_ticket: MachineJoinIrohTicket::try_new("core-ticket")
            .expect("valid core iroh ticket"),
    }
}
