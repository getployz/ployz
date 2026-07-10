//! Interactive confirmation gates for destructive CLI operations.

use std::io::{self, Write};

use ployz_core::deploy::VolumeName;
use ployz_core::ids::NamespaceId;

use crate::runtime::PloyzctlExecutionError;

pub(crate) fn confirm_namespace_remove(
    namespace_id: &NamespaceId,
) -> Result<(), PloyzctlExecutionError> {
    let prompt = crate::commands::namespace::NamespaceRemoveConfirmation {
        namespace_id: namespace_id.clone(),
        volume_backed_services: Vec::new(),
    }
    .prompt();
    eprint!("{prompt}");
    io::stderr().flush().map_err(|error| {
        PloyzctlExecutionError::ReadNamespaceRemoveConfirmation {
            message: error.to_string(),
        }
    })?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(|error| {
        PloyzctlExecutionError::ReadNamespaceRemoveConfirmation {
            message: error.to_string(),
        }
    })?;
    if answer.trim() == namespace_id.as_str() {
        Ok(())
    } else {
        Err(PloyzctlExecutionError::NamespaceRemoveNotConfirmed {
            namespace_id: namespace_id.clone(),
        })
    }
}

pub(crate) fn confirm_volume_remove(
    namespace_id: &NamespaceId,
    volume_name: &VolumeName,
) -> Result<(), PloyzctlExecutionError> {
    let confirmation = crate::commands::volume::VolumeRemoveConfirmation {
        namespace_id: namespace_id.clone(),
        volume_name: volume_name.clone(),
    };
    eprint!("{}", confirmation.prompt());
    io::stderr().flush().map_err(
        |error| PloyzctlExecutionError::ReadVolumeRemoveConfirmation {
            message: error.to_string(),
        },
    )?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(|error| {
        PloyzctlExecutionError::ReadVolumeRemoveConfirmation {
            message: error.to_string(),
        }
    })?;
    if answer.trim() == confirmation.confirmation() {
        Ok(())
    } else {
        Err(PloyzctlExecutionError::VolumeRemoveNotConfirmed {
            namespace_id: namespace_id.clone(),
            volume_name: volume_name.clone(),
        })
    }
}
