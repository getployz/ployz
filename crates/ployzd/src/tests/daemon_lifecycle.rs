use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::config::{
    DEFAULT_GATEWAY_CERTIFICATE_STATE_DIR, DaemonProcessConfigError, DaemonProcessConfigInner,
    PLOYZ_BUILD_REGISTRY_MIRROR_ENV, PLOYZ_DATAPLANE_BRIDGE_IFNAME_ENV,
    PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV, PLOYZ_DATAPLANE_WG_IFNAME_ENV, PLOYZ_DATAPLANE_WG_MTU_ENV,
    PLOYZ_DEPLOY_MACHINES_ENV, PLOYZ_EBPF_BYTECODE_ENV, PLOYZ_EBPF_CTL_ENV,
    PLOYZ_GATEWAY_LISTEN_ADDR_ENV, PLOYZ_JOIN_NKEY_SEED_FILE_ENV, PLOYZ_MACHINE_BOOTSTRAP_URL_ENV,
    PLOYZ_MACHINE_ID_ENV, PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV, PLOYZ_NATS_CA_FILE_ENV,
    PLOYZ_NATS_NKEY_SEED_FILE_ENV, PLOYZ_NATS_URL_ENV, load_daemon_process_config,
};
use crate::role_cli::{DaemonProcessRole, parse_role_args};
use ployz_core::network::MachineEndpointSubnet;
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::NatsClientUrl;
use ployz_test_support::ids::machine_id;

/// Syntactically valid NKey user seed for config tests (not a real key).
const TEST_SEED: &str = "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM";
static TEMP_SEED_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_seed_file(name: &str) -> String {
    let index = TEMP_SEED_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ployzd-role-seed-{}-{index}", std::process::id()));
    fs::create_dir_all(&dir).expect("seed dir can be created");
    let path = dir.join(name);
    fs::write(&path, format!("{TEST_SEED}\n")).expect("seed file can be written");
    path.to_str().expect("temp path is utf-8").to_owned()
}

#[test]
fn control_role_loads_configured_deploy_machines() {
    let seed_file = temp_seed_file("controller.seed");
    let config = load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
        PLOYZ_MACHINE_ID_ENV => Some("core_1".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/tmp/ployz-test-ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some(seed_file.clone()),
        PLOYZ_DEPLOY_MACHINES_ENV => Some("core_1,edge_2".to_owned()),
        _ => None,
    })
    .expect("control role config loads");

    let DaemonProcessConfigInner::Control(config) = config.inner() else {
        panic!("control role should produce control config");
    };
    assert_eq!(
        config.deploy_machines,
        vec![machine_id("core_1"), machine_id("edge_2")]
    );
    let seed = ployz_core::nats_config::NatsUserSeed::try_new(TEST_SEED)
        .expect("valid deterministic controller seed");
    assert_eq!(
        config.environment_revision_key,
        ployz_core::deploy::EnvironmentRevisionKey::derive_from_controller_seed(&seed)
    );
    assert_eq!(
        format!("{:?}", config.environment_revision_key),
        "EnvironmentRevisionKey([redacted])"
    );
}

#[test]
fn control_role_rejects_invalid_deploy_machine() {
    let seed_file = temp_seed_file("controller.seed");
    let error = load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
        PLOYZ_MACHINE_ID_ENV => Some("core_1".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/tmp/ployz-test-ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some(seed_file.clone()),
        PLOYZ_DEPLOY_MACHINES_ENV => Some("core_1,not a machine".to_owned()),
        _ => None,
    })
    .expect_err("invalid deploy machine is rejected");

    assert!(matches!(
        error,
        DaemonProcessConfigError::InvalidDeployMachine { value } if value == "not a machine"
    ));
}

#[test]
fn role_parser_accepts_the_supervisor_process_commands() {
    for role in [
        DaemonProcessRole::Control,
        DaemonProcessRole::Machine(machine_id("machine_7")),
        DaemonProcessRole::Gateway,
        DaemonProcessRole::Dns,
    ] {
        assert_eq!(
            parse_role_args(role.argv()).expect("rendered role argv parses"),
            role
        );
    }
}

