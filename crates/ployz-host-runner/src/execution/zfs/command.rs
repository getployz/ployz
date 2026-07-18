//! Bounded ZFS command and output parsing seams.

use std::time::Duration;

use ployz_core::storage::StorageEffectFailure as ZfsEffectError;

use crate::execution::{HostRunnerCommandOutput, HostRunnerCommandRunner};

pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const INSTALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Clone, Copy)]
pub(super) enum EffectClass {
    Install,
    PoolList,
    OwnedPool,
    Dataset,
    Destructive,
    Mismatch,
}

pub(super) fn checked(
    runner: &mut impl HostRunnerCommandRunner,
    program: &str,
    args: &[&str],
    timeout: Duration,
    class: EffectClass,
) -> Result<HostRunnerCommandOutput, ZfsEffectError> {
    let output = runner
        .command_with_timeout(program, args, timeout)
        .map_err(|error| effect_error(class, error.to_string()))?;
    if !output.success {
        return Err(effect_error(class, output.failure));
    }
    Ok(output)
}

fn effect_error(class: EffectClass, message: String) -> ZfsEffectError {
    match class {
        EffectClass::Install => ZfsEffectError::Installation { message },
        EffectClass::PoolList => ZfsEffectError::PoolList { message },
        EffectClass::OwnedPool => ZfsEffectError::OwnedPool { message },
        EffectClass::Dataset => ZfsEffectError::Dataset { message },
        EffectClass::Destructive => ZfsEffectError::DestructiveEffect { message },
        EffectClass::Mismatch => ZfsEffectError::PreparedStateMismatch { message },
    }
}

pub(super) fn parse_u64(label: &str, value: &str) -> Result<u64, ZfsEffectError> {
    value.parse().map_err(|error| ZfsEffectError::GatherParse {
        message: format!("{label} {value:?}: {error}"),
    })
}
