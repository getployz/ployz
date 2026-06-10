use ployz_core::ids::NodeId;
use ployz_core::install::{
    AbsoluteInstallPath, FirstNodeInstallSpec, InstallArtifactSource, InstallArtifactVersion,
    InstallSha256Digest, KeeperFirstNodeInstall, MachineBootstrapUrl, MachineJoinArtifact,
    MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial, MachineJoinNatsCredentials,
    MachineJoinPloyzdArtifact, MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery,
    MachineJoinTrustedNats,
};
use ployz_core::nats_config::{NatsCaCertificatePem, NatsServerName};
use ployz_core::roles::FirstNodeGateway;

#[test]
fn first_node_install_spec_wire_shape_is_grouped_json() {
    let mut install = keeper_install(FirstNodeGateway::Install);
    install.machine_bootstrap_url = Some(
        MachineBootstrapUrl::try_new("https://example.test/ployz.sh").expect("valid bootstrap url"),
    );
    install.machine_join_template_file = Some(
        AbsoluteInstallPath::try_new("/etc/ployz/machine-join-template.json")
            .expect("valid template file path"),
    );
    install.node_public_ip = Some("203.0.113.10".parse().expect("valid IP"));

    let value = serde_json::to_value(FirstNodeInstallSpec::from(install)).expect("spec serializes");

    assert_eq!(
        value,
        serde_json::json!({
            "node_id": "node_1",
            "gateway": "install",
            "node_public_ip": "203.0.113.10",
            "machine_bootstrap_url": "https://example.test/ployz.sh",
            "machine_join_template_file": "/etc/ployz/machine-join-template.json",
            "artifacts": {
                "ployzd": {
                    "version": "0.1.0",
                    "source": "/tmp/ployzd",
                    "sha256": "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
                    "install_path": "/usr/local/bin/ployzd"
                },
                "ebpf_bytecode": {
                    "version": "0.1.0",
                    "source": "/tmp/ployz-ebpf-tc",
                    "sha256": "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
                    "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
                },
                "ebpf_ctl": {
                    "version": "0.1.0",
                    "source": "/tmp/ployz-ebpf-ctl",
                    "sha256": "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
                    "install_path": "/usr/local/bin/ployz-ebpf-ctl"
                },
                "nats_server": {
                    "version": "2.12.0",
                    "source": "/tmp/nats-server",
                    "sha256": "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
                    "binary": "/usr/local/bin/nats-server",
                    "config": "/etc/nats/nats-server.conf"
                }
            }
        })
    );
}

