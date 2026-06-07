use ployz_core::ids::NodeId;
pub use ployz_core::roles::{FirstNodeGateway, first_node_process_set};

use crate::commands::PloyzctlCliError;

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
    let mut rest = args;

    while !rest.is_empty() {
        match rest {
            [flag, tail @ ..] if flag == "--gateway" => {
                if gateway == FirstNodeGateway::Install {
                    return Err(PloyzctlCliError::DuplicateArgument { flag: "--gateway" });
                }
                gateway = FirstNodeGateway::Install;
                rest = tail;
            }
            [flag] if flag == "--node" => {
                return Err(PloyzctlCliError::MissingValue { flag: "--node" });
            }
            [flag, value, tail @ ..] if flag == "--node" => {
                set_once(&mut node_id, flag_value("--node", value)?, "--node")?;
                rest = tail;
            }
            [unknown, ..] => {
                return Err(PloyzctlCliError::UnexpectedArgument {
                    value: unknown.clone(),
                });
            }
            [] => unreachable!("loop exits for empty args"),
        }
    }

    Ok(FirstNodeInitCommand {
        node_id: NodeId::try_new(required(node_id, "--node")?).map_err(|error| {
            PloyzctlCliError::InvalidValue {
                flag: "--node",
                message: error.to_string(),
            }
        })?,
        gateway,
    })
}

fn flag_value(flag: &'static str, value: &str) -> Result<String, PloyzctlCliError> {
    if value.starts_with('-') {
        return Err(PloyzctlCliError::MissingValue { flag });
    }
    Ok(value.to_owned())
}

fn set_once(
    slot: &mut Option<String>,
    value: String,
    flag: &'static str,
) -> Result<(), PloyzctlCliError> {
    if slot.is_some() {
        return Err(PloyzctlCliError::DuplicateArgument { flag });
    }
    *slot = Some(value);
    Ok(())
}

fn required(value: Option<String>, flag: &'static str) -> Result<String, PloyzctlCliError> {
    value.ok_or(PloyzctlCliError::MissingRequiredArgument { flag })
}
