mod backend;
pub mod buckets;
pub mod config;
pub mod coord;
pub mod role;
pub mod store;
pub mod subjects;

use async_nats::Client;
use buckets::AssetPolicy;
use ployz_types::error::{Error, Result};

#[derive(Clone)]
pub struct NatsStore {
    client: Client,
    jetstream: async_nats::jetstream::Context,
    asset_policy: AssetPolicy,
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
        let jetstream = async_nats::jetstream::new(client.clone());
        Self {
            client,
            jetstream,
            asset_policy,
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
}
