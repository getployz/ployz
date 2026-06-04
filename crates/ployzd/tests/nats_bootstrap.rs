use std::path::PathBuf;

use ployzd::nats_process::NatsServerConfig;

#[test]
fn single_node_config_enables_jetstream_on_loopback() {
    let config = NatsServerConfig::single_node("plz-core-1", PathBuf::from("/var/lib/ployz/nats"));
    let rendered = config.render();

    assert!(rendered.contains("server_name: plz-core-1"));
    assert!(rendered.contains("host: 127.0.0.1"));
    assert!(rendered.contains("port: 4222"));
    assert!(rendered.contains("jetstream"));
    assert!(rendered.contains("store_dir: \"/var/lib/ployz/nats\""));
}
