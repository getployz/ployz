use crate::commands::compose::ComposeCheckCommand;

use super::PloyzctlExecutionOutput;

pub(super) fn check(command: ComposeCheckCommand) -> PloyzctlExecutionOutput {
    let stderr = command.diagnostics.join("\n");
    PloyzctlExecutionOutput {
        stdout: String::new(),
        stderr: if stderr.is_empty() {
            stderr
        } else {
            stderr + "\n"
        },
    }
}

#[cfg(test)]
mod tests {
    use super::check;
    use crate::commands::compose::ComposeCheckCommand;
    use crate::runtime::PloyzctlExecutionOutput;

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
