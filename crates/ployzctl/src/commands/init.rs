use ployz_core::ids::NodeId;
pub use ployz_core::roles::{FirstNodeGateway, first_node_process_set};

use crate::commands::{ArgCursor, PloyzctlCliError, invalid_value, required, set_once};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstNodeInitCommand {
    pub node_id: NodeId,
    pub gateway: FirstNodeGateway,
}

impl FirstNodeInitCommand {
    #[must_use]
    pub fn output(&self) -> FirstNodeInitOutput {
        FirstNodeInitOutput {
            node_id: self.node_id.clone(),
            gateway: self.gateway,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.output().render()
    }
}

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

pub fn parse_init_command(args: &[String]) -> Result<FirstNodeInitCommand, PloyzctlCliError> {
    let mut node_id = None;
    let mut gateway = FirstNodeGateway::Skip;
    let mut args = ArgCursor::new(args);

    while !args.is_empty() {
        if args.take_flag("--gateway") {
            if gateway == FirstNodeGateway::Install {
                return Err(PloyzctlCliError::DuplicateArgument { flag: "--gateway" });
            }
            gateway = FirstNodeGateway::Install;
            continue;
        }
        if let Some(value) = args.take_value("--node")? {
            set_once(&mut node_id, value, "--node")?;
            continue;
        }
        return Err(args.unexpected());
    }

    Ok(FirstNodeInitCommand {
        node_id: NodeId::try_new(required(node_id, "--node")?)
            .map_err(|error| invalid_value("--node", error))?,
        gateway,
    })
}
