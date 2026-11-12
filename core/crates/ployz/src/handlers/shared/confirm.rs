use clap::ArgMatches;
use ployz_core::Hint;

use super::super::Error;
use crate::ui::{self, Tree};

/// `--confirm` typed `name`, or a person typed it at the prompt after seeing
/// `unconfirmed`'s loss tree. Anywhere else its refusal is the error; a
/// different `--confirm` is a usage error naming `retry`.
pub(in crate::handlers) fn confirm(
    matches: &ArgMatches,
    name: &str,
    what: &str,
    retry: String,
    unconfirmed: impl FnOnce() -> Result<(Error, Tree), Error>,
) -> Result<(), Error> {
    match matches.get_one::<String>("confirm") {
        Some(typed) if typed == name => Ok(()),
        Some(typed) => Err(Error::usage(format!(
            "--confirm {} does not match {what} {name}. No changes made.",
            typed.escape_debug()
        ))
        .hint(Hint::Retry(retry))),
        None => {
            let (refusal, loss) = unconfirmed()?;
            if ui::can_prompt() {
                ui::note_inline(&loss);
            }
            ui::confirm_name(name, || refusal, "Cancelled. Nothing was removed.")
        }
    }
}
