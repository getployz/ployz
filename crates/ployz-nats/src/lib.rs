mod backend;
pub mod buckets;
pub mod config;
pub mod coord;
pub mod role;
pub mod store;
pub mod subjects;

use async_nats::Client;
use buckets::AssetPolicy;
use ployz_store_api::StoreRuntimeControl;
use ployz_types::error::{Error, Result};
use ployz_types::model::OverlayIp;
use serde::Deserialize;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;
use store::deploys::CachedDeployProjection;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct NatsStore {
    client: Client,
    jetstream: async_nats::jetstream::Context,
    asset_policy: AssetPolicy,
    pub(crate) deploy_projection: Arc<RwLock<Option<CachedDeployProjection>>>,
}

impl NatsStore {
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self::with_policy(
            client,
            AssetPolicy {
                storage_candidates: 1,
                replica_preference: role::ReplicaPreference::Default,
            },
        )
    }

    #[must_use]
    pub fn with_policy(client: Client, asset_policy: AssetPolicy) -> Self {
        let jetstream = async_nats::jetstream::with_domain(client.clone(), config::HUB_DOMAIN);
        Self {
            client,
            jetstream,
            asset_policy,
            deploy_projection: Arc::new(RwLock::new(None)),
        }
    }

    #[must_use]
    pub fn client(&self) -> &Client {
        &self.client
    }

    #[must_use]
    pub fn jetstream(&self) -> &async_nats::jetstream::Context {
        &self.jetstream
    }

    #[must_use]
    pub fn asset_policy(&self) -> AssetPolicy {
        self.asset_policy
    }

    pub async fn connect(url: &str) -> Result<Self> {
        let client = async_nats::connect(url)
            .await
            .map_err(|error| Error::operation("nats_connect", error.to_string()))?;
        Ok(Self::new(client))
    }

    pub async fn connect_for_network(data_dir: &Path, network: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct NetworkConfig {
            overlay_ip: OverlayIp,
        }

        let path = data_dir.join("networks").join(network).join("network.json");
        let data = std::fs::read_to_string(&path).map_err(|error| {
            Error::operation(
                "nats_network_config_read",
                format!("read {}: {error}", path.display()),
            )
        })?;
        let config: NetworkConfig = serde_json::from_str(&data).map_err(|error| {
            Error::operation(
                "nats_network_config_decode",
                format!("decode {}: {error}", path.display()),
            )
        })?;
        let mut last_error = None;
        for url in client_urls_for_network(config.overlay_ip) {
            match Self::connect(&url).await {
                Ok(store) => match store.start().await {
                    Ok(()) => return Ok(store),
                    Err(error) => last_error = Some(error),
                },
                Err(error) => last_error = Some(error),
            }
        }
        let Some(error) = last_error else {
            return Err(Error::operation("nats_connect", "no client URLs available"));
        };
        Err(error)
    }
}

fn client_urls_for_network(overlay_ip: OverlayIp) -> [String; 2] {
    [
        format!("nats://{}:{}", Ipv4Addr::LOCALHOST, config::CLIENT_PORT),
        format!("nats://[{}]:{}", overlay_ip.0, config::CLIENT_PORT),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_client_urls_try_local_bridge_before_overlay() {
        let urls = client_urls_for_network(OverlayIp("fd00::1".parse().expect("valid ip")));

        assert_eq!(urls[0], "nats://127.0.0.1:4222");
        assert_eq!(urls[1], "nats://[fd00::1]:4222");
    }
}
