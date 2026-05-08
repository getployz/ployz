mod backend;
mod buckets;
pub mod config;
mod coord;
mod store;
mod subjects;

use async_nats::Client;
use buckets::NatsAssetNames;
use ployz_store_api::StoreRuntimeControl;
use ployz_types::error::{Error, Result};
use ployz_types::model::{MachineId, OverlayIp, StorageParticipation};
use serde::Deserialize;
use std::net::Ipv4Addr;
use std::path::Path;

use buckets::AssetPolicy;
pub use buckets::{NatsAssetScope, NatsAssetSpec};
pub use coord::jobs::{
    CertRenewalConsumerPolicy, CertRenewalJobDelivery, JobSchedule, NatsCertRenewalJobConsumer,
};
pub use coord::locks::{Lease, LockAcquireError, NatsDeployLock, NatsLocks};
pub use coord::rpc::{
    NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcFailureKind, RpcPolicy,
    decode_daemon_request, encode_daemon_response,
};
pub use subjects::{NatsScope, acme_account_lock, cert_lock, deploy_lock, subnet_lock};

#[derive(Deserialize)]
struct NetworkConfig {
    overlay_ip: OverlayIp,
    #[serde(default)]
    storage_participation: Option<StorageParticipation>,
}

#[derive(Clone)]
pub struct NatsStore {
    client: Client,
    jetstream: async_nats::jetstream::Context,
    asset_policy: AssetPolicy,
    assets: NatsAssetNames,
    scope: NatsScope,
}

pub struct NatsNodeCommandListener {
    client: Client,
    subject: String,
    queue_group: String,
}

impl NatsNodeCommandListener {
    #[must_use]
    pub fn into_parts(self) -> (Client, String, String) {
        (self.client, self.subject, self.queue_group)
    }
}

impl NatsStore {
    #[must_use]
    pub fn local(client: Client) -> Self {
        Self::local_with_policy(client, AssetPolicy { replicas: 1 })
    }

    #[must_use]
    pub(crate) fn local_with_policy(client: Client, asset_policy: AssetPolicy) -> Self {
        Self::with_scope_and_policy(client, NatsScope::local_default(), asset_policy)
    }

    #[must_use]
    pub fn with_scope(client: Client, scope: NatsScope) -> Self {
        Self::with_scope_and_policy(client, scope, AssetPolicy { replicas: 1 })
    }

    #[must_use]
    pub(crate) fn with_scope_and_policy(
        client: Client,
        scope: NatsScope,
        asset_policy: AssetPolicy,
    ) -> Self {
        let jetstream =
            async_nats::jetstream::with_domain(client.clone(), scope.authority_domain());
        let assets = NatsAssetNames::new(&scope);
        Self {
            client,
            jetstream,
            asset_policy,
            assets,
            scope,
        }
    }

    #[must_use]
    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    #[must_use]
    pub(crate) fn jetstream(&self) -> &async_nats::jetstream::Context {
        &self.jetstream
    }

    #[must_use]
    pub(crate) fn asset_policy(&self) -> AssetPolicy {
        self.asset_policy
    }

    #[must_use]
    pub(crate) fn assets(&self) -> &NatsAssetNames {
        &self.assets
    }

    #[must_use]
    pub fn asset_manifest(&self) -> Vec<NatsAssetSpec> {
        Self::asset_manifest_for_scope(&self.scope)
    }

    #[must_use]
    pub fn asset_manifest_for_scope(scope: &NatsScope) -> Vec<NatsAssetSpec> {
        let assets = NatsAssetNames::new(scope);
        assets
            .stream_assets()
            .into_iter()
            .chain(assets.kv_assets())
            .collect()
    }

    #[must_use]
    pub fn node_command_listener(&self, machine_id: &MachineId) -> NatsNodeCommandListener {
        NatsNodeCommandListener {
            client: self.client.clone(),
            subject: subjects::node_command_listener_in(&self.scope, machine_id),
            queue_group: subjects::node_command_queue_group(machine_id),
        }
    }

    pub async fn asset_stream_info(
        &self,
        asset: &NatsAssetSpec,
    ) -> Result<async_nats::jetstream::stream::Info> {
        let stream = match asset.kind {
            "kv" => format!("KV_{}", asset.name),
            _ => asset.name.clone(),
        };
        let mut stream_handle = self
            .jetstream
            .get_stream(stream.as_str())
            .await
            .map_err(|error| Error::operation("nats_asset_stream", format!("{error:?}")))?;
        stream_handle
            .info()
            .await
            .map_err(|error| Error::operation("nats_asset_stream_info", format!("{error:?}")))
            .cloned()
    }

    #[must_use]
    pub fn scope(&self) -> &NatsScope {
        &self.scope
    }

    #[must_use]
    pub fn installation(&self) -> &ployz_types::model::InstallationId {
        self.scope.installation()
    }

    #[must_use]
    pub fn authority(&self) -> &ployz_types::model::AuthorityId {
        self.scope.authority()
    }

    #[must_use]
    pub fn asset_domain(&self, scope: NatsAssetScope) -> String {
        match scope {
            NatsAssetScope::AuthorityLocal => self.scope.authority_domain(),
            NatsAssetScope::InstallationRoot => self.scope.root_domain(),
        }
    }

    pub async fn connect_local(url: &str) -> Result<Self> {
        let client = async_nats::connect(url)
            .await
            .map_err(|error| Error::operation("nats_connect", error.to_string()))?;
        Ok(Self::local(client))
    }

    pub async fn connect_with_scope(url: &str, scope: NatsScope) -> Result<Self> {
        let client = async_nats::connect(url)
            .await
            .map_err(|error| Error::operation("nats_connect", error.to_string()))?;
        Ok(Self::with_scope(client, scope))
    }

    pub async fn connect_for_network(data_dir: &Path, network: &str) -> Result<Self> {
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
        let scope = nats_scope_for_network_config(&config);
        let mut last_error = None;
        for url in client_urls_for_network(config.overlay_ip) {
            match Self::connect_with_scope(&url, scope.clone()).await {
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

fn nats_scope_for_network_config(config: &NetworkConfig) -> NatsScope {
    NatsScope::local_for_storage_participation(
        config
            .storage_participation
            .as_ref()
            .unwrap_or(&StorageParticipation::default_authority()),
    )
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
    use ployz_types::model::{AuthorityId, InstallationId};

    #[test]
    fn network_client_urls_try_local_bridge_before_overlay() {
        let urls = client_urls_for_network(OverlayIp("fd00::1".parse().expect("valid ip")));

        assert_eq!(urls[0], "nats://127.0.0.1:4222");
        assert_eq!(urls[1], "nats://[fd00::1]:4222");
    }

    #[test]
    fn network_config_scope_uses_persisted_authority() {
        let config = NetworkConfig {
            overlay_ip: OverlayIp("fd00::1".parse().expect("valid ip")),
            storage_participation: Some(StorageParticipation::Authority {
                authority_id: AuthorityId("auth-sin".into()),
            }),
        };

        let scope = nats_scope_for_network_config(&config);

        assert_eq!(scope.installation(), &InstallationId::local());
        assert_eq!(scope.authority(), &AuthorityId("auth-sin".into()));
    }

    #[test]
    fn network_config_scope_defaults_only_without_authority() {
        let config = NetworkConfig {
            overlay_ip: OverlayIp("fd00::1".parse().expect("valid ip")),
            storage_participation: Some(StorageParticipation::Candidate),
        };

        let scope = nats_scope_for_network_config(&config);

        assert_eq!(scope.installation(), &InstallationId::local());
        assert_eq!(scope.authority(), &AuthorityId::default_authority());
    }
}