#[test]
fn nats_client_roles_load_the_host_runner_written_nats_url() {
    let config = load_daemon_process_config(
        DaemonProcessRole::Machine(machine_id("machine_7")),
        |name| match name {
            PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
            PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
            PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/machine.seed".to_owned()),
            PLOYZ_EBPF_BYTECODE_ENV => Some("/tmp/ployz-ebpf".to_owned()),
            PLOYZ_EBPF_CTL_ENV => Some("/tmp/ployz-ebpf-ctl".to_owned()),
            PLOYZ_DATAPLANE_BRIDGE_IFNAME_ENV => Some("br-ployz".to_owned()),
            PLOYZ_DATAPLANE_WG_IFNAME_ENV => Some("wg-ployz".to_owned()),
            PLOYZ_DATAPLANE_WG_MTU_ENV => Some("1412".to_owned()),
            PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV => Some("10.77.2.0/24".to_owned()),
            PLOYZ_BUILD_REGISTRY_MIRROR_ENV => Some("mirror.gcr.io".to_owned()),
            _ => None,
        },
    )
    .expect("machine role config loads");

    let DaemonProcessConfigInner::Machine(config) = config.inner() else {
        panic!("machine role should produce machine config");
    };
    assert_eq!(config.machine_id, machine_id("machine_7"));
    assert_eq!(config.nats.url, NatsClientUrl::loopback(7422));
    assert_eq!(
        config.nats.ca_file,
        std::path::PathBuf::from("/var/lib/ployz/nats/ca.pem")
    );
    assert_eq!(
        config.nats.seed_file,
        std::path::PathBuf::from("/var/lib/ployz/nats/machine.seed")
    );
    assert_eq!(
        config.nats.principal,
        NatsPrincipal::Machine {
            machine_id: machine_id("machine_7")
        }
    );
    assert_eq!(
        config.artifacts.ebpf_bytecode_path,
        std::path::PathBuf::from("/tmp/ployz-ebpf")
    );
    assert_eq!(
        config.artifacts.ebpf_ctl_path,
        std::path::PathBuf::from("/tmp/ployz-ebpf-ctl")
    );
    assert_eq!(config.ployz_native_mesh.bridge_ifname, "br-ployz");
    assert_eq!(config.ployz_native_mesh.wg_ifname, "wg-ployz");
    assert_eq!(config.ployz_native_mesh.wg_mtu, Some(1412));
    assert_eq!(config.ployz_native_mesh.endpoint_subnet, "10.77.2.0/24");
    assert_eq!(
        config
            .build_registry_mirror
            .as_ref()
            .expect("configured build mirror")
            .as_str(),
        "mirror.gcr.io"
    );
}

#[test]
fn machine_role_rejects_an_unsafe_build_registry_mirror() {
    let error = load_daemon_process_config(
        DaemonProcessRole::Machine(machine_id("machine_7")),
        |name| match name {
            PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
            PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
            PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/machine.seed".to_owned()),
            PLOYZ_BUILD_REGISTRY_MIRROR_ENV => Some("mirror.gcr.io\"]\n[worker.oci]".to_owned()),
            _ => None,
        },
    )
    .expect_err("unsafe mirror is rejected");

    assert!(matches!(
        error,
        DaemonProcessConfigError::InvalidBuildRegistryMirror { value, .. }
            if value.contains("worker.oci")
    ));
}

#[test]
fn machine_role_rejects_invalid_dataplane_wireguard_mtu() {
    let error = load_daemon_process_config(
        DaemonProcessRole::Machine(machine_id("machine_7")),
        |name| match name {
            PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
            PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
            PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/machine.seed".to_owned()),
            PLOYZ_DATAPLANE_WG_MTU_ENV => Some("not-a-number".to_owned()),
            _ => None,
        },
    )
    .expect_err("invalid WireGuard MTU is rejected");

    assert!(matches!(
        error,
        DaemonProcessConfigError::InvalidDataplaneWgMtu { value, .. }
            if value == "not-a-number"
    ));
}

#[test]
fn machine_role_derives_endpoint_subnet_from_machine_id() {
    let config =
        load_daemon_process_config(DaemonProcessRole::Machine(machine_id("edge_2")), |name| {
            match name {
                PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
                PLOYZ_NATS_NKEY_SEED_FILE_ENV => {
                    Some("/var/lib/ployz/nats/machine.seed".to_owned())
                }
                _ => None,
            }
        })
        .expect("machine role config loads");

    let DaemonProcessConfigInner::Machine(config) = config.inner() else {
        panic!("machine role should produce machine config");
    };
    assert_eq!(config.ployz_native_mesh.endpoint_subnet, "10.198.2.0/24");
}

