use serde::{Deserialize, Serialize};

use crate::operation::FailureMessage;

use super::MachineEndpointSubnet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum EndpointBridgeStatus {
    Ready {
        subnet: MachineEndpointSubnet,
    },
    Missing,
    SubnetMismatch {
        expected: MachineEndpointSubnet,
        observed: MachineEndpointSubnet,
    },
    InvalidSubnet {
        observed: String,
    },
    Unavailable {
        message: FailureMessage,
    },
}
