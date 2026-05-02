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
use ployz_types::model::{MachineRole, OverlayIp};
use serde::Deserialize;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;
use store::deploys::CachedDeployProjection;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct NatsStore {
    client: Client,
    hub_js: async_nats::jetstream::Context,
    local_js: async_nats::jetstream::Context,
    role: MachineRole,
    leaf_domain: Option<String>,
    asset_policy: AssetPolicy,
    pub(crate) deploy_projection: Arc<RwLock<Option<CachedDeployProjection>>>,
}

impl NatsStore {
    /// Build a `NatsStore` with the default `R=1` policy and `StorageCandidate` role.
    /// Use this for short-lived clients that only need hub access (CLI tools, tests).
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self::for_role(
            client,
            MachineRole::StorageCandidate,
            None,
            AssetPolicy {
                storage_candidates: 1,
                replica_preference: role::ReplicaPreference::Default,
            },
        )
    }

    #[must_use]
    pub fn with_policy(client: Client, asset_policy: AssetPolicy) -> Self {
        Self::for_role(client, MachineRole::StorageCandidate, None, asset_policy)
    }

    #[must_use]
    pub fn for_role(
        client: Client,
        role: MachineRole,
        leaf_domain: Option<String>,
        asset_policy: AssetPolicy,
    ) -> Self {
        let hub_js = async_nats::jetstream::with_domain(client.clone(), config::HUB_DOMAIN);
        let local_js = match (role, leaf_domain.as_deref()) {
            (MachineRole::Mirror, Some(domain)) => {
                async_nats::jetstream::with_domain(client.clone(), domain)
            }
            // StorageCandidate's local context is the hub. Leaf has no local JetStream;
            // we still hand back the hub context so callers don't have to special-case
            // it — Leaf-role daemons should not be issuing local reads anyway.
            (MachineRole::StorageCandidate, _)
            | (MachineRole::Leaf, _)
            | (MachineRole::Mirror, None) => hub_js.clone(),
        };
        Self {
            client,
            hub_js,
            local_js,
            role,
            leaf_domain,
            asset_policy,
            deploy_projection: Arc::new(RwLock::new(None)),
        }
    }

    #[must_use]
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// JetStream context for the cluster-wide hub domain. Use for writes,
    /// locks, atomic publishes, and anything that needs linearizability.
    #[must_use]
    pub fn hub_jetstream(&self) -> &async_nats::jetstream::Context {
        &self.hub_js
    }

    /// JetStream context for the node's local domain. Equal to `hub_jetstream`
    /// on `StorageCandidate`. On `Mirror`, points at the leaf domain so reads
    /// can be served from local mirrored streams without leafnode round-trips.
    #[must_use]
    pub fn local_jetstream(&self) -> &async_nats::jetstream::Context {
        &self.local_js
    }

    /// Backwards-compatible alias for `hub_jetstream`. Existing call sites
    /// publish, take locks, and run atomic batches against this — all of those
    /// operations must stay hub-targeted.
    #[must_use]
    pub fn jetstream(&self) -> &async_nats::jetstream::Context {
        &self.hub_js
    }

    #[must_use]
    pub fn role(&self) -> MachineRole {
        self.role
    }

    #[must_use]
    pub fn leaf_domain(&self) -> Option<&str> {
        self.leaf_domain.as_deref()
    }

    #[must_use]
    pub fn asset_policy(&self) -> AssetPolicy {
        self.asset_policy
    }

    pub async fn connect(url: &str) -> Result<Self> {
        Self::connect_with_role(url, MachineRole::StorageCandidate, None).await
    }

    pub async fn connect_with_role(
        url: &str,
        role: MachineRole,
        leaf_domain: Option<String>,
    ) -> Result<Self> {
        let client = async_nats::connect(url)
            .await
            .map_err(|error| Error::operation("nats_connect", error.to_string()))?;
        Ok(Self::for_role(
            client,
            role,
            leaf_domain,
            AssetPolicy {
                storage_candidates: 1,
                replica_preference: role::ReplicaPreference::Default,
            },
        ))
    }

    pub async fn connect_for_network(data_dir: &Path, network: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct NetworkConfig {
            overlay_ip: OverlayIp,
            #[serde(default = "default_machine_role")]
            machine_role: MachineRole,
        }

        fn default_machine_role() -> MachineRole {
            MachineRole::StorageCandidate
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
        let leaf_domain = match config.machine_role {
            MachineRole::Mirror => Some(config::leaf_domain(config.overlay_ip.0)),
            MachineRole::StorageCandidate | MachineRole::Leaf => None,
        };
        let mut last_error = None;
        for url in client_urls_for_network(config.overlay_ip) {
            match Self::connect_with_role(&url, config.machine_role, leaf_domain.clone()).await {
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