#[test]
fn dns_role_loads_the_machine_endpoint_subnet() {
    let config = load_daemon_process_config(DaemonProcessRole::Dns, |name| match name {
        PLOYZ_MACHINE_ID_ENV => Some("edge_2".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/machine.seed".to_owned()),
        PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV => Some("10.77.2.0/24".to_owned()),
        _ => None,
    })
    .expect("DNS role config loads");

    let DaemonProcessConfigInner::Dns(config) = config.inner() else {
        panic!("DNS role should produce DNS config");
    };
    assert_eq!(
        config.endpoint_subnet,
        MachineEndpointSubnet::try_new("10.77.2.0/24").expect("valid endpoint subnet")
    );
}

#[test]
fn dns_role_rejects_invalid_machine_endpoint_subnet() {
    let result = load_daemon_process_config(DaemonProcessRole::Dns, |name| match name {
        PLOYZ_MACHINE_ID_ENV => Some("edge_2".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/machine.seed".to_owned()),
        PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV => Some("not-a-subnet".to_owned()),
        _ => None,
    });

    assert!(matches!(
        result,
        Err(DaemonProcessConfigError::InvalidDataplaneEndpointSubnet { value, .. })
            if value == "not-a-subnet"
    ));
}

#[test]
fn control_role_loads_optional_machine_bootstrap_url() {
    let seed_file = temp_seed_file("controller.seed");
    let config = load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
        PLOYZ_MACHINE_ID_ENV => Some("core_a".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/tmp/ployz-test-ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some(seed_file.clone()),
        PLOYZ_MACHINE_BOOTSTRAP_URL_ENV => Some("https://example.test/ployz.sh".to_owned()),
        _ => None,
    })
    .expect("control role config loads");

    let DaemonProcessConfigInner::Control(config) = config.inner() else {
        panic!("control role should produce control config");
    };
    assert_eq!(config.deploy_machines, vec![machine_id("core_a")]);
    assert_eq!(
        config.machine_bootstrap.bootstrap_url.as_str(),
        "https://example.test/ployz.sh"
    );
}

#[test]
fn control_role_loads_optional_machine_join_template() {
    let template_path = temp_join_template_file();
    let seed_file = temp_seed_file("controller.seed");
    let config = load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
        PLOYZ_MACHINE_ID_ENV => Some("core_a".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/tmp/ployz-test-ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some(seed_file.clone()),
        PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV => Some(template_path.clone()),
        _ => None,
    })
    .expect("control role config loads");

    let DaemonProcessConfigInner::Control(config) = config.inner() else {
        panic!("control role should produce control config");
    };
    assert!(config.machine_bootstrap.join_material.is_none());
}

#[test]
fn control_role_loads_optional_machine_join_secret_delivery() {
    let template_path = temp_join_template_file();
    let seed_file = temp_seed_file("controller.seed");
    let join_seed_file = temp_seed_file("join.seed");
    let config = load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
        PLOYZ_MACHINE_ID_ENV => Some("core_a".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/tmp/ployz-test-ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some(seed_file.clone()),
        PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV => Some(template_path.clone()),
        PLOYZ_JOIN_NKEY_SEED_FILE_ENV => Some(join_seed_file.clone()),
        _ => None,
    })
    .expect("control role config loads");

    let DaemonProcessConfigInner::Control(config) = config.inner() else {
        panic!("control role should produce control config");
    };
    let Some(ref material) = config.machine_bootstrap.join_material else {
        panic!("machine join material should load");
    };
    assert_eq!(
        material
            .join_template
            .join_bundle
            .material
            .cluster_name
            .as_str(),
        "prod"
    );
    assert_eq!(
        material.join_secret_delivery.nats_credentials.secret(),
        TEST_SEED
    );
}

#[test]
fn gateway_role_loads_optional_listen_addr() {
    let config = load_daemon_process_config(DaemonProcessRole::Gateway, |name| match name {
        PLOYZ_MACHINE_ID_ENV => Some("machine_7".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/machine.seed".to_owned()),
        PLOYZ_GATEWAY_LISTEN_ADDR_ENV => Some("127.0.0.1:18080".to_owned()),
        _ => None,
    })
    .expect("gateway role config loads");

    let DaemonProcessConfigInner::Gateway(config) = config.inner() else {
        panic!("gateway role should produce gateway config");
    };
    assert_eq!(config.machine_id, machine_id("machine_7"));
    assert_eq!(config.nats.url, NatsClientUrl::loopback(4222));
    assert_eq!(
        config.nats.principal,
        NatsPrincipal::Machine {
            machine_id: machine_id("machine_7")
        }
    );
    assert_eq!(config.listen_addr, socket(18080));
    assert_eq!(
        config.certificate_state_dir,
        std::path::PathBuf::from(DEFAULT_GATEWAY_CERTIFICATE_STATE_DIR)
    );
}

#[test]
fn gateway_role_rejects_invalid_listen_addr() {
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Gateway, |name| match name {
            PLOYZ_MACHINE_ID_ENV => Some("machine_7".to_owned()),
            PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
            PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
            PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/machine.seed".to_owned()),
            PLOYZ_GATEWAY_LISTEN_ADDR_ENV => Some("127.0.0.1".to_owned()),
            _ => None,
        }),
        Err(DaemonProcessConfigError::InvalidGatewayListenAddr { .. })
    ));
}

