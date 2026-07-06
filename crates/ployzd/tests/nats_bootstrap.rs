use std::path::PathBuf;

use ployz_core::nats_config::{NatsListener, NatsServerTlsFiles};
use ployz_nats::connect::{NatsClientEndpoint, NatsClientUrl};
use ployz_test_support::ids::machine_id;
use ployzd::adapters::nats_server::{NatsServerConfig, NatsServerRuntime, PreparedNatsServerService};

#[test]
fn single_machine_config_enables_tls_and_disables_jetstream_on_loopback() {
    let rendered = single_machine_config().render();

    assert!(rendered.contains("server_name: core-1"));
    assert!(rendered.contains("host: 127.0.0.1"));
    assert!(rendered.contains("port: 4222"));
    assert!(rendered.contains("tls {"));
    assert!(rendered.contains("cert_file: \"/var/lib/ployz/nats/server.crt\""));
    assert!(rendered.contains("key_file: \"/var/lib/ployz/nats/server.key\""));
    assert!(rendered.contains("jetstream: disabled"));
    assert!(rendered.contains("include \"authorized-users.conf\""));
}

#[test]
fn supervised_runtime_uses_prepared_config_endpoint() {
    let config = single_machine_config();
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
        single_machine_config(),
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
        single_machine_config(),
    )
    .expect("valid supervised service");

    assert!(service.rendered_config().contains("server_name: core-1"));
    assert_eq!(
        service.client_endpoint(),
        NatsClientEndpoint::loopback(4222)
    );
}

#[test]
fn external_runtime_keeps_the_supplied_endpoint() {
    let url = NatsClientUrl::try_new("nats://10.0.0.12:4222").expect("valid NATS URL");
    let runtime = NatsServerRuntime::External(url.clone());

    assert_eq!(runtime.client_url(), url);
}

fn single_machine_config() -> NatsServerConfig {
    NatsServerConfig::single_machine(
        machine_id("core-1"),
        NatsListener::Loopback,
        NatsServerTlsFiles {
            cert_file: PathBuf::from("/var/lib/ployz/nats/server.crt"),
            key_file: PathBuf::from("/var/lib/ployz/nats/server.key"),
        },
        PathBuf::from("authorized-users.conf"),
    )
    .expect("valid config")
}
