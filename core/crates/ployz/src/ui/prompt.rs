//! Questions put to a person. Only Interactive mode with a terminal on stdin
//! asks; anywhere else a prompt returns its refusal, which names the flag
//! that answers it. A declined prompt says what did not happen and exits 130.

use std::io::{self, IsTerminal as _};

use dialoguer::{Confirm, Input, Select, console::Term};

use super::{Mode, mode};
use crate::failure::Failure;

/// Whether this run may ask: Interactive mode and a terminal to read from.
#[must_use]
pub(crate) fn can_prompt() -> bool {
    mode() == Mode::Interactive && io::stdin().is_terminal()
}

/// Ask for `name` to be typed. Anything else, an empty answer or Ctrl-C
/// declines, printing `declined`.
pub(crate) fn confirm_name(
    name: &str,
    refusal: impl FnOnce() -> Failure,
    declined: &str,
) -> Result<(), Failure> {
    if !can_prompt() {
        return Err(refusal());
    }
    let answer = ask(|| {
        Input::<String>::new()
            .with_prompt(format!("Type {name} to confirm"))
            .allow_empty(true)
            .interact_text_on(&Term::stderr())
            .map(Some)
    })?;
    if answer.as_deref().map(str::trim) == Some(name) {
        Ok(())
    } else {
        Err(decline(declined))
    }
}

/// Ask a yes/no question that defaults to no.
pub(crate) fn confirm(
    question: &str,
    refusal: impl FnOnce() -> Failure,
    declined: &str,
) -> Result<(), Failure> {
    if !can_prompt() {
        return Err(refusal());
    }
    let answer = ask(|| {
        Confirm::new()
            .with_prompt(question)
            .default(false)
            .interact_on_opt(&Term::stderr())
    })?;
    if answer == Some(true) {
        Ok(())
    } else {
        Err(decline(declined))
    }
}

/// Pick one of `items`, starting on `default`. Esc or Ctrl-C declines.
pub(crate) fn select<T: ToString>(
    title: &str,
    items: &[T],
    default: usize,
    refusal: impl FnOnce() -> Failure,
    declined: &str,
) -> Result<usize, Failure> {
    if !can_prompt() {
        return Err(refusal());
    }
    ask(|| {
        Select::new()
            .with_prompt(title)
            .items(items.iter().map(ToString::to_string))
            .default(default)
            .interact_on_opt(&Term::stderr())
    })?
    .ok_or_else(|| decline(declined))
}

/// Ctrl-C reaches the prompt as `Interrupted`: that is a no, and the cursor a
/// select hid comes back.
fn ask<T>(prompt: impl FnOnce() -> dialoguer::Result<Option<T>>) -> Result<Option<T>, Failure> {
    match crate::cancellation::while_prompting(prompt)? {
        Ok(answer) => Ok(answer),
        Err(dialoguer::Error::IO(error)) => {
            let _ = Term::stderr().show_cursor();
            if error.kind() == io::ErrorKind::Interrupted {
                let _ = Term::stderr().write_line("");
                Ok(None)
            } else {
                Err(Failure::from(error))
            }
        }
    }
}

fn decline(declined: &str) -> Failure {
    super::note(declined);
    Failure::exit(super::exit::CANCELLED_EXIT).interrupted()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::RpcErrorCode;

    fn refusal() -> Failure {
        Failure::coded(RpcErrorCode::ConfirmationRequired, "needs a terminal")
    }

    #[test]
    fn every_prompt_refuses_without_an_interactive_terminal() {
        // Tests run in Plain mode, the default before dispatch.
        let refused = |result: Result<(), Failure>| {
            let error = result.unwrap_err();
            assert_eq!(error.report().code, RpcErrorCode::ConfirmationRequired);
            assert_eq!(crate::ui::exit_code(&error.report().code), 2);
        };
        refused(confirm_name("blog", refusal, "Cancelled."));
        refused(confirm("Reset?", refusal, "Cancelled."));
        refused(select("Pick", &["a", "b"], 0, refusal, "Cancelled.").map(drop));
    }
}
