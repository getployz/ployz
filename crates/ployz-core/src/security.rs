//! Role and authority models for NATS subject permissions.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::MachineId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "principal", rename_all = "snake_case", deny_unknown_fields)]
pub enum NatsPrincipal {
    Machine { machine_id: MachineId },
    Controller,
    User,
    Join,
    System,
}

impl NatsPrincipal {
    /// The stable key this principal is stored and rendered under: KV
    /// authority records and the `authorized-users.conf` principal markers
    /// both use it, so storage and recovery evidence cannot disagree.
    #[must_use]
    pub fn authority_key(&self) -> String {
        match self {
            Self::Machine { machine_id } => format!("machine_{}", machine_id.as_str()),
            Self::Controller => "controller".to_owned(),
            Self::User => "user".to_owned(),
            Self::Join => "join".to_owned(),
            Self::System => "system".to_owned(),
        }
    }

    pub fn try_from_authority_key(key: &str) -> Result<Self, NatsPrincipalKeyError> {
        if let Some(machine_id) = key.strip_prefix("machine_") {
            let machine_id =
                MachineId::try_new(machine_id).map_err(|_| NatsPrincipalKeyError::Invalid {
                    key: key.to_owned(),
                })?;
            return Ok(Self::Machine { machine_id });
        }
        match key {
            "controller" => Ok(Self::Controller),
            "user" => Ok(Self::User),
            "join" => Ok(Self::Join),
            "system" => Ok(Self::System),
            _ => Err(NatsPrincipalKeyError::Invalid {
                key: key.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsPrincipalKeyError {
    Invalid { key: String },
}

impl fmt::Display for NatsPrincipalKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { key } => {
                write!(formatter, "{key:?} is not a NATS principal authority key")
            }
        }
    }
}

impl std::error::Error for NatsPrincipalKeyError {}
