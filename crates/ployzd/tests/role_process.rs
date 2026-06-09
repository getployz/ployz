use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::Command;

use ployz_core::ids::NodeId;
use ployz_core::subjects::API_OPS_STATUS;
use ployz_nats::connect::NatsClientUrl;
use ployz_transport::iroh_endpoint::IrohEndpoint;
use ployzd::app::{
    ControlWork, DnsWork, GatewayWork, RoleProcessPlan, TunnelWork, plan_configured_process,
};
use ployzd::config::{
    ControlProcessConfig, DaemonProcessConfig, DaemonProcessConfigError, DnsProcessConfig,
    GatewayProcessConfig, NodeProcessConfig, PLOYZ_EBPF_BYTECODE_ENV, PLOYZ_EBPF_CTL_ENV,
    PLOYZ_GATEWAY_LISTEN_ADDR_ENV, PLOYZ_MACHINE_BOOTSTRAP_URL_ENV,
    PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV, PLOYZ_NATS_URL_ENV, PLOYZ_NODE_ID_ENV,
    PLOYZ_NODE_PUBLIC_IP_ENV, PLOYZ_TUNNEL_CORE_DIRECT_ADDRS_ENV, PLOYZ_TUNNEL_CORE_NODE_ENV,
    PLOYZ_TUNNEL_CORE_PUBLIC_KEY_ENV, PLOYZ_TUNNEL_CORE_RELAY_URL_ENV,
    PLOYZ_TUNNEL_LISTEN_ADDR_ENV, PLOYZ_TUNNEL_NATS_ADDR_ENV, TunnelProcessConfig,
    load_daemon_process_config,
};
use ployzd::iroh_tunnel::PreparedTunnelService;
use ployzd::nats_process::NatsServerRuntime;
use ployzd::role::{DaemonProcessRole, TunnelSide, parse_role_args};

