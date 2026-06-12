use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use ployz_core::nats_config::NatsUserSeed;
use ployz_core::security::NatsPrincipal;
use ployz_core::subjects::API_OPS_STATUS;
use ployz_nats::connect::{NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust};
use ployz_test_support::ids::node_id;
use ployzd::app::{ControlWork, DnsWork, GatewayWork, RoleProcessPlan, plan_configured_process};
use ployzd::config::{
    ControlProcessConfig, DaemonProcessConfig, DaemonProcessConfigError, DnsProcessConfig,
    GatewayProcessConfig, NodeDataplaneConfig, NodeProcessArtifacts, NodeProcessConfig,
    PLOYZ_DATAPLANE_BRIDGE_IFNAME_ENV, PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV,
    PLOYZ_DATAPLANE_WG_IFNAME_ENV, PLOYZ_DEPLOY_NODES_ENV, PLOYZ_EBPF_BYTECODE_ENV,
    PLOYZ_EBPF_CTL_ENV, PLOYZ_GATEWAY_LISTEN_ADDR_ENV, PLOYZ_MACHINE_BOOTSTRAP_URL_ENV,
    PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV, PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_NKEY_SEED_FILE_ENV,
    PLOYZ_NATS_URL_ENV, PLOYZ_NODE_ID_ENV, PLOYZ_NODE_PUBLIC_IP_ENV, RoleNatsConnect,
    load_daemon_process_config,
};
use ployzd::nats_process::NatsServerRuntime;
use ployzd::role::{DaemonProcessRole, parse_role_args};

/// Syntactically valid NKey user seed for config tests (not a real key).
const TEST_SEED: &str = "SUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
static TEMP_SEED_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn test_connect_config(url: NatsClientUrl) -> NatsConnectConfig {
    NatsConnectConfig {
        url,
        auth: NatsClientAuth::NkeySeed(
            NatsUserSeed::try_new(TEST_SEED).expect("test seed is valid"),
        ),
        trust: NatsTlsTrust::ClusterCa("/tmp/ployz-test-ca.pem".into()),
        principal: NatsPrincipal::Controller,
    }
}

fn role_connect(url: NatsClientUrl, principal: NatsPrincipal) -> RoleNatsConnect {
    RoleNatsConnect {
        url,
        ca_file: "/tmp/ployz-test-ca.pem".into(),
        seed_file: "/tmp/ployz-test-node.seed".into(),
        principal,
    }
}

fn temp_seed_file(name: &str) -> String {
    let index = TEMP_SEED_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ployzd-role-seed-{}-{index}", std::process::id()));
    fs::create_dir_all(&dir).expect("seed dir can be created");
    let path = dir.join(name);
    fs::write(&path, format!("{TEST_SEED}\n")).expect("seed file can be written");
    path.to_str().expect("temp path is utf-8").to_owned()
}

#[test]
fn control_process_owns_api_and_nats_assurance() {
    let url = NatsClientUrl::loopback(4222);
    let config = DaemonProcessConfig::Control(ControlProcessConfig::new(
        NatsServerRuntime::External(url.clone()),
        node_id("core_1"),
        test_connect_config(url.clone()),
    ));
    let RoleProcessPlan::Control(plan) = plan_configured_process(&config) else {
        panic!("control role should produce a control process plan");
    };

    assert_eq!(config.role(), DaemonProcessRole::Control);
    let DaemonProcessConfig::Control(config) = &config else {
        panic!("control config stays typed");
    };
    assert_eq!(config.deploy_nodes, vec![node_id("core_1")]);
    assert_eq!(plan.nats, NatsServerRuntime::External(url.clone()));
    assert_eq!(plan.nats_url(), url);
    assert_eq!(
        plan.work,
        &[
            ControlWork::AssureNatsResources,
            ControlWork::ServeOperationApi
        ]
    );
    assert_eq!(service_names(&plan.service_catalog), vec!["plz-api"]);
    assert!(plan.service_catalog.has_endpoint_subject(API_OPS_STATUS));
}

