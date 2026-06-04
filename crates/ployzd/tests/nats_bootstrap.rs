use std::path::PathBuf;

use ployz_nats::connect::NatsClientEndpoint;
use ployzd::nats_process::{
    NatsServerConfig, NatsServerConfigError, NatsServerRuntime, PreparedNatsServerService,
};

#[test]
fn single_node_config_enables_jetstream_on_loopback() {
    let config = NatsServerConfig::single_node("plz-core-1", PathBuf::from("/var/lib/ployz/nats"))
        .expect("valid config");
    let rendered = config.render();

    assert!(rendered.contains("server_name: plz-core-1"));
    assert!(rendered.contains("host: 127.0.0.1"));
    assert!(rendered.contains("port: 4222"));
    assert!(rendered.contains("jetstream"));
    assert!(rendered.contains("store_dir: \"/var/lib/ployz/nats\""));
}

#[test]
fn supervised_runtime_uses_prepared_config_endpoint() {
    let config = NatsServerConfig::single_node("plz-core-1", PathBuf::from("/var/lib/ployz/nats"))
        .expect("valid config");
    let service = PreparedNatsServerService::prepare(
        PathBuf::from("/usr/local/bin/nats-server"),
        PathBuf::from("/etc/ployz/nats.conf"),
        config,
    )
    .expect("valid supervised service");
    let runtime = NatsServerRuntime::Supervised(service);

    assert_eq!(
        runtime.client_endpoint(),
        NatsClientEndpoint::loopback(4222)
    );
}

#[test]
fn supervised_server_command_uses_config_file() {
    let service = PreparedNatsServerService::prepare(
        PathBuf::from("/usr/local/bin/nats-server"),
        PathBuf::from("/etc/ployz/nats.conf"),
        NatsServerConfig::single_node("plz-core-1", PathBuf::from("/var/lib/ployz/nats"))
            .expect("valid config"),
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
        NatsServerConfig::single_node("plz-core-1", PathBuf::from("/var/lib/ployz/nats"))
            .expect("valid config"),
    )
    .expect("valid supervised service");

    assert!(
        service
            .rendered_config()
            .contains("server_name: plz-core-1")
    );
    assert_eq!(
        service.client_endpoint(),
        NatsClientEndpoint::loopback(4222)
    );
}

#[test]
fn config_rejects_values_that_change_rendered_shape() {
    assert_eq!(
        NatsServerConfig::single_node(
            "plz-core-1\nport: 9999",
            PathBuf::from("/var/lib/ployz/nats")
        ),
        Err(NatsServerConfigError::InvalidToken {
            field: "server_name",
            value: "plz-core-1\nport: 9999".to_owned(),
        })
    );
}

#[test]
fn config_renderer_escapes_quoted_path_values() {
    let config = NatsServerConfig::single_node(
        "plz-core-1",
        PathBuf::from("/var/lib/ployz/nats \"quoted\" \\ path"),
    )
    .expect("path escapes during render");

    assert!(
        config
            .render()
            .contains("store_dir: \"/var/lib/ployz/nats \\\"quoted\\\" \\\\ path\"")
    );
}

#[test]
fn external_runtime_keeps_the_supplied_endpoint() {
    let endpoint = NatsClientEndpoint::tcp("10.0.0.12", 4222);
    let runtime = NatsServerRuntime::External(endpoint.clone());

    assert_eq!(runtime.client_endpoint(), endpoint);
}