#[test]
fn control_role_rejects_invalid_machine_bootstrap_url() {
    let seed_file = temp_seed_file("controller.seed");
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
            PLOYZ_MACHINE_ID_ENV => Some("core_1".to_owned()),
            PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
            PLOYZ_NATS_CA_FILE_ENV => Some("/tmp/ployz-test-ca.pem".to_owned()),
            PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some(seed_file.clone()),
            PLOYZ_MACHINE_BOOTSTRAP_URL_ENV => Some("http://example.test/ployz.sh".to_owned()),
            _ => None,
        }),
        Err(DaemonProcessConfigError::InvalidMachineBootstrapUrl { .. })
    ));
}

#[test]
fn nats_client_roles_fail_when_nats_url_is_missing_or_invalid() {
    assert_eq!(
        load_daemon_process_config(DaemonProcessRole::Gateway, |_| None),
        Err(DaemonProcessConfigError::MissingMachineId {
            role: DaemonProcessRole::Gateway,
        })
    );
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Dns, |name| {
            match name {
                PLOYZ_MACHINE_ID_ENV => Some("machine_7".to_owned()),
                PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222\nnext".to_owned()),
                _ => None,
            }
        }),
        Err(DaemonProcessConfigError::InvalidNatsUrl {
            role: DaemonProcessRole::Dns,
            ..
        })
    ));
}

#[test]
fn nats_client_roles_require_ca_and_seed_file_envs() {
    assert!(matches!(
        load_daemon_process_config(
            DaemonProcessRole::Machine(machine_id("machine_7")),
            |name| {
                match name {
                    PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                    _ => None,
                }
            }
        ),
        Err(DaemonProcessConfigError::MissingNatsCaFile { .. })
    ));
    assert!(matches!(
        load_daemon_process_config(
            DaemonProcessRole::Machine(machine_id("machine_7")),
            |name| {
                match name {
                    PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                    PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
                    _ => None,
                }
            }
        ),
        Err(DaemonProcessConfigError::MissingNatsSeedFile { .. })
    ));
}

/// A configured-but-missing seed file is a config error only for control:
/// Host Runner wrote `controller.seed` at install. Machine and gateway carry the
/// path into the typed `AwaitingSeedFile` startup state instead.
#[test]
fn control_role_requires_a_readable_controller_seed_file() {
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
            PLOYZ_MACHINE_ID_ENV => Some("core_1".to_owned()),
            PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
            PLOYZ_NATS_CA_FILE_ENV => Some("/tmp/ployz-test-ca.pem".to_owned()),
            PLOYZ_NATS_NKEY_SEED_FILE_ENV =>
                Some("/tmp/ployz-test-missing-controller.seed".to_owned()),
            _ => None,
        }),
        Err(DaemonProcessConfigError::ReadNatsSeedFile {
            role: DaemonProcessRole::Control,
            ..
        })
    ));
    let machine = load_daemon_process_config(
        DaemonProcessRole::Machine(machine_id("machine_7")),
        |name| match name {
            PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
            PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
            PLOYZ_NATS_NKEY_SEED_FILE_ENV => {
                Some("/tmp/ployz-test-missing-machine.seed".to_owned())
            }
            _ => None,
        },
    )
    .expect("machine role config loads with a missing seed file");
    let DaemonProcessConfigInner::Machine(machine) = machine.inner() else {
        panic!("machine role should produce machine config");
    };
    assert!(matches!(
        crate::adapters::credentials::observe_role_credentials(&machine.nats, 1),
        crate::adapters::credentials::MachineCredentialState::AwaitingSeedFile { attempts: 1, .. }
    ));
}

fn temp_join_template_file() -> String {
    let index = TEMP_SEED_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "ployzd-join-template-{}-{index}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("join template dir can be created");
    let path = dir.join("join-template.json");
    fs::write(
        &path,
        r#"{
  "join_bundle": {
    "material": {
      "cluster_name": "prod",
      "runtime_nats_url": "nats://127.0.0.1:7422",
      "trusted_nats": {
        "ca_pem": "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n"
      },
      "recovery_key_wrapped": [1, 2, 3],
      "core_seeds_wrapped": [4, 5, 6],
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
      },
      "railpack": {
        "version": "v0.31.0",
        "source": "/tmp/railpack",
        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "install_path": "/usr/local/lib/ployz/railpack/v0.31.0/railpack"
      }
    }
  }
}
"#,
    )
    .expect("join template can be written");
    path.to_str().expect("temp path is utf-8").to_owned()
}

fn socket(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}