#[test]
fn control_role_loads_configured_deploy_nodes() {
    let seed_file = temp_seed_file("controller.seed");
    let config = load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
        PLOYZ_NODE_ID_ENV => Some("core_1".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/tmp/ployz-test-ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some(seed_file.clone()),
        PLOYZ_DEPLOY_NODES_ENV => Some("core_1,edge_2".to_owned()),
        _ => None,
    })
    .expect("control role config loads");

    let DaemonProcessConfig::Control(config) = config else {
        panic!("control role should produce control config");
    };
    assert_eq!(
        config.deploy_nodes,
        vec![node_id("core_1"), node_id("edge_2")]
    );
}

#[test]
fn control_role_rejects_invalid_deploy_node() {
    let seed_file = temp_seed_file("controller.seed");
    let error = load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
        PLOYZ_NODE_ID_ENV => Some("core_1".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/tmp/ployz-test-ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some(seed_file.clone()),
        PLOYZ_DEPLOY_NODES_ENV => Some("core_1,not a node".to_owned()),
        _ => None,
    })
    .expect_err("invalid deploy node is rejected");

    assert!(matches!(
        error,
        DaemonProcessConfigError::InvalidDeployNode { value } if value == "not a node"
    ));
}

#[test]
fn node_process_owns_node_rpc_and_observations_only() {
    let node_id = node_id("node_7");
    let url = NatsClientUrl::loopback(7422);
    let config = DaemonProcessConfig::Node(NodeProcessConfig::new(
        node_id.clone(),
        role_connect(
            url.clone(),
            NatsPrincipal::Node {
                node_id: node_id.clone(),
            },
        ),
        NodeProcessArtifacts::new("/tmp/ployz-ebpf".into(), "/tmp/ployz-ebpf-ctl".into()),
        NodeDataplaneConfig::new(
            "br-ployz".to_owned(),
            "ployz-wg0".to_owned(),
            "10.42.7.0/24".to_owned(),
        ),
        None,
    ));
    let RoleProcessPlan::Node(plan) = plan_configured_process(&config) else {
        panic!("node role should produce a node process plan");
    };

    assert_eq!(config.role(), DaemonProcessRole::Node(node_id.clone()));
    assert_eq!(plan.node_id, node_id);
    assert_eq!(plan.nats_url, url);
    assert_eq!(
        plan.work,
        &[
            ployzd::app::NodeWork::ServeNodeRpc,
            ployzd::app::NodeWork::PublishDockerObservations
        ]
    );
    assert_eq!(service_names(&plan.service_catalog), vec!["plz-node"]);
    assert!(!plan.service_catalog.has_endpoint_subject(API_OPS_STATUS));
}

#[test]
fn gateway_and_dns_are_watchers_not_command_surfaces() {
    let url = NatsClientUrl::loopback(7422);
    let gateway_config = DaemonProcessConfig::Gateway(GatewayProcessConfig::new(
        node_id("node_7"),
        role_connect(
            url.clone(),
            NatsPrincipal::Node {
                node_id: node_id("node_7"),
            },
        ),
        socket(8080),
    ));
    let dns_config = DaemonProcessConfig::Dns(DnsProcessConfig::new(
        node_id("node_7"),
        role_connect(
            url.clone(),
            NatsPrincipal::Node {
                node_id: node_id("node_7"),
            },
        ),
    ));

    let RoleProcessPlan::Gateway(gateway) = plan_configured_process(&gateway_config) else {
        panic!("gateway role should produce a gateway process plan");
    };
    let RoleProcessPlan::Dns(dns) = plan_configured_process(&dns_config) else {
        panic!("dns role should produce a dns process plan");
    };

    assert_eq!(gateway_config.role(), DaemonProcessRole::Gateway);
    assert_eq!(dns_config.role(), DaemonProcessRole::Dns);
    assert_eq!(gateway.node_id, node_id("node_7"));
    assert_eq!(gateway.nats_url, url);
    assert_eq!(gateway.listen_addr, socket(8080));
    assert_eq!(dns.node_id, node_id("node_7"));
    assert_eq!(dns.nats_url, url);
    assert_eq!(
        gateway.work,
        &[
            GatewayWork::WatchRoutes,
            GatewayWork::WatchContainerHealth,
            GatewayWork::ServeLastKnownGoodRoutes
        ]
    );
    assert_eq!(
        dns.work,
        &[
            DnsWork::WatchServices,
            DnsWork::WatchNodeAddresses,
            DnsWork::ServeLastKnownGoodAnswers
        ]
    );
}

