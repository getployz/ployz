//! What a Deployment runner may do once the store has answered one of its records: the read
//! of the Deployment, and the latch that keeps the run stopped once a read said the store no
//! longer holds it.

use std::sync::OnceLock;

/// The store's answer to a record of the run, as the facts the runner reads.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Read {
    Running,
    Cancelling,
    Unknown,
    /// The Deployment ended, or another runner owns it. `record` refuses both as a conflict.
    Ended,
    /// The record failed, so the store said nothing.
    Unavailable,
}

/// What one read of the Deployment says this run may do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Standing {
    /// The store holds the run: Running.
    Keep,
    /// A user cancelled the Deployment: Cancelling.
    Cancel,
    Over(Over),
    /// The read failed, so nothing is known: the run goes on and reads again.
    Unread,
}

/// The store no longer holds the run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Over {
    /// The Deployment is Unknown: its lease lapsed, a newer claim took the Environment
    /// over, its runner claimed it again after a preview, or Cloud abandoned it after
    /// one. The store takes only an Interrupted outcome from the run.
    Lost,
    /// The Deployment ended, or another runner owns it: the store takes nothing more
    /// from the run.
    Ended,
}

/// What `read` says the run may do.
#[must_use]
pub const fn standing(read: Read) -> Standing {
    match read {
        Read::Running => Standing::Keep,
        Read::Cancelling => Standing::Cancel,
        Read::Unknown => Standing::Over(Over::Lost),
        Read::Ended => Standing::Over(Over::Ended),
        Read::Unavailable => Standing::Unread,
    }
}

/// What the first read that found the store no longer holding the run said. The store never
/// takes a run back from Unknown or an end, so no later read changes it.
#[derive(Debug, Default)]
pub struct Latch {
    over: OnceLock<Over>,
}

impl Latch {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            over: OnceLock::new(),
        }
    }

    /// A latch that already holds `over`.
    #[must_use]
    pub fn holding(over: Option<Over>) -> Self {
        let latch = Self::new();
        if let Some(over) = over {
            latch.over.get_or_init(|| over);
        }
        latch
    }

    /// What the latch holds. Once it holds something, the run reads nothing more and says
    /// the same: a read that fails after a takeover still leaves the run lost.
    #[must_use]
    pub fn latched(&self) -> Option<Over> {
        self.over.get().copied()
    }

    /// What `read`, made while the latch held nothing, says the run may do. A read that finds
    /// the run over latches it.
    pub fn settle(&self, read: Read) -> Standing {
        let said = standing(read);
        if let Standing::Over(over) = said {
            self.over.get_or_init(|| over);
        }
        said
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_says_what_the_run_may_do() {
        assert_eq!(standing(Read::Running), Standing::Keep);
        assert_eq!(standing(Read::Cancelling), Standing::Cancel);
        assert_eq!(standing(Read::Unknown), Standing::Over(Over::Lost));
        assert_eq!(standing(Read::Ended), Standing::Over(Over::Ended));
        assert_eq!(standing(Read::Unavailable), Standing::Unread);
    }

    #[test]
    fn the_first_read_that_finds_the_run_over_latches_and_no_later_read_changes_it() {
        let latch = Latch::new();
        assert_eq!(latch.latched(), None);
        assert_eq!(latch.settle(Read::Running), Standing::Keep);
        assert_eq!(latch.settle(Read::Unavailable), Standing::Unread);
        assert_eq!(latch.latched(), None);
        assert_eq!(latch.settle(Read::Unknown), Standing::Over(Over::Lost));
        assert_eq!(latch.latched(), Some(Over::Lost));
        assert_eq!(latch.settle(Read::Ended), Standing::Over(Over::Ended));
        assert_eq!(latch.latched(), Some(Over::Lost));
    }

    #[test]
    fn a_cancel_does_not_latch() {
        let latch = Latch::new();
        assert_eq!(latch.settle(Read::Cancelling), Standing::Cancel);
        assert_eq!(latch.latched(), None);
    }

    #[test]
    fn a_latch_can_start_holding_what_a_run_was_left_with() {
        assert_eq!(Latch::holding(None).latched(), None);
        assert_eq!(
            Latch::holding(Some(Over::Ended)).latched(),
            Some(Over::Ended)
        );
    }
}
