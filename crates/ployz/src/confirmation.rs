//! Interactive confirmation gates for destructive CLI operations.

use std::io::{self, Write};

use ployz_core::deploy::VolumeName;
use ployz_core::ids::NamespaceId;

use crate::execution_support::PloyzctlExecutionError;

/// Prompt on stderr, read one line from stdin, and require it to equal
/// `expected`. `read_error` classifies an stderr/stdin I/O failure;
/// `not_confirmed` classifies a mismatched answer. Both destructive gates
/// share this skeleton so only the prompt, the expected phrase, and the two
/// error shapes vary.
fn read_typed_confirmation(
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

pub(crate) fn confirm_namespace_remove(
    namespace_id: &NamespaceId,
) -> Result<(), PloyzctlExecutionError> {
    let prompt = crate::commands::namespace::NamespaceRemoveConfirmation {
        namespace_id: namespace_id.clone(),
        volume_backed_services: Vec::new(),
    }
    .prompt();
    read_typed_confirmation(
        &prompt,
        namespace_id.as_str(),
        |message| PloyzctlExecutionError::ReadNamespaceRemoveConfirmation { message },
        || PloyzctlExecutionError::NamespaceRemoveNotConfirmed {
            namespace_id: namespace_id.clone(),
        },
    )
}

pub(crate) fn confirm_volume_remove(
    namespace_id: &NamespaceId,
    volume_name: &VolumeName,
) -> Result<(), PloyzctlExecutionError> {
    let confirmation = crate::commands::volume::VolumeRemoveConfirmation {
        namespace_id: namespace_id.clone(),
        volume_name: volume_name.clone(),
    };
    read_typed_confirmation(
        &confirmation.prompt(),
        &confirmation.confirmation(),
        |message| PloyzctlExecutionError::ReadVolumeRemoveConfirmation { message },
        || PloyzctlExecutionError::VolumeRemoveNotConfirmed {
            namespace_id: namespace_id.clone(),
            volume_name: volume_name.clone(),
        },
    )
}
