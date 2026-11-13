//! What a Machine does with a `SetVolumeSize`, once it holds the Volume and reads its bound:
//! the order its refusals come in, and whether the request shrinks, grows, only raises a
//! copy behind, or changes nothing.

use ployz_core::{ProvisionedVolumeMaximumBytes, SizeRefusal, shrink_floor};

/// What the Machine read about the Volume and what the request asks for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SizeFacts {
    /// The bound the Volume holds.
    pub bound: ProvisionedVolumeMaximumBytes,
    /// The bound the request asks for.
    pub wanted: ProvisionedVolumeMaximumBytes,
    pub allow_shrink: bool,
    /// A Volume Run holds the Volume, or a receive runs into its slot.
    pub switching: bool,
    /// Bytes the dataset holding the data references.
    pub referenced_bytes: u64,
    /// A slot's landed copy carries a bound below `wanted`, so it is raised too.
    pub copy_behind: bool,
}

/// Why the request is refused before anything is written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refused {
    /// The request is below the bound and does not allow a shrink.
    Shrink,
    /// A Volume Run holds the Volume and the request would change its bound.
    Switching,
    /// The request is below the smallest bound its data leaves room for.
    BelowFloor {
        floor: ProvisionedVolumeMaximumBytes,
    },
}

impl Refused {
    #[must_use]
    pub const fn reason(self) -> SizeRefusal {
        match self {
            Self::Shrink => SizeRefusal::Shrink,
            Self::Switching => SizeRefusal::Switching,
            Self::BelowFloor { .. } => SizeRefusal::BelowFloor,
        }
    }
}

/// What to do with the request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SizePlan {
    /// Reply with the bound the Volume holds and write nothing.
    Answer(ProvisionedVolumeMaximumBytes),
    Refuse(Refused),
    /// Lower the bound, and a slot's copy with it.
    Shrink,
    /// Commit the difference to the Pool and raise the bound, then the copy when `raise_copy`.
    Grow {
        raise_copy: bool,
    },
    /// The bound already stands: only raise the copy behind it.
    RaiseCopy,
}

/// Shrink refusals come first, then an equal bound, then a Volume mid-switch, then a grow.
#[must_use]
pub fn size_plan(facts: SizeFacts) -> SizePlan {
    let SizeFacts {
        bound,
        wanted,
        allow_shrink,
        switching,
        referenced_bytes,
        copy_behind,
    } = facts;
    if wanted < bound {
        if !allow_shrink {
            return SizePlan::Refuse(Refused::Shrink);
        }
        if switching {
            return SizePlan::Refuse(Refused::Switching);
        }
        let floor = shrink_floor(referenced_bytes);
        if wanted < floor {
            return SizePlan::Refuse(Refused::BelowFloor { floor });
        }
        return SizePlan::Shrink;
    }
    if wanted == bound && !copy_behind {
        return SizePlan::Answer(bound);
    }
    if switching {
        if wanted == bound {
            return SizePlan::Answer(bound);
        }
        return SizePlan::Refuse(Refused::Switching);
    }
    if wanted > bound {
        return SizePlan::Grow {
            raise_copy: copy_behind,
        };
    }
    SizePlan::RaiseCopy
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    const GB: u64 = 1_000_000_000;

    fn bytes(gb: u64) -> ProvisionedVolumeMaximumBytes {
        ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(gb * GB).unwrap())
    }

    fn facts(bound: u64, wanted: u64) -> SizeFacts {
        SizeFacts {
            bound: bytes(bound),
            wanted: bytes(wanted),
            allow_shrink: false,
            switching: false,
            referenced_bytes: 0,
            copy_behind: false,
        }
    }

    #[test]
    fn a_request_below_the_bound_is_refused_unless_it_allows_a_shrink() {
        assert_eq!(size_plan(facts(4, 2)), SizePlan::Refuse(Refused::Shrink),);
        assert_eq!(
            size_plan(SizeFacts {
                switching: true,
                ..facts(4, 2)
            }),
            SizePlan::Refuse(Refused::Shrink),
        );
    }

    #[test]
    fn a_shrink_that_is_allowed_meets_the_switch_before_the_floor() {
        let shrink = SizeFacts {
            allow_shrink: true,
            referenced_bytes: 2 * GB,
            ..facts(4, 2)
        };
        assert_eq!(
            size_plan(SizeFacts {
                switching: true,
                ..shrink
            }),
            SizePlan::Refuse(Refused::Switching),
        );
        assert_eq!(
            size_plan(shrink),
            SizePlan::Refuse(Refused::BelowFloor {
                floor: shrink_floor(2 * GB)
            }),
        );
    }

    #[test]
    fn a_shrink_to_the_floor_or_above_lowers_the_bound() {
        let shrink = SizeFacts {
            allow_shrink: true,
            ..facts(4, 2)
        };
        assert_eq!(size_plan(shrink), SizePlan::Shrink);
        let at_floor = SizeFacts {
            referenced_bytes: GB,
            ..shrink
        };
        assert_eq!(shrink_floor(at_floor.referenced_bytes), bytes(2));
        assert_eq!(size_plan(at_floor), SizePlan::Shrink);
    }

    #[test]
    fn an_equal_bound_answers_with_no_write_even_mid_switch() {
        assert_eq!(size_plan(facts(4, 4)), SizePlan::Answer(bytes(4)));
        assert_eq!(
            size_plan(SizeFacts {
                switching: true,
                ..facts(4, 4)
            }),
            SizePlan::Answer(bytes(4)),
        );
        assert_eq!(
            size_plan(SizeFacts {
                switching: true,
                copy_behind: true,
                ..facts(4, 4)
            }),
            SizePlan::Answer(bytes(4)),
        );
    }

    #[test]
    fn an_equal_bound_with_a_copy_behind_raises_the_copy() {
        assert_eq!(
            size_plan(SizeFacts {
                copy_behind: true,
                ..facts(4, 4)
            }),
            SizePlan::RaiseCopy,
        );
    }

    #[test]
    fn a_grow_is_refused_mid_switch_and_otherwise_commits() {
        assert_eq!(
            size_plan(SizeFacts {
                switching: true,
                ..facts(2, 4)
            }),
            SizePlan::Refuse(Refused::Switching),
        );
        assert_eq!(size_plan(facts(2, 4)), SizePlan::Grow { raise_copy: false });
        assert_eq!(
            size_plan(SizeFacts {
                copy_behind: true,
                ..facts(2, 4)
            }),
            SizePlan::Grow { raise_copy: true },
        );
    }

    #[test]
    fn a_refusal_reads_as_its_size_refusal() {
        assert_eq!(Refused::Shrink.reason(), SizeRefusal::Shrink);
        assert_eq!(Refused::Switching.reason(), SizeRefusal::Switching);
        assert_eq!(
            Refused::BelowFloor { floor: bytes(1) }.reason(),
            SizeRefusal::BelowFloor
        );
    }
}