#[test]
fn role_parser_accepts_the_supervisor_process_commands() {
    for role in [
        DaemonProcessRole::Control,
        DaemonProcessRole::Node(node_id("node_7")),
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
    let config =
        load_daemon_process_config(
            DaemonProcessRole::Node(node_id("node_7")),
            |name| match name {
                PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
                PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/node.seed".to_owned()),
                PLOYZ_EBPF_BYTECODE_ENV => Some("/tmp/ployz-ebpf".to_owned()),
                PLOYZ_EBPF_CTL_ENV => Some("/tmp/ployz-ebpf-ctl".to_owned()),
                PLOYZ_DATAPLANE_BRIDGE_IFNAME_ENV => Some("br-ployz".to_owned()),
                PLOYZ_DATAPLANE_WG_IFNAME_ENV => Some("wg-ployz".to_owned()),
                PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV => Some("10.77.2.0/24".to_owned()),
                PLOYZ_NODE_PUBLIC_IP_ENV => Some("203.0.113.7".to_owned()),
                _ => None,
            },
        )
        .expect("node role config loads");

    let DaemonProcessConfig::Node(config) = config else {
        panic!("node role should produce node config");
    };
    assert_eq!(config.node_id, node_id("node_7"));
    assert_eq!(config.nats.url, NatsClientUrl::loopback(7422));
    assert_eq!(
        config.nats.ca_file,
        std::path::PathBuf::from("/var/lib/ployz/nats/ca.pem")
    );
    assert_eq!(
        config.nats.seed_file,
        std::path::PathBuf::from("/var/lib/ployz/nats/node.seed")
    );
    assert_eq!(
        config.nats.principal,
        NatsPrincipal::Node {
            node_id: node_id("node_7")
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
    assert_eq!(config.dataplane.bridge_ifname, "br-ployz");
    assert_eq!(config.dataplane.wg_ifname, "wg-ployz");
    assert_eq!(config.dataplane.endpoint_subnet, "10.77.2.0/24");
    assert_eq!(
        config.public_ip,
        Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)))
    );
}

#[test]
fn node_role_derives_endpoint_subnet_from_node_id() {
    let config =
        load_daemon_process_config(
            DaemonProcessRole::Node(node_id("edge_2")),
            |name| match name {
                PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
                PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/node.seed".to_owned()),
                _ => None,
            },
        )
        .expect("node role config loads");

    let DaemonProcessConfig::Node(config) = config else {
        panic!("node role should produce node config");
    };
    assert_eq!(config.dataplane.endpoint_subnet, "10.42.2.0/24");
}

#[test]
fn node_role_rejects_invalid_public_ip() {
    assert!(matches!(
        load_daemon_process_config(
            DaemonProcessRole::Node(node_id("node_7")),
            |name| match name {
                PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
                PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/node.seed".to_owned()),
                PLOYZ_NODE_PUBLIC_IP_ENV => Some("not-an-ip".to_owned()),
                _ => None,
            }
        ),
        Err(DaemonProcessConfigError::InvalidNodePublicIp { .. })
    ));
}

#[test]
fn control_role_loads_optional_machine_bootstrap_url() {
    let seed_file = temp_seed_file("controller.seed");
    let config = load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
        PLOYZ_NODE_ID_ENV => Some("core_a".to_owned()),
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
    assert_eq!(config.deploy_nodes, vec![node_id("core_a")]);
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
        PLOYZ_NODE_ID_ENV => Some("core_a".to_owned()),
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
    let Some(template) = config.machine_bootstrap.join_template else {
        panic!("machine join template should load");
    };
    assert_eq!(template.join_bundle.material.cluster_name.as_str(), "prod");
}