#[test]
fn control_process_owns_api_and_nats_assurance() {
    let url = NatsClientUrl::loopback(4222);
    let config = DaemonProcessConfig::Control(ControlProcessConfig::new(
        NatsServerRuntime::External(url.clone()),
        node_id("core_1"),
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
fn node_process_owns_node_rpc_and_observations_only() {
    let node_id = node_id("node_7");
    let url = NatsClientUrl::loopback(7422);
    let config = DaemonProcessConfig::Node(NodeProcessConfig::new(
        node_id.clone(),
        url.clone(),
        "/tmp/ployz-ebpf".into(),
        "/tmp/ployz-ebpf-ctl".into(),
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
        url.clone(),
        socket(8080),
    ));
    let dns_config =
        DaemonProcessConfig::Dns(DnsProcessConfig::new(node_id("node_7"), url.clone()));

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
fn tunnel_side_decides_byte_transport_work() {
    let edge_service = PreparedTunnelService::edge(
        "ployzd-tunnel-edge",
        socket(7422),
        IrohEndpoint::new(node_id("core_1"), "core-public-key"),
    );
    let core_service = PreparedTunnelService::core("ployzd-tunnel-core", socket(4222));
    let edge_config = DaemonProcessConfig::Tunnel(TunnelProcessConfig::new(edge_service.clone()));
    let core_config = DaemonProcessConfig::Tunnel(TunnelProcessConfig::new(core_service.clone()));

    let RoleProcessPlan::Tunnel(edge) = plan_configured_process(&edge_config) else {
        panic!("edge tunnel role should produce a tunnel process plan");
    };
    let RoleProcessPlan::Tunnel(core) = plan_configured_process(&core_config) else {
        panic!("core tunnel role should produce a tunnel process plan");
    };

    assert_eq!(
        edge_config.role(),
        DaemonProcessRole::Tunnel(TunnelSide::Edge)
    );
    assert_eq!(
        core_config.role(),
        DaemonProcessRole::Tunnel(TunnelSide::Core)
    );
    assert_eq!(edge.service, edge_service);
    assert_eq!(core.service, core_service);
    assert_eq!(edge.side(), TunnelSide::Edge);
    assert_eq!(core.side(), TunnelSide::Core);
    assert_eq!(
        edge.work(),
        &[TunnelWork::ExposeLoopbackNats, TunnelWork::OpenIrohToCore]
    );
    assert_eq!(
        core.work(),
        &[
            TunnelWork::AcceptIrohFromEdges,
            TunnelWork::ForwardCoreNatsBytes
        ]
    );
}

#[test]
fn role_parser_accepts_the_supervisor_process_commands() {
    assert_eq!(
        parse_role_args(["control"].map(str::to_owned)),
        Ok(DaemonProcessRole::Control)
    );
    assert_eq!(
        parse_role_args(["node", "--id", "node_7"].map(str::to_owned)),
        Ok(DaemonProcessRole::Node(node_id("node_7")))
    );
    assert_eq!(
        parse_role_args(["gateway"].map(str::to_owned)),
        Ok(DaemonProcessRole::Gateway)
    );
    assert_eq!(
        parse_role_args(["dns"].map(str::to_owned)),
        Ok(DaemonProcessRole::Dns)
    );
    assert_eq!(
        parse_role_args(["tunnel", "--side", "edge"].map(str::to_owned)),
        Ok(DaemonProcessRole::Tunnel(TunnelSide::Edge))
    );
}

#[test]
fn nats_client_roles_load_the_keeper_written_nats_url() {
    let config =
        load_daemon_process_config(
            DaemonProcessRole::Node(node_id("node_7")),
            |name| match name {
                PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                PLOYZ_EBPF_BYTECODE_ENV => Some("/tmp/ployz-ebpf".to_owned()),
                PLOYZ_EBPF_CTL_ENV => Some("/tmp/ployz-ebpf-ctl".to_owned()),
                PLOYZ_NODE_PUBLIC_IP_ENV => Some("203.0.113.7".to_owned()),
                _ => None,
            },
        )
        .expect("node role config loads");

    let DaemonProcessConfig::Node(config) = config else {
        panic!("node role should produce node config");
    };
    assert_eq!(config.node_id, node_id("node_7"));
    assert_eq!(config.nats_url, NatsClientUrl::loopback(7422));
    assert_eq!(
        config.ebpf_bytecode_path,
        std::path::PathBuf::from("/tmp/ployz-ebpf")
    );
    assert_eq!(
        config.ebpf_ctl_path,
        std::path::PathBuf::from("/tmp/ployz-ebpf-ctl")
    );
    assert_eq!(
        config.public_ip,
        Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)))
    );
}

#[test]
fn node_role_rejects_invalid_public_ip() {
    assert!(matches!(
        load_daemon_process_config(
            DaemonProcessRole::Node(node_id("node_7")),
            |name| match name {
                PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:7422".to_owned()),
                PLOYZ_NODE_PUBLIC_IP_ENV => Some("not-an-ip".to_owned()),
                _ => None,
            }
        ),
        Err(DaemonProcessConfigError::InvalidNodePublicIp { .. })
    ));
}

#[test]
fn control_role_loads_optional_machine_bootstrap_url() {
    let config = load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
        PLOYZ_MACHINE_BOOTSTRAP_URL_ENV => Some("https://example.test/ployz.sh".to_owned()),
        _ => None,
    })
    .expect("control role config loads");

    let DaemonProcessConfig::Control(config) = config else {
        panic!("control role should produce control config");
    };
    assert_eq!(
        config.machine_bootstrap.bootstrap_url.as_str(),
        "https://example.test/ployz.sh"
    );
}

#[test]
fn control_role_loads_optional_machine_join_template() {
    let template_path = temp_join_template_file();
    let config = load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
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
    assert_eq!(
        template.secret_delivery.nats_credentials.secret(),
        "user-jwt-and-seed"
    );
}

#[test]
fn gateway_role_loads_optional_listen_addr() {
    let config = load_daemon_process_config(DaemonProcessRole::Gateway, |name| match name {
        PLOYZ_NODE_ID_ENV => Some("node_7".to_owned()),
        PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
        PLOYZ_GATEWAY_LISTEN_ADDR_ENV => Some("127.0.0.1:18080".to_owned()),
        _ => None,
    })
    .expect("gateway role config loads");

    let DaemonProcessConfig::Gateway(config) = config else {
        panic!("gateway role should produce gateway config");
    };
    assert_eq!(config.node_id, node_id("node_7"));
    assert_eq!(config.nats_url, NatsClientUrl::loopback(4222));
    assert_eq!(config.listen_addr, socket(18080));
}

