//! Product-shaped Host Runner plans, ordered execution, and progress evidence.

pub(crate) mod execution;
pub(crate) mod nats_material;
pub(crate) mod progress;
pub(crate) mod steps;

#[cfg(test)]
pub use execution::HostRunnerRecordFailure;
pub use execution::{
    HostRunnerPlanExecution, HostRunnerPlanFailure, HostRunnerPlanTerminal, HostRunnerStepEffects,
    HostRunnerStepEvent, HostRunnerStepRecorder, execute_host_runner_plan,
};
pub use progress::HostRunnerTextRecorder;
#[cfg(test)]
pub use progress::render_step_event;
pub use steps::*;
