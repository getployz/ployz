use ployz_core::ids::MachineId;
use ployz_core::install::{
    AbsoluteInstallPath, FirstMachineInstallArtifacts, FirstMachineInstallSpec,
    InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion, InstallSha256Digest,
    MachineBootstrapUrl, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
    MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTrustedNats,
    NatsServerInstallSpec,
};
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::roles::{DnsRole, GatewayRole, InstallRolePolicy};

#[test]
fn first_machine_install_spec_wire_shape_is_grouped_json() {
    let mut install = first_machine_install_spec(GatewayRole::Install, DnsRole::Install);
    install.machine_bootstrap_url = Some(
        MachineBootstrapUrl::try_new("https://example.test/ployz.sh").expect("valid bootstrap url"),
    );
    install.machine_join_template_file = Some(
        AbsoluteInstallPath::try_new("/etc/ployz/machine-join-template.json")
            .expect("valid template file path"),
    );
    install.machine_public_ip = Some("203.0.113.10".parse().expect("valid IP"));

    let value = serde_json::to_value(install).expect("spec serializes");

    assert_eq!(
        value,
        serde_json::json!({
            "machine_id": "machine_1",
            "gateway": "install",
            "dns": "install",
            "machine_public_ip": "203.0.113.10",
            "machine_bootstrap_url": "https://example.test/ployz.sh",
            "machine_join_template_file": "/etc/ployz/machine-join-template.json",
            "machine_join_cluster_name": "ployz",
            "machine_join_runtime_nats_url": "tls://203.0.113.10:4222",
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
fn first_machine_install_spec_parses_from_grouped_json() {
    let spec = serde_json::from_value::<FirstMachineInstallSpec>(serde_json::json!({
        "machine_id": "machine_1",
        "gateway": "skip",
        "dns": "install",
        "machine_public_ip": null,
        "machine_bootstrap_url": null,
        "machine_join_template_file": null,
        "machine_join_cluster_name": "ployz",
        "machine_join_runtime_nats_url": "tls://203.0.113.10:4222",
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
        spec,
        first_machine_install_spec(GatewayRole::Skip, DnsRole::Install)
    );
    assert_eq!(
        spec.role_policy(),
        InstallRolePolicy::install_all().without_gateway()
    );
}

#[test]
fn first_machine_install_spec_parses_explicit_dns_opt_out() {
    let with_dns_skip = serde_json::to_value(first_machine_install_spec(
        GatewayRole::Install,
        DnsRole::Skip,
    ))
    .expect("spec serializes");
    let spec =
        serde_json::from_value::<FirstMachineInstallSpec>(with_dns_skip).expect("spec parses back");

    assert_eq!(
        spec.role_policy(),
        InstallRolePolicy::install_all().without_dns()
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
    assert!(NatsUserSeed::try_new("").is_err());
    assert!(NatsUserSeed::try_new("creds\0bad").is_err());
    assert!(
        NatsUserSeed::try_new("SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM").is_ok()
    );
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

    assert!(!rendered.contains("SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM"));
}

fn first_machine_install_spec(gateway: GatewayRole, dns: DnsRole) -> FirstMachineInstallSpec {
    FirstMachineInstallSpec {
        machine_id: MachineId::try_new("machine_1").expect("valid machine id"),
        gateway,
        dns,
        machine_public_ip: None,
        machine_bootstrap_url: None,
        machine_join_template_file: None,
        machine_join_cluster_name: MachineJoinClusterName::try_new("ployz")
            .expect("valid cluster name"),
        machine_join_runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(
            "tls://203.0.113.10:4222",
        )
        .expect("valid runtime nats url"),
        artifacts: FirstMachineInstallArtifacts {
            ployzd: install_artifact("/tmp/ployzd", "/usr/local/bin/ployzd"),
            ebpf_bytecode: install_artifact(
                "/tmp/ployz-ebpf-tc",
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            ),
            ebpf_ctl: install_artifact("/tmp/ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
            nats_server: NatsServerInstallSpec {
                version: InstallArtifactVersion::try_new("2.12.0").expect("valid nats version"),
                source: InstallArtifactSource::try_new("/tmp/nats-server")
                    .expect("valid nats source"),
                sha256: InstallSha256Digest::try_new(
                    "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
                )
                .expect("valid nats digest"),
                binary: AbsoluteInstallPath::try_new("/usr/local/bin/nats-server")
                    .expect("valid nats binary path"),
                config: AbsoluteInstallPath::try_new("/etc/nats/nats-server.conf")
                    .expect("valid nats config path"),
            },
        },
    }
}

fn install_artifact(source: &str, install_path: &str) -> InstallArtifactSpec {
    InstallArtifactSpec {
        version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
        source: InstallArtifactSource::try_new(source).expect("valid source"),
        sha256: InstallSha256Digest::try_new(
            "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e",
        )
        .expect("valid digest"),
        install_path: AbsoluteInstallPath::try_new(install_path).expect("valid install path"),
    }
}

fn machine_join_bundle() -> MachineJoinBundle {
    MachineJoinBundle {
        material: MachineJoinMaterial {
            cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                .expect("valid runtime nats url"),
            trusted_nats: MachineJoinTrustedNats {
                ca_pem: NatsCaCertificatePem::try_new(
                    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                )
                .expect("valid ca pem"),
            },
            recovery_key_wrapped: None,
            ployzd: join_artifact("/tmp/ployzd", "/usr/local/bin/ployzd"),
            ebpf_bytecode: join_artifact(
                "/tmp/ployz-ebpf-tc",
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            ),
            ebpf_ctl: join_artifact("/tmp/ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
        },
    }
}

fn join_artifact(source: &str, install_path: &str) -> InstallArtifactSpec {
    InstallArtifactSpec {
        version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
        source: InstallArtifactSource::try_new(source).expect("valid source"),
        sha256: InstallSha256Digest::try_new(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("valid digest"),
        install_path: AbsoluteInstallPath::try_new(install_path).expect("valid install path"),
    }
}

fn machine_join_secret_delivery() -> MachineJoinSecretDelivery {
    MachineJoinSecretDelivery {
        nats_credentials: NatsUserSeed::try_new(
            "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM",
        )
        .expect("valid nats credentials"),
    }
}
