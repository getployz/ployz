use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeCommand {
    Probe,
    ReceiveImage { operation_id: String },
    ReceiveVolume { transfer_id: String },
    PromoteStorage { operation_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeClientError {
    pub node_id: String,
    pub message: String,
}

#[async_trait]
pub trait NodePeerClient: Send + Sync {
    fn node_id(&self) -> &str;
    async fn send(&self, command: NodeCommand) -> Result<(), NodeClientError>;
}

#[derive(Default, Clone)]
pub struct NodeClientRegistry<C> {
    clients: Arc<Mutex<BTreeMap<String, C>>>,
}

impl<C> NodeClientRegistry<C>
where
    C: Clone + NodePeerClient,
{
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn insert(&self, client: C) {
        self.clients
            .lock()
            .expect("node clients")
            .insert(client.node_id().to_string(), client);
    }

    pub async fn send(&self, node_id: &str, command: NodeCommand) -> Result<(), NodeClientError> {
        let client = self
            .clients
            .lock()
            .expect("node clients")
            .get(node_id)
            .cloned();
        let Some(client) = client else {
            return Err(NodeClientError {
                node_id: node_id.to_string(),
                message: "node client not registered".to_string(),
            });
        };
        client.send(command).await
    }
}
