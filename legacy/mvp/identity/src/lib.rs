use std::collections::BTreeSet;
use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for NodeId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleNodes(BTreeSet<NodeId>);

impl VisibleNodes {
    #[must_use]
    pub fn new(nodes: impl IntoIterator<Item = NodeId>) -> Self {
        Self(nodes.into_iter().collect())
    }

    pub fn iter(&self) -> impl Iterator<Item = &NodeId> {
        self.0.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for VisibleNodes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let records = self
            .0
            .iter()
            .cloned()
            .map(|node_id| VisibleNodeRecord { node_id })
            .collect::<Vec<_>>();
        records.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for VisibleNodes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let records = Vec::<VisibleNodeRecord>::deserialize(deserializer)?;
        Ok(Self::new(records.into_iter().map(|record| record.node_id)))
    }
}

#[derive(Serialize, Deserialize)]
struct VisibleNodeRecord {
    node_id: NodeId,
}

#[cfg(test)]
mod tests {
    use super::{NodeId, VisibleNodes};

    #[test]
    fn visible_nodes_serialize_as_named_node_records() {
        let nodes = VisibleNodes::new([NodeId::new("node-b"), NodeId::new("node-a")]);

        let json = serde_json::to_string(&nodes).expect("visible nodes serialize");

        assert_eq!(json, r#"[{"node_id":"node-a"},{"node_id":"node-b"}]"#);
        let decoded: VisibleNodes = serde_json::from_str(&json).expect("visible nodes deserialize");
        let decoded_nodes = decoded.iter().cloned().collect::<Vec<_>>();
        assert_eq!(
            decoded_nodes,
            vec![NodeId::new("node-a"), NodeId::new("node-b")]
        );
    }
}
