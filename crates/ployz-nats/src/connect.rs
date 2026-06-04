//! NATS client connection setup.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsClientEndpoint {
    pub url: String,
}

impl NatsClientEndpoint {
    #[must_use]
    pub fn loopback(port: u16) -> Self {
        Self {
            url: format!("nats://127.0.0.1:{port}"),
        }
    }
}
