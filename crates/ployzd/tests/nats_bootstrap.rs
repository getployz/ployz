use std::path::PathBuf;

use ployz_core::ids::NodeId;
use ployz_core::nats_config::{NatsListener, NatsServerTlsFiles};
use ployz_nats::connect::{NatsClientEndpoint, NatsClientUrl};
use ployzd::nats_process::{NatsServerConfig, NatsServerRuntime, PreparedNatsServerService};

#[test]
fn single_node_config_enables_tls_jetstream_on_loopback() {
    let rendered = single_node_config(PathBuf::from("/var/lib/ployz/nats")).render();

    assert!(rendered.contains("server_name: core-1"));
    assert!(rendered.contains("host: 127.0.0.1"));
    assert!(rendered.contains("port: 4222"));
    assert!(rendered.contains("tls {"));
    assert!(rendered.contains("cert_file: \"/var/lib/ployz/nats/server.crt\""));
    assert!(rendered.contains("key_file: \"/var/lib/ployz/nats/server.key\""));
    assert!(rendered.contains("jetstream"));
    assert!(rendered.contains("store_dir: \"/var/lib/ployz/nats\""));
    assert!(rendered.contains("include \"authorized-users.conf\""));
}

#[test]
fn supervised_runtime_uses_prepared_config_endpoint() {
    let config = single_node_config(PathBuf::from("/var/lib/ployz/nats"));
    let service = PreparedNatsServerService::prepare(
        PathBuf::from("/usr/local/bin/nats-server"),
        PathBuf::from("/etc/ployz/nats.conf"),
        config,
    )
    .expect("valid supervised service");
    let runtime = NatsServerRuntime::Supervised(service);

    assert_eq!(runtime.client_url(), NatsClientUrl::loopback(4222));
}

#[test]
fn supervised_server_command_uses_config_file() {
    let service = PreparedNatsServerService::prepare(
        PathBuf::from("/usr/local/bin/nats-server"),
        PathBuf::from("/etc/ployz/nats.conf"),
        single_node_config(PathBuf::from("/var/lib/ployz/nats")),
    )
    .expect("valid supervised service");
    let command = service.command();

    assert_eq!(command.get_program(), "/usr/local/bin/nats-server");
    assert_eq!(
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
        vec!["--config", "/etc/ployz/nats.conf"]
    );
}

#[test]
fn prepared_server_keeps_rendered_config_with_endpoint() {
    let service = PreparedNatsServerService::prepare(
        PathBuf::from("/usr/local/bin/nats-server"),
        PathBuf::from("/etc/ployz/nats.conf"),
        single_node_config(PathBuf::from("/var/lib/ployz/nats")),
    )
    .expect("valid supervised service");

    assert!(service.rendered_config().contains("server_name: core-1"));
    assert_eq!(
        service.client_endpoint(),
        NatsClientEndpoint::loopback(4222)
    );
}

#[test]
fn config_renderer_escapes_quoted_path_values() {
    let config = single_node_config(PathBuf::from("/var/lib/ployz/nats \"quoted\" \\ path"));

    assert!(
        config
            .render()
            .contains("store_dir: \"/var/lib/ployz/nats \\\"quoted\\\" \\\\ path\"")
    );
}

#[test]
fn external_runtime_keeps_the_supplied_endpoint() {
    let url = NatsClientUrl::try_new("nats://10.0.0.12:4222").expect("valid NATS URL");
    let runtime = NatsServerRuntime::External(url.clone());

    assert_eq!(runtime.client_url(), url);
}

fn single_node_config(store_dir: PathBuf) -> NatsServerConfig {
    NatsServerConfig::single_node(
        node_id("core-1"),
        store_dir,
        NatsListener::Loopback,
        NatsServerTlsFiles {
            cert_file: PathBuf::from("/var/lib/ployz/nats/server.crt"),
            key_file: PathBuf::from("/var/lib/ployz/nats/server.key"),
        },
        PathBuf::from("authorized-users.conf"),
    )
    .expect("valid config")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}
