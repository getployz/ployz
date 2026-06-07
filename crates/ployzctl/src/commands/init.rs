use ployz_core::ids::NodeId;
pub use ployz_core::roles::{FirstNodeGateway, first_node_process_set};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstNodeInitOutput {
    pub node_id: NodeId,
    pub gateway: FirstNodeGateway,
}

impl FirstNodeInitOutput {
    #[must_use]
    pub fn render(&self) -> String {
        let process_set = first_node_process_set(&self.node_id, self.gateway);
        let roles = process_set
            .roles()
            .iter()
            .map(|role| role.process_name())
            .collect::<Vec<_>>()
            .join(" ");

        format!(
            "init first node {}\nsupervise {}\nsupervise roles {}\n",
            self.node_id.as_str(),
            process_set.nats_server.process_name(),
            roles
        )
    }
}
