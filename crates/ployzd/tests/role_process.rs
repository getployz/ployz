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
    GatewayProcessConfig, LoadedDaemonProcessConfig, NodeProcessConfig, PLOYZ_NATS_URL_ENV,
    TunnelProcessConfig, load_daemon_process_config,
};
use ployzd::iroh_tunnel::PreparedTunnelService;
use ployzd::nats_process::NatsServerRuntime;
use ployzd::role::{DaemonProcessRole, TunnelSide, parse_role_args};

#[test]
fn control_process_owns_api_and_nats_assurance() {
    let url = NatsClientUrl::loopback(4222);
    let config = DaemonProcessConfig::Control(ControlProcessConfig::new(
        NatsServerRuntime::External(url.clone()),
    ));
    let RoleProcessPlan::Control(plan) = plan_configured_process(&config) else {
        panic!("control role should produce a control process plan");
    };

    assert_eq!(config.role(), DaemonProcessRole::Control);
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
    let config = DaemonProcessConfig::Node(NodeProcessConfig::new(node_id.clone(), url.clone()));
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
    let gateway_config = DaemonProcessConfig::Gateway(GatewayProcessConfig::new(url.clone()));
    let dns_config = DaemonProcessConfig::Dns(DnsProcessConfig::new(url.clone()));

    let RoleProcessPlan::Gateway(gateway) = plan_configured_process(&gateway_config) else {
        panic!("gateway role should produce a gateway process plan");
    };
    let RoleProcessPlan::Dns(dns) = plan_configured_process(&dns_config) else {
        panic!("dns role should produce a dns process plan");
    };

    assert_eq!(gateway_config.role(), DaemonProcessRole::Gateway);
    assert_eq!(dns_config.role(), DaemonProcessRole::Dns);
    assert_eq!(gateway.nats_url, url);
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
    let LoadedDaemonProcessConfig::Configured(config) =
        load_daemon_process_config(DaemonProcessRole::Node(node_id("node_7")), |name| {
            (name == PLOYZ_NATS_URL_ENV).then(|| "nats://127.0.0.1:7422".to_owned())
        })
        .expect("node role config loads")
    else {
        panic!("node role should be configured");
    };

    let DaemonProcessConfig::Node(config) = config else {
        panic!("node role should produce node config");
    };
    assert_eq!(config.node_id, node_id("node_7"));
    assert_eq!(config.nats_url, NatsClientUrl::loopback(7422));
}

#[test]
fn nats_client_roles_fail_when_nats_url_is_missing_or_invalid() {
    assert_eq!(
        load_daemon_process_config(DaemonProcessRole::Gateway, |_| None),
        Err(DaemonProcessConfigError::MissingNatsUrl {
            role: DaemonProcessRole::Gateway,
        })
    );
    assert!(matches!(
        load_daemon_process_config(DaemonProcessRole::Dns, |name| {
            (name == PLOYZ_NATS_URL_ENV).then(|| "nats://127.0.0.1:4222\nnext".to_owned())
        }),
        Err(DaemonProcessConfigError::InvalidNatsUrl {
            role: DaemonProcessRole::Dns,
            ..
        })
    ));
}

#[test]
fn tunnel_roles_wait_for_transport_join_material() {
    assert_eq!(
        load_daemon_process_config(DaemonProcessRole::Tunnel(TunnelSide::Edge), |_| None),
        Ok(LoadedDaemonProcessConfig::TunnelConfigPending {
            side: TunnelSide::Edge
        })
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

#[test]
fn binary_node_role_accepts_keeper_written_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["node", "--id", "node_7"])
        .env(PLOYZ_NATS_URL_ENV, "nats://127.0.0.1:7422")
        .output()
        .expect("ployzd binary runs");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ployzd role: node\nployzd plan: node\n"
    );
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
