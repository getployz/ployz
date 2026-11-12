use clap::ArgMatches;
use ployz_core::Hint;

use super::super::Error;
use super::super::teardown::Drafted;
use crate::ui::{self, Tree};

/// `--confirm` typed `name`, or a person typed it at the prompt after seeing
/// `unconfirmed`'s loss tree and the lines of `signing`, the teardown this Device signs
/// once they confirm. Anywhere else its refusal is the error, before anything is signed;
/// a different `--confirm` is a usage error naming `retry`.
pub(in crate::handlers) fn confirm(
    matches: &ArgMatches,
    (name, signing): (&str, Option<&Drafted>),
    what: &str,
    retry: String,
    unconfirmed: impl FnOnce() -> Result<(Error, Tree), Error>,
) -> Result<(), Error> {
    match matches.get_one::<String>("confirm") {
        Some(typed) if typed == name => {
            signing.inspect(|signing| signing.show());
            Ok(())
        }
        Some(typed) => Err(Error::usage(format!(
            "--confirm {} does not match {what} {name}. No changes made.",
            typed.escape_debug()
        ))
        .hint(Hint::Retry(retry))),
        None => {
            let (refusal, loss) = unconfirmed()?;
            if ui::can_prompt() {
                ui::note_inline(&loss);
                signing.inspect(|signing| signing.show());
            }
            ui::confirm_name(name, || refusal, "Cancelled. Nothing was removed.")
        }
    }
}