#[test]
fn gateway_role_rejects_invalid_listen_addr() {
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Gateway, |name| match name {
            PLOYZ_NODE_ID_ENV => Some("node_7".to_owned()),
            PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
            PLOYZ_GATEWAY_LISTEN_ADDR_ENV => Some("127.0.0.1".to_owned()),
            _ => None,
        }),
        Err(DaemonProcessConfigError::InvalidGatewayListenAddr { .. })
    ));
}

#[test]
fn control_role_rejects_invalid_machine_bootstrap_url() {
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Control, |name| match name {
            PLOYZ_NATS_URL_ENV => Some("nats://127.0.0.1:4222".to_owned()),
            PLOYZ_MACHINE_BOOTSTRAP_URL_ENV => Some("http://example.test/ployz.sh".to_owned()),
            _ => None,
        }),
        Err(DaemonProcessConfigError::InvalidMachineBootstrapUrl { .. })
    ));
}

#[test]
fn binary_node_role_enters_real_runtime_and_fails_when_nats_is_unreachable() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["node", "--id", "node_7"])
        .env(PLOYZ_NATS_URL_ENV, "nats://127.0.0.1:7422")
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
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["gateway"])
        .env(PLOYZ_NODE_ID_ENV, "node_7")
        .env(PLOYZ_NATS_URL_ENV, "nats://127.0.0.1:7422")
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
fn tunnel_roles_require_explicit_transport_config() {
    assert_eq!(
        load_daemon_process_config(DaemonProcessRole::Tunnel(TunnelSide::Edge), |_| None),
        Err(DaemonProcessConfigError::MissingTunnelConfig {
            side: TunnelSide::Edge,
            env: PLOYZ_TUNNEL_LISTEN_ADDR_ENV,
        })
    );
    assert_eq!(
        load_daemon_process_config(DaemonProcessRole::Tunnel(TunnelSide::Core), |_| None),
        Err(DaemonProcessConfigError::MissingTunnelConfig {
            side: TunnelSide::Core,
            env: PLOYZ_TUNNEL_NATS_ADDR_ENV,
        })
    );
}

#[test]
fn tunnel_roles_load_core_and_edge_config() {
    let config =
        load_daemon_process_config(
            DaemonProcessRole::Tunnel(TunnelSide::Core),
            |name| match name {
                PLOYZ_TUNNEL_NATS_ADDR_ENV => Some("127.0.0.1:4222".to_owned()),
                _ => None,
            },
        )
        .expect("core tunnel config loads");
    let DaemonProcessConfig::Tunnel(config) = config else {
        panic!("core tunnel role should produce tunnel config");
    };
    assert_eq!(config.side(), TunnelSide::Core);
    assert_eq!(config.service.service_name, "ployzd-tunnel-core");
    assert_eq!(
        config.service.local_client_endpoint().socket_addr(),
        Some(socket(4222))
    );

    let config =
        load_daemon_process_config(
            DaemonProcessRole::Tunnel(TunnelSide::Edge),
            |name| match name {
                PLOYZ_TUNNEL_LISTEN_ADDR_ENV => Some("127.0.0.1:7422".to_owned()),
                PLOYZ_TUNNEL_CORE_NODE_ENV => Some("core_1".to_owned()),
                PLOYZ_TUNNEL_CORE_PUBLIC_KEY_ENV => Some("core-public-key".to_owned()),
                PLOYZ_TUNNEL_CORE_DIRECT_ADDRS_ENV => {
                    Some("203.0.113.10:4433,203.0.113.11:4433".to_owned())
                }
                PLOYZ_TUNNEL_CORE_RELAY_URL_ENV => Some("https://relay.example.test".to_owned()),
                _ => None,
            },
        )
        .expect("edge tunnel config loads");
    let DaemonProcessConfig::Tunnel(config) = config else {
        panic!("edge tunnel role should produce tunnel config");
    };
    assert_eq!(config.side(), TunnelSide::Edge);
    assert_eq!(config.service.service_name, "ployzd-tunnel-edge");
    assert_eq!(
        config.service.local_client_endpoint().socket_addr(),
        Some(socket(7422))
    );
    assert_eq!(
        config.service.tunnel,
        ployz_transport::nats_tunnel::NatsTunnelConfig::Edge(
            ployz_transport::nats_tunnel::EdgeNatsTunnelConfig::new(
                socket(7422),
                IrohEndpoint::new(node_id("core_1"), "core-public-key")
                    .with_direct_address("203.0.113.10:4433".parse().expect("valid address"))
                    .with_direct_address("203.0.113.11:4433".parse().expect("valid address"))
                    .with_relay_url("https://relay.example.test")
            )
        )
    );
}

