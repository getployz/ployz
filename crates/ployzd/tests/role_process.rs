use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::NatsClientUrl;
use ployz_test_support::ids::machine_id;
use ployzd::config::{
    DaemonProcessConfig, DaemonProcessConfigError, PLOYZ_DATAPLANE_BRIDGE_IFNAME_ENV,
    PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV, PLOYZ_DATAPLANE_WG_IFNAME_ENV, PLOYZ_DEPLOY_MACHINES_ENV,
    PLOYZ_EBPF_BYTECODE_ENV, PLOYZ_EBPF_CTL_ENV, PLOYZ_GATEWAY_LISTEN_ADDR_ENV,
    PLOYZ_JOIN_NKEY_SEED_FILE_ENV, PLOYZ_MACHINE_BOOTSTRAP_URL_ENV, PLOYZ_MACHINE_ID_ENV,
    PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV, PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_NKEY_SEED_FILE_ENV,
    PLOYZ_NATS_URL_ENV, load_daemon_process_config,
};
use ployzd::role_cli::{DaemonProcessRole, parse_role_args};

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

    let DaemonProcessConfig::Control(config) = config else {
        panic!("control role should produce control config");
    };
    assert_eq!(
        config.deploy_machines,
        vec![machine_id("core_1"), machine_id("edge_2")]
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
fn nats_client_roles_load_the_keeper_written_nats_url() {
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
            PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV => Some("10.77.2.0/24".to_owned()),
            _ => None,
        },
    )
    .expect("machine role config loads");

    let DaemonProcessConfig::Machine(config) = config else {
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
    assert_eq!(config.ployz_native_mesh.endpoint_subnet, "10.77.2.0/24");
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

    let DaemonProcessConfig::Machine(config) = config else {
        panic!("machine role should produce machine config");
    };
    assert_eq!(config.ployz_native_mesh.endpoint_subnet, "10.42.2.0/24");
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

    let DaemonProcessConfig::Control(config) = config else {
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

    let DaemonProcessConfig::Control(config) = config else {
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

    let DaemonProcessConfig::Control(config) = config else {
        panic!("control role should produce control config");
    };
    let Some(material) = config.machine_bootstrap.join_material else {
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

    let DaemonProcessConfig::Gateway(config) = config else {
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
fn binary_machine_role_enters_real_runtime_and_fails_when_nats_is_unreachable() {
    let seed_file = temp_seed_file("binary-machine.seed");
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["machine", "--id", "machine_7"])
        .env(PLOYZ_NATS_URL_ENV, "nats://127.0.0.1:7422")
        .env(PLOYZ_NATS_CA_FILE_ENV, "/tmp/ployz-test-ca.pem")
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, &seed_file)
        .output()
        .expect("ployzd binary runs");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("failed to connect to NATS"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn binary_gateway_role_enters_real_runtime_and_fails_when_nats_is_unreachable() {
    let seed_file = temp_seed_file("binary-gateway.seed");
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["gateway"])
        .env(PLOYZ_MACHINE_ID_ENV, "machine_7")
        .env(PLOYZ_NATS_URL_ENV, "nats://127.0.0.1:7422")
        .env(PLOYZ_NATS_CA_FILE_ENV, "/tmp/ployz-test-ca.pem")
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, &seed_file)
        .output()
        .expect("ployzd binary runs");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("failed to connect to NATS"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn binary_dns_role_enters_real_runtime_and_fails_when_nats_is_unreachable() {
    let seed_file = temp_seed_file("binary-dns.seed");
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["dns"])
        .env(PLOYZ_MACHINE_ID_ENV, "machine_7")
        .env(PLOYZ_NATS_URL_ENV, "nats://127.0.0.1:7422")
        .env(PLOYZ_NATS_CA_FILE_ENV, "/tmp/ployz-test-ca.pem")
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, &seed_file)
        .output()
        .expect("ployzd binary runs");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("failed to connect to NATS"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
/// keeper wrote `controller.seed` at install. Machine and gateway carry the
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
    let DaemonProcessConfig::Machine(machine) = machine else {
        panic!("machine role should produce machine config");
    };
    assert!(matches!(
        ployzd::adapters::credentials::observe_role_credentials(&machine.nats, 1),
        ployzd::adapters::credentials::MachineCredentialState::AwaitingSeedFile { attempts: 1, .. }
    ));
}

#[test]
fn binary_machine_role_requires_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["machine", "--id", "machine_7"])
        .env_remove(PLOYZ_NATS_URL_ENV)
        .output()
        .expect("ployzd binary runs");

    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "PLOYZ_NATS_URL is required for ployzd machine\n"
    );
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