#[test]
fn first_node_install_spec_converts_to_install_contract() {
    let spec = serde_json::from_value::<FirstNodeInstallSpec>(serde_json::json!({
        "node_id": "node_1",
        "gateway": "skip",
        "node_public_ip": null,
        "machine_bootstrap_url": null,
        "machine_join_template_file": null,
        "artifacts": {
            "ployzd": {
                "version": "0.1.0",
                "source": "/tmp/ployzd",
                "sha256": "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
                "install_path": "/usr/local/bin/ployzd"
            },
            "ebpf_bytecode": {
                "version": "0.1.0",
                "source": "/tmp/ployz-ebpf-tc",
                "sha256": "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
                "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
            },
            "ebpf_ctl": {
                "version": "0.1.0",
                "source": "/tmp/ployz-ebpf-ctl",
                "sha256": "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
                "install_path": "/usr/local/bin/ployz-ebpf-ctl"
            },
            "nats_server": {
                "version": "2.12.0",
                "source": "/tmp/nats-server",
                "sha256": "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
                "binary": "/usr/local/bin/nats-server",
                "config": "/etc/nats/nats-server.conf"
            }
        }
    }))
    .expect("spec parses");

    assert_eq!(
        KeeperFirstNodeInstall::from(spec),
        keeper_install(FirstNodeGateway::Skip)
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
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1").is_err());
    assert!(MachineJoinRuntimeNatsUrl::try_new("tls://core.example.test").is_err());
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://core_1:7422").is_err());
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://-bad.example.test:7422").is_err());
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://[::1:7422").is_err());
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:0").is_err());
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:99999").is_err());
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://localhost:7422").is_ok());
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422").is_ok());
    assert!(MachineJoinRuntimeNatsUrl::try_new("nats://[::1]:7422").is_ok());
    assert!(MachineJoinRuntimeNatsUrl::try_new("tls://core.example.test:4222").is_ok());
    assert!(MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222").is_ok());
    assert!(MachineJoinNatsCredentials::try_new("").is_err());
    assert!(MachineJoinNatsCredentials::try_new("creds\0bad").is_err());
    assert!(MachineJoinNatsCredentials::try_new("user-jwt-and-seed").is_ok());
    assert!(NatsServerName::try_new("").is_err());
    assert!(NatsServerName::try_new("server one").is_err());
    assert!(NatsServerName::try_new("server_1").is_ok());
    assert!(NatsCaCertificatePem::try_new("").is_err());
    assert!(NatsCaCertificatePem::try_new("not-a-pem").is_err());
    assert!(NatsCaCertificatePem::try_new("-----BEGIN CERTIFICATE-----\nTUlJQg==").is_err());
    assert!(
        NatsCaCertificatePem::try_new(
            "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n"
        )
        .is_ok()
    );
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
                "server_name": "server_1",
                "ca_pem": "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n"
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
                    "server_name": "server_1",
                    "ca_pem": "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n"
                },
                "ployzd": {
                    "version": "0.1.0",
                    "source": "/tmp/ployzd",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "install_path": "/usr/local/bin/ployzd"
                },
                "ebpf_bytecode": {
                    "version": "0.1.0",
                    "source": "/tmp/ployz-ebpf-tc",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
                },
                "ebpf_ctl": {
                    "version": "0.1.0",
                    "source": "/tmp/ployz-ebpf-ctl",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "install_path": "/usr/local/bin/ployz-ebpf-ctl"
                }
            }
        })
    );
}

#[test]
fn machine_join_bundle_debug_redacts_secrets() {
    let rendered = format!("{:?}", machine_join_secret_delivery());

    assert!(!rendered.contains("user-jwt-and-seed"));
}

fn keeper_install(gateway: FirstNodeGateway) -> KeeperFirstNodeInstall {
    KeeperFirstNodeInstall {
        node_id: NodeId::try_new("node_1").expect("valid node id"),
        gateway,
        node_public_ip: None,
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
        ebpf_bytecode_version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
        ebpf_bytecode_source: InstallArtifactSource::try_new("/tmp/ployz-ebpf-tc")
            .expect("valid source"),
        ebpf_bytecode_sha256: InstallSha256Digest::try_new(
            "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
        )
        .expect("valid digest"),
        ebpf_bytecode_install_path: AbsoluteInstallPath::try_new(
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
        )
        .expect("valid install path"),
        ebpf_ctl_version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
        ebpf_ctl_source: InstallArtifactSource::try_new("/tmp/ployz-ebpf-ctl")
            .expect("valid source"),
        ebpf_ctl_sha256: InstallSha256Digest::try_new(
            "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
        )
        .expect("valid digest"),
        ebpf_ctl_install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployz-ebpf-ctl")
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
                server_name: NatsServerName::try_new("server_1").expect("valid nats server name"),
                ca_pem: NatsCaCertificatePem::try_new(
                    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                )
                .expect("valid ca pem"),
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
            ebpf_bytecode: MachineJoinArtifact {
                version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                source: InstallArtifactSource::try_new("/tmp/ployz-ebpf-tc").expect("valid source"),
                sha256: InstallSha256Digest::try_new(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("valid digest"),
                install_path: AbsoluteInstallPath::try_new(
                    "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                )
                .expect("valid install path"),
            },
            ebpf_ctl: MachineJoinArtifact {
                version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                source: InstallArtifactSource::try_new("/tmp/ployz-ebpf-ctl")
                    .expect("valid source"),
                sha256: InstallSha256Digest::try_new(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("valid digest"),
                install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployz-ebpf-ctl")
                    .expect("valid install path"),
            },
        },
    }
}

fn machine_join_secret_delivery() -> MachineJoinSecretDelivery {
    MachineJoinSecretDelivery {
        nats_credentials: MachineJoinNatsCredentials::try_new("user-jwt-and-seed")
            .expect("valid nats credentials"),
    }
}
