use std::error::Error;
use std::io;
use std::time::Duration;

pub struct TestNats {
    _server: nats_server::Server,
    url: String,
}

impl TestNats {
    pub async fn start_jetstream() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let config = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ployz-nats/tests/configs/jetstream.conf"
        );
        let server = nats_server::run_server(config);
        let url = server.client_url();
        wait_for_nats(&url).await?;

        Ok(Self {
            _server: server,
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
        format!("NATS test server did not become ready at {url}: {last_error}"),
    )
    .into())
}
