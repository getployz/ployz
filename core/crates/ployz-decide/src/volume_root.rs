//! How a Volume's root reads from its `readonly` and its `ployz:*` marks.

use ployz_core::{PlacedImage, RootMark, Snapshot, VolumeCopy, VolumeOrigin, WriterEpoch};

/// The `ployz:*` marks [`RootCopy::parse`] reads off a root. They narrow which steps can
/// have left it read-only and name none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootMarks {
    /// `ployz:cut`, read only on a read-only root, the one place `parse` looks at it.
    pub cut: bool,
    pub promote: PromoteMark,
}

/// `ployz:promote`, and whether `ployz:seed` is beside it. `ployz:seed` without
/// `ployz:promote` counts for nothing, so it has no place here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromoteMark {
    Unset,
    Set { seed: bool },
}

/// A Volume's root on this Machine, parsed once from its `readonly` and its marks for
/// `Volume.Inspect` and every reply built by ployzd's `VolumeStorage::copy`. Each variant claims
/// only what those show. `seed` is a root Volume.Seed landed that Docker
/// has not adopted: it carries both `ployz:seed` and `ployz:promote`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootCopy {
    /// Writable, whatever its marks.
    Writable { seed: bool },
    /// Read-only with `ployz:cut`. A cut (`copy_cut`) leaves it from that mark until its
    /// rename into the slot, and a promote of a cut copy (`copy_writer`) from its rename
    /// back until `make_writable`'s `readonly=off`. The marks do not tell those two apart.
    Cut { seed: bool },
    /// Read-only with `ployz:promote` and no `ployz:cut`. A promote of a pulled copy
    /// (`copy_writer`) leaves it from its rename until `make_writable`'s `readonly=off`,
    /// and a Seed landing from `Copied::land`'s rename until `Renamed::finish_landing`'s
    /// `readonly=off`. So does a cut (`copy_cut`) of a root Docker has not registered, from
    /// `readonly=on` until `ployz:cut`.
    Promote { seed: bool },
    /// Read-only with neither mark. A cut (`copy_cut`) of a registered root leaves it from
    /// `readonly=on` until `ployz:cut`.
    Unmarked,
}

impl RootCopy {
    /// `switching` is the root's read-only split, ployzd's `Dataset::switching`.
    #[must_use]
    pub fn parse(switching: bool, marks: RootMarks) -> Self {
        let seed = marks.promote == PromoteMark::Set { seed: true };
        match (switching, marks.cut, marks.promote) {
            (false, _, _) => Self::Writable { seed },
            (true, true, _) => Self::Cut { seed },
            (true, false, PromoteMark::Set { .. }) => Self::Promote { seed },
            (true, false, PromoteMark::Unset) => Self::Unmarked,
        }
    }

    /// The root as `Volume.Inspect` and every `SetVolumeCopy` reply carry it.
    #[must_use]
    pub fn view(
        self,
        newest: Option<Snapshot>,
        epoch: WriterEpoch,
        origin: Option<VolumeOrigin>,
        placed: Option<PlacedImage>,
    ) -> VolumeCopy {
        let (readonly, seed, mark) = match self {
            Self::Writable { seed } => (false, seed, None),
            Self::Cut { seed } => (true, seed, Some(RootMark::Cut)),
            Self::Promote { seed } => (true, seed, Some(RootMark::Promote)),
            Self::Unmarked => (true, false, None),
        };
        VolumeCopy::Root {
            readonly,
            newest,
            epoch,
            seed,
            mark,
            origin,
            placed,
        }
    }
}
