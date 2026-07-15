use ployz_core::operation::OperationOutcome;

/// Whether a completed command should exit the process successfully. A command
/// that followed an operation to a failed or cancelled terminal state prints
/// that failure but must still exit non-zero so scripts can detect it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommandExit {
    #[default]
    Success,
    Failure,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PloyzctlExecutionOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit: CommandExit,
}

impl PloyzctlExecutionOutput {
    #[must_use]
    pub fn stdout(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            exit: CommandExit::Success,
        }
    }

    #[must_use]
    pub fn with_stderr(mut self, stderr: String) -> Self {
        self.stderr = stderr;
        self
    }

    /// Records the terminal outcome of a followed operation on the output: a
    /// failed or cancelled operation exits non-zero, a successful one (or one
    /// whose outcome could not be determined) leaves the exit unchanged.
    #[must_use]
    pub fn with_operation_outcome(mut self, outcome: Option<OperationOutcome>) -> Self {
        if matches!(outcome, Some(outcome) if !outcome.is_success()) {
            self.exit = CommandExit::Failure;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandExit, PloyzctlExecutionOutput};
    use ployz_core::operation::OperationOutcome;

    #[test]
    fn failed_and_cancelled_operations_exit_non_zero() {
        for outcome in [OperationOutcome::Failed, OperationOutcome::Cancelled] {
            let output = PloyzctlExecutionOutput::stdout(String::new())
                .with_operation_outcome(Some(outcome));
            assert_eq!(output.exit, CommandExit::Failure, "outcome {outcome:?}");
        }
    }

    #[test]
    fn successful_or_unknown_operations_keep_success_exit() {
        for outcome in [Some(OperationOutcome::Succeeded), None] {
            let output =
                PloyzctlExecutionOutput::stdout(String::new()).with_operation_outcome(outcome);
            assert_eq!(output.exit, CommandExit::Success, "outcome {outcome:?}");
        }
    }
}
