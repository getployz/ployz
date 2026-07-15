use crate::deploy::compose_command::ComposeCheckCommand;

use crate::execution_support::{CommandExit, PloyzctlExecutionOutput};

pub(crate) fn check(command: ComposeCheckCommand) -> PloyzctlExecutionOutput {
    let stderr = command.diagnostics.join("\n");
    PloyzctlExecutionOutput {
        stdout: String::new(),
        stderr: if stderr.is_empty() {
            stderr
        } else {
            stderr + "\n"
        },
        exit: CommandExit::Success,
    }
}

#[cfg(test)]
mod tests {
    use super::check;
    use crate::deploy::compose_command::ComposeCheckCommand;
    use crate::execution_support::{CommandExit, PloyzctlExecutionOutput};

    #[test]
    fn check_renders_exact_diagnostic_bytes() {
        let output = check(ComposeCheckCommand {
            diagnostics: vec!["first".to_owned(), "second".to_owned()],
        });

        assert_eq!(
            output,
            PloyzctlExecutionOutput {
                stdout: String::new(),
                stderr: "first\nsecond\n".to_owned(),
                exit: CommandExit::Success,
            }
        );
    }

    #[test]
    fn check_without_diagnostics_is_silent() {
        let output = check(ComposeCheckCommand {
            diagnostics: Vec::new(),
        });

        assert_eq!(output, PloyzctlExecutionOutput::default());
    }
}