#[test]
fn gateway_role_loads_optional_listen_addr() {
    let config = load_daemon_process_config(DaemonProcessRole::Gateway, |name| match name {
        PLOYZ_NODE_ID_ENV => Some("node_7".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
        PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
        PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/node.seed".to_owned()),
        PLOYZ_GATEWAY_LISTEN_ADDR_ENV => Some("127.0.0.1:18080".to_owned()),
        _ => None,
    })
    .expect("gateway role config loads");

    let DaemonProcessConfig::Gateway(config) = config else {
        panic!("gateway role should produce gateway config");
    };
    assert_eq!(config.node_id, node_id("node_7"));
    assert_eq!(config.nats.url, NatsClientUrl::loopback(4222));
    assert_eq!(
        config.nats.principal,
        NatsPrincipal::Node {
            node_id: node_id("node_7")
        }
    );
    assert_eq!(config.listen_addr, socket(18080));
}

#[test]
fn gateway_role_rejects_invalid_listen_addr() {
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Gateway, |name| match name {
            PLOYZ_NODE_ID_ENV => Some("node_7".to_owned()),
            PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
            PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
            PLOYZ_NATS_NKEY_SEED_FILE_ENV => Some("/var/lib/ployz/nats/node.seed".to_owned()),
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
            PLOYZ_NODE_ID_ENV => Some("core_1".to_owned()),
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
fn binary_node_role_enters_real_runtime_and_fails_when_nats_is_unreachable() {
    let seed_file = temp_seed_file("binary-node.seed");
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["node", "--id", "node_7"])
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
        .env(PLOYZ_NODE_ID_ENV, "node_7")
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
        Err(DaemonProcessConfigError::MissingNodeId {
            role: DaemonProcessRole::Gateway,
        })
    );
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Dns, |name| {
            match name {
                PLOYZ_NODE_ID_ENV => Some("node_7".to_owned()),
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
        load_daemon_process_config(DaemonProcessRole::Node(node_id("node_7")), |name| {
            match name {
                PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                _ => None,
            }
        }),
        Err(DaemonProcessConfigError::MissingNatsCaFile { .. })
    ));
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Node(node_id("node_7")), |name| {
            match name {
                PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
                _ => None,
            }
        }),
        Err(DaemonProcessConfigError::MissingNatsSeedFile { .. })
    ));
}

/// A configured-but-missing seed file is a config error only for control:
/// keeper wrote `controller.seed` at install. Node and gateway carry the
/// path into the typed `AwaitingSeedFile` startup state instead.
#[test]
fn control_role_requires_a_readable_controller_seed_file() {
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
            PLOYZ_NODE_ID_ENV => Some("core_1".to_owned()),
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
    let node =
        load_daemon_process_config(
            DaemonProcessRole::Node(node_id("node_7")),
            |name| match name {
                PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                PLOYZ_NATS_CA_FILE_ENV => Some("/var/lib/ployz/nats/ca.pem".to_owned()),
                PLOYZ_NATS_NKEY_SEED_FILE_ENV => {
                    Some("/tmp/ployz-test-missing-node.seed".to_owned())
                }
                _ => None,
            },
        )
        .expect("node role config loads with a missing seed file");
    let DaemonProcessConfig::Node(node) = node else {
        panic!("node role should produce node config");
    };
    assert!(matches!(
        ployzd::node_credentials::observe_role_credentials(&node.nats, 1),
        ployzd::node_credentials::NodeCredentialState::AwaitingSeedFile { attempts: 1, .. }
    ));
}

#[test]
fn binary_node_role_requires_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["node", "--id", "node_7"])
        .env_remove(PLOYZ_NATS_URL_ENV)
        .output()
        .expect("ployzd binary runs");

    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "PLOYZ_NATS_URL is required for ployzd node\n"
    );
}

fn temp_join_template_file() -> String {
    let dir = std::env::temp_dir().join(format!("ployzd-join-template-{}", std::process::id()));
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

fn service_names(catalog: &ployzd::services::DaemonServiceCatalog) -> Vec<&str> {
    catalog
        .services()
        .iter()
        .map(|service| service.name)
        .collect()
}
