//! A deployed Managed Volume's size. It lives in three places: Working State, the
//! `config_volume_storage` lock row and Applied State. The lock row is the one the
//! Machines answer to, so Head and Applied State read the lock, and only Cloud, which
//! drives the Machines through a resize, moves it. No `Command` reaches it, so
//! `/api/config/write`, the CLI and the agent can't claim a size the Machines don't
//! hold.

use ployz_core::config::{SavedEnvironmentIntent, VolumeKind};
use ployz_core::{ProvisionedVolumeMaximumBytes, RpcError};
use serde_json::json;

use crate::error;
use crate::id::EnvironmentId;
use crate::storage::Tx;

/// Whether a storage an intent asks for fits the storage a Volume was fixed at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StorageRule<'a> {
    /// A hand-typed change of one Volume, by resource ID: that Volume keeps its fixed
    /// storage exactly, and a different size points at `ployz volume set --size`,
    /// which resizes every copy and records the size. Without this a user could
    /// stage 5 GB on a Volume its Servers hold at 20 GB, deploy it, and read 5 GB in
    /// `volume ls`. Every other Volume of the intent is checked as `Replay`, so a
    /// History restore staged earlier doesn't block an unrelated edit.
    Edit { volume: &'a str },
    /// An intent made earlier coming back: publish, Deploy admission, a History
    /// restore or undo, a Branch landing. A Managed Volume fits at or below its fixed
    /// size, since a Deploy accepts a live bound at or above the size it asks for, so
    /// rolling back past a grow still deploys. A Docker Volume, or a change of kind,
    /// must match.
    Replay,
}

/// Refuse `intent` when it gives a Volume storage its fixed storage doesn't fit
/// under `rule`.
pub(crate) fn check_storage(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    intent: &SavedEnvironmentIntent,
    rule: StorageRule<'_>,
) -> Result<(), RpcError> {
    if intent.volumes.is_empty() {
        return Ok(());
    }
    let locked = super::locked_storage(tx, environment)?;
    for volume in &intent.volumes {
        let Some(fixed) = locked.get(&volume.resource_id) else {
            continue;
        };
        let edited = rule
            == StorageRule::Edit {
                volume: &volume.resource_id,
            };
        if fits(fixed, &volume.storage, edited) {
            continue;
        }
        let name = &volume.name;
        return Err(match (fixed, &volume.storage) {
            (
                VolumeKind::Provisioned {
                    maximum_bytes: fixed,
                },
                VolumeKind::Provisioned {
                    maximum_bytes: asked,
                },
            ) if edited => {
                let verb = if asked > fixed { "grow" } else { "shrink" };
                error::conflict(
                    format!(
                        "{name} is deployed at {fixed}; {verb} it with ployz volume set {name} \
                         --size {}",
                        gb(*asked)
                    ),
                    json!({ "volume": name, "storage_locked": true, "recorded": fixed }),
                )
            }
            (
                VolumeKind::Provisioned {
                    maximum_bytes: fixed,
                },
                VolumeKind::Provisioned {
                    maximum_bytes: asked,
                },
            ) => error::conflict(
                format!(
                    "This revision asks {asked} for {name}, which is deployed at {fixed}. Grow it \
                     first with ployz volume set {name} --size {}",
                    gb(*asked)
                ),
                json!({
                    "volume": name, "storage_locked": true, "recorded": fixed, "asked": asked
                }),
            ),
            _ => error::conflict(
                format!(
                    "Volume {name} keeps its storage once a Deploy has been requested; add a new \
                     Volume for other storage"
                ),
                json!({ "volume": name, "storage_locked": true }),
            ),
        });
    }
    Ok(())
}

/// Whether `asked` fits `fixed`: exactly under an `Edit` of this Volume, at or below
/// it under `Replay`; a Docker Volume, or a change of kind, must match either way.
fn fits(fixed: &VolumeKind, asked: &VolumeKind, edited: bool) -> bool {
    match (fixed, asked) {
        (
            VolumeKind::Provisioned {
                maximum_bytes: fixed,
            },
            VolumeKind::Provisioned {
                maximum_bytes: asked,
            },
        ) => {
            if edited {
                asked == fixed
            } else {
                asked <= fixed
            }
        }
        (fixed, asked) => fixed == asked,
    }
}

/// `bytes` as `volume set --size` takes it: the GB number without its unit.
fn gb(bytes: ProvisionedVolumeMaximumBytes) -> String {
    bytes.to_string().trim_end_matches(" GB").to_owned()
}

#[cfg(test)]
mod tests {
    use ployz_core::config::VolumeKind;

    use super::{fits, gb};

    fn managed(gb: u64) -> VolumeKind {
        VolumeKind::Provisioned {
            maximum_bytes: (gb * 1_000_000_000).try_into().unwrap(),
        }
    }

    #[test]
    fn a_managed_volume_fits_exactly_when_edited_and_at_or_below_when_replayed() {
        let docker = VolumeKind::Docker {};
        for (fixed, asked, edited, expected) in [
            (managed(10), managed(10), true, true),
            (managed(10), managed(10), false, true),
            (managed(10), managed(5), true, false),
            (managed(10), managed(5), false, true),
            (managed(10), managed(20), true, false),
            (managed(10), managed(20), false, false),
            (docker, docker, true, true),
            (docker, docker, false, true),
            (docker, managed(10), true, false),
            (docker, managed(10), false, false),
            (managed(10), docker, true, false),
            (managed(10), docker, false, false),
        ] {
            assert_eq!(
                fits(&fixed, &asked, edited),
                expected,
                "fixed {fixed:?}, asked {asked:?}, edited {edited}"
            );
        }
    }

    #[test]
    fn the_size_flag_takes_the_gb_number_alone() {
        assert_eq!(gb(20_000_000_000.try_into().unwrap()), "20");
        assert_eq!(gb(1_500_000_000.try_into().unwrap()), "1.5");
    }
}
