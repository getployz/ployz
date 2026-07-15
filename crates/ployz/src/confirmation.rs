//! Interactive confirmation gates for destructive CLI operations.

use std::io::{self, Write};

use crate::execution_support::PloyzctlExecutionError;

/// Prompt on stderr, read one line from stdin, and require it to equal
/// `expected`. `read_error` classifies an stderr/stdin I/O failure;
/// `not_confirmed` classifies a mismatched answer. Both destructive gates
/// share this skeleton so only the prompt, the expected phrase, and the two
/// error shapes vary.
pub(crate) fn read_typed_confirmation(
    prompt: &str,
    expected: &str,
    read_error: impl Fn(String) -> PloyzctlExecutionError,
    not_confirmed: impl FnOnce() -> PloyzctlExecutionError,
) -> Result<(), PloyzctlExecutionError> {
    eprint!("{prompt}");
    io::stderr()
        .flush()
        .map_err(|error| read_error(error.to_string()))?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| read_error(error.to_string()))?;
    if answer.trim() == expected {
        Ok(())
    } else {
        Err(not_confirmed())
    }
}
