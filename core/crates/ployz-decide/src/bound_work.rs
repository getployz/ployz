//! One piece of work a management key started on a Machine, as the daemon's registry keeps
//! it from its bind until evidence says it ended (`ployzd`'s
//! `machine::local_machine::bound`).
//!
//! The work holds a Bound while it acts. A Bound goes with what its holder last [`Seen`] of
//! the work. Only a sighting that [`Seen::ended`] calls evidence lets it go clean; any other
//! leaves a doubt `D`. A Tie lets the work take another Bound until a key change ends it.
//! The registry forgets the entry only once it is [`Entry::settled`], and a key change
//! reports it while it is [`Entry::unconfirmed`], however many key changes later, until a
//! later look that is evidence clears its doubts.

/// What a look at bound work saw of it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Seen {
    /// Docker reported the command exited, with its exit code.
    ExitCode,
    /// The process itself is gone: a reaped child, its pidfd read readable, or its PID and
    /// start time no longer in `/proc`.
    Reaped,
    /// The task that did the work returned and was joined.
    Joined,
    /// Docker confirmed the Container or helper that ran the work removed, stopped or
    /// replaced by a new process, so every process of the old one is gone.
    ContainerGone,
    /// The holder never sent what would start the work.
    NeverSent,
    /// The far end answered the request that was the work.
    Answered,
    /// The work's effect is in place: a look found what it stored. It ended, after the key
    /// change that ended it.
    Landed,
    /// Reported not running with no exit code, or not stored yet while the process that
    /// took it still runs: it may not have started.
    Pending,
    /// The record of the work is gone, as Docker forgets every exec when it restarts.
    Forgotten,
    /// The look itself failed: an error, a timeout, an unreadable `/proc` file.
    Unchecked,
    /// Another holder has the lock the look takes.
    Busy,
}

impl Seen {
    /// Whether this sighting is evidence the work ended: positive, and unable to also mean
    /// that the work has not started yet, that its state was lost, or that the look failed.
    #[must_use]
    pub const fn ended(self) -> bool {
        match self {
            Self::ExitCode
            | Self::Reaped
            | Self::Joined
            | Self::ContainerGone
            | Self::NeverSent
            | Self::Answered
            | Self::Landed => true,
            Self::Pending | Self::Forgotten | Self::Unchecked | Self::Busy => false,
        }
    }

    /// Every sighting, for a model to try each.
    pub const ALL: [Self; 11] = [
        Self::ExitCode,
        Self::Reaped,
        Self::Joined,
        Self::ContainerGone,
        Self::NeverSent,
        Self::Answered,
        Self::Landed,
        Self::Pending,
        Self::Forgotten,
        Self::Unchecked,
        Self::Busy,
    ];
}

/// The facts the registry keeps about one piece of bound work.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Entry<D> {
    ended: bool,
    holders: u32,
    tied: bool,
    doubts: Vec<D>,
}

impl<D> Entry<D> {
    /// Work just bound: one Bound, tied, live.
    #[must_use]
    pub const fn bound() -> Self {
        Self {
            ended: false,
            holders: 1,
            tied: true,
            doubts: Vec::new(),
        }
    }

    /// Take another Bound of the work, refused once a key change ended it.
    pub fn hold(&mut self) -> bool {
        if self.ended || !self.tied {
            return false;
        }
        self.holders += 1;
        true
    }

    /// A Bound goes, its holder having last seen `seen`. Unless that is evidence the work
    /// ended, `doubt` stays.
    pub fn release(&mut self, seen: Seen, doubt: D) {
        self.holders = self.holders.saturating_sub(1);
        if !seen.ended() {
            self.doubts.push(doubt);
        }
    }

    /// The last Tie went, so the work takes no Bound again.
    pub fn untie(&mut self) {
        self.tied = false;
    }

    /// A key change ended the work: whether it was live until now.
    pub fn end(&mut self) -> bool {
        !std::mem::replace(&mut self.ended, true)
    }

    /// A later look saw `seen` of the work each doubt `looked` picks. Only evidence that it
    /// ended clears them: whether it cleared any.
    pub fn confirm(&mut self, seen: Seen, mut looked: impl FnMut(&D) -> bool) -> bool {
        if !seen.ended() {
            return false;
        }
        let before = self.doubts.len();
        self.doubts.retain(|doubt| !looked(doubt));
        self.doubts.len() != before
    }

    #[must_use]
    pub fn is_ended(&self) -> bool {
        self.ended
    }

    #[must_use]
    pub fn holders(&self) -> u32 {
        self.holders
    }

    #[must_use]
    pub fn doubts(&self) -> &[D] {
        &self.doubts
    }

    pub fn doubts_mut(&mut self) -> &mut [D] {
        &mut self.doubts
    }

    /// Nothing holds the work, no doubt stands, and nothing can take it again: the registry
    /// may forget it.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.holders == 0 && self.doubts.is_empty() && (self.ended || !self.tied)
    }

    /// A key change ended the work, and it still runs or went without evidence it ended.
    #[must_use]
    pub fn unconfirmed(&self) -> bool {
        self.ended && !self.settled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_doubt_outlives_the_last_tie_and_a_key_change_until_a_look_clears_it() {
        let mut entry = Entry::bound();
        entry.release(Seen::Unchecked, "unknown");
        entry.untie();
        assert!(!entry.settled());
        assert!(entry.end());
        assert!(entry.unconfirmed());
        assert!(!entry.end(), "a second key change finds it already ended");
        assert!(entry.unconfirmed());
        assert!(entry.confirm(Seen::Reaped, |_| true));
        assert!(entry.settled());
    }

    #[test]
    fn work_that_ends_with_evidence_settles_and_takes_no_bound_once_ended() {
        let mut entry = Entry::bound();
        assert!(entry.hold());
        entry.end();
        assert!(!entry.hold());
        entry.release(Seen::ExitCode, ());
        assert!(entry.unconfirmed(), "one Bound still holds it");
        entry.release(Seen::Joined, ());
        assert!(entry.settled());
    }
}
