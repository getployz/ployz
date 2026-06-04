//! Managed `nats-server` process lifecycle.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerConfig {
    pub host: String,
    pub port: u16,
    pub server_name: String,
    pub jetstream_store_dir: PathBuf,
}

impl NatsServerConfig {
    #[must_use]
    pub fn single_node(server_name: impl Into<String>, jetstream_store_dir: PathBuf) -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 4222,
            server_name: server_name.into(),
            jetstream_store_dir,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "server_name: {}\nhost: {}\nport: {}\njetstream {{\n  store_dir: \"{}\"\n}}\n",
            self.server_name,
            self.host,
            self.port,
            self.jetstream_store_dir.display()
        )
    }
}
