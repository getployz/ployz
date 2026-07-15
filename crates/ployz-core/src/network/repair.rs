//! Bounded network repair request and per-machine outcome contracts.

use serde::{Deserialize, Serialize};

use crate::ids::MachineId;
use crate::machine::runtime::MachineFactsRefreshConfirmation;
use crate::operation::FailureMessage;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "failure", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkRepairRequestFailure {
    NoAnswer,
    TimedOut,
    RequestFailed { message: FailureMessage },
    ProtocolFailed { message: FailureMessage },
    DecodeFailed { message: FailureMessage },
    WrongResponder { actual_machine_id: MachineId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkRepairMachineFactsRefreshOutcome {
    Refreshed {
        refresh: MachineFactsRefreshConfirmation,
    },
    Unavailable {
        machine_id: MachineId,
        failure: NetworkRepairRequestFailure,
    },
    Failed {
        machine_id: MachineId,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "problem", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkRepairDnsRefreshProblem {
    Unavailable {
        machine_id: MachineId,
        failure: NetworkRepairRequestFailure,
    },
    ResolverNotServing {
        machine_id: MachineId,
    },
    Stale {
        machine_id: MachineId,
        stale_machine_ids: Vec<MachineId>,
    },
}
