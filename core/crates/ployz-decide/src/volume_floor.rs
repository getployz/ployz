//! A Volume's floor on one Machine: the newest lane gen it accepted. A lower gen is refused,
//! so a step from a run Cloud has since replaced lands nowhere.

use ployz_core::VolumeGen;

/// Whether `gen` passes `floor`, leaving the floor where it is.
///
/// # Errors
/// Returns the floor for a `gen` below it.
pub fn check(floor: VolumeGen, r#gen: VolumeGen) -> Result<(), VolumeGen> {
    if r#gen < floor { Err(floor) } else { Ok(()) }
}

/// The floor after `gen` is admitted: it only rises.
///
/// # Errors
/// Returns the floor for a `gen` below it, as [`check`] does.
pub fn admit(floor: VolumeGen, r#gen: VolumeGen) -> Result<VolumeGen, VolumeGen> {
    check(floor, r#gen)?;
    Ok(floor.max(r#gen))
}