#[test]
fn edge_tunnel_rejects_non_loopback_listener() {
    assert_eq!(
        load_daemon_process_config(
            DaemonProcessRole::Tunnel(TunnelSide::Edge),
            |name| match name {
                PLOYZ_TUNNEL_LISTEN_ADDR_ENV => Some("0.0.0.0:7422".to_owned()),
                PLOYZ_TUNNEL_CORE_NODE_ENV => Some("core_1".to_owned()),
                PLOYZ_TUNNEL_CORE_PUBLIC_KEY_ENV => Some("core-public-key".to_owned()),
                _ => None,
            }
        ),
        Err(DaemonProcessConfigError::NonLoopbackTunnelListenAddr {
            side: TunnelSide::Edge,
            value: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 7422),
        })
    );
}

#[test]
fn edge_tunnel_rejects_invalid_core_dial_hints() {
    assert!(matches!(
        load_daemon_process_config(
            DaemonProcessRole::Tunnel(TunnelSide::Edge),
            |name| match name {
                PLOYZ_TUNNEL_LISTEN_ADDR_ENV => Some("127.0.0.1:7422".to_owned()),
                PLOYZ_TUNNEL_CORE_NODE_ENV => Some("core_1".to_owned()),
                PLOYZ_TUNNEL_CORE_PUBLIC_KEY_ENV => Some("core-public-key".to_owned()),
                PLOYZ_TUNNEL_CORE_DIRECT_ADDRS_ENV => Some("not-a-socket".to_owned()),
                _ => None,
            }
        ),
        Err(DaemonProcessConfigError::InvalidTunnelDirectAddress { value, .. })
            if value == "not-a-socket"
    ));
    assert_eq!(
        load_daemon_process_config(
            DaemonProcessRole::Tunnel(TunnelSide::Edge),
            |name| match name {
                PLOYZ_TUNNEL_LISTEN_ADDR_ENV => Some("127.0.0.1:7422".to_owned()),
                PLOYZ_TUNNEL_CORE_NODE_ENV => Some("core_1".to_owned()),
                PLOYZ_TUNNEL_CORE_PUBLIC_KEY_ENV => Some("core-public-key".to_owned()),
                PLOYZ_TUNNEL_CORE_RELAY_URL_ENV => Some("http://relay.example.test".to_owned()),
                _ => None,
            }
        ),
        Err(DaemonProcessConfigError::InvalidTunnelRelayUrl {
            value: "http://relay.example.test".to_owned(),
        })
    );
}

#[test]
fn binary_tunnel_role_enters_runtime_when_configured() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["tunnel", "--side", "edge"])
        .env(PLOYZ_TUNNEL_LISTEN_ADDR_ENV, "127.0.0.1:7422")
        .env(PLOYZ_TUNNEL_CORE_NODE_ENV, "core_1")
        .env(PLOYZ_TUNNEL_CORE_PUBLIC_KEY_ENV, "core-public-key")
        .output()
        .expect("ployzd binary runs");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("core iroh public key \"core-public-key\" is invalid"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
        "server_id": "server_1",
        "config_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      },
      "core_iroh": {
        "node_id": "core_1",
        "public_key": "core-public-key",
        "direct_addresses": [],
        "relay_url": null
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
  },
  "secret_delivery": {
    "nats_credentials": "user-jwt-and-seed",
    "core_iroh_ticket": "core-ticket"
  }
}
"#,
    )
    .expect("join template can be written");
    path.to_str().expect("temp path is utf-8").to_owned()
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
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
