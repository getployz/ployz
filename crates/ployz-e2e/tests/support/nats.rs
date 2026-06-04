use std::error::Error;

use testcontainers_modules::nats::{Nats, NatsServerCmd};
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};

pub struct TestNats {
    _container: ContainerAsync<Nats>,
    url: String,
}

impl TestNats {
    pub async fn start_jetstream() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let command = NatsServerCmd::default().with_jetstream();
        let container = Nats::default().with_cmd(&command).start().await?;
        let host = container.get_host().await?;
        let port = container.get_host_port_ipv4(4222).await?;

        Ok(Self {
            _container: container,
            url: format!("nats://{host}:{port}"),
        })
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}
