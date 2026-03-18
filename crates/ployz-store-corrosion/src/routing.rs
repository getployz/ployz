use std::path::Path;

use ployz_corrosion::CorrosionStore;
use ployz_types::Result;
use ployz_types::model::RoutingState;
use ployz_types::store::RoutingStore;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct CorrosionRoutingStore {
    inner: CorrosionStore,
}

impl CorrosionRoutingStore {
    pub async fn connect_for_network(data_dir: &Path, network: &str) -> Result<Self> {
        CorrosionStore::connect_for_network(data_dir, network)
            .await
            .map(|inner| Self { inner })
    }

    pub async fn load_routing_state(&self) -> Result<RoutingState> {
        RoutingStore::load_routing_state(&self.inner).await
    }

    pub async fn subscribe_routing_invalidations(&self) -> Result<mpsc::Receiver<()>> {
        RoutingStore::subscribe_routing_invalidations(&self.inner).await
    }
}
