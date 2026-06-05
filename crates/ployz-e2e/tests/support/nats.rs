use std::error::Error;
use std::io;
use std::time::Duration;

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
        let url = format!("nats://{host}:{port}");
        wait_for_nats(&url).await?;

        Ok(Self {
            _container: container,
            url,
        })
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

async fn wait_for_nats(url: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut last_error = "no connection attempt made".to_owned();
    for _ in 0..50 {
        match async_nats::connect(url).await {
            Ok(client) => {
                drop(client);
                return Ok(());
            }
            Err(error) => {
                last_error = error.to_string();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("NATS test container did not become ready at {url}: {last_error}"),
    )
    .into())
}
