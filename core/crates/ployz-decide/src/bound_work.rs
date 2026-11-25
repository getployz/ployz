//! One piece of work a management key started on a Machine, as the daemon's registry keeps
//! it from its bind until evidence says it ended (`ployzd`'s
//! `machine::local_machine::bound`).
//!
//! The work holds a Bound while it acts. A Bound goes with evidence that its work ended, or
//! without it, which leaves a doubt `D`. A Tie lets the work take another Bound until a key
//! change ends it. The registry forgets the entry only once it is [`Entry::settled`], and a
//! key change reports it while it is [`Entry::unconfirmed`], however many key changes
//! later, until a later look clears its doubts.

/// The facts the registry keeps about one piece of bound work.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Entry<D> {
    ended: bool,
    holders: u32,
    tied: bool,
    doubts: Vec<D>,
}

/// How a Bound went.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum Release<D> {
    /// Its holder saw the work end: the process was reaped, Docker reported the exec not
    /// running, or the Build's execution returned.
    Ended,
    /// Its holder went without seeing the work end.
    Unconfirmed(D),
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

    pub fn release(&mut self, how: Release<D>) {
        self.holders = self.holders.saturating_sub(1);
        if let Release::Unconfirmed(doubt) = how {
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

    /// A later look saw the work of each doubt `ended` picks end.
    pub fn confirm(&mut self, mut ended: impl FnMut(&D) -> bool) {
        self.doubts.retain(|doubt| !ended(doubt));
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
        entry.release(Release::Unconfirmed("unknown"));
        entry.untie();
        assert!(!entry.settled());
        assert!(entry.end());
        assert!(entry.unconfirmed());
        assert!(!entry.end(), "a second key change finds it already ended");
        assert!(entry.unconfirmed());
        entry.confirm(|_| true);
        assert!(entry.settled());
    }

    #[test]
    fn work_that_ends_with_evidence_settles_and_takes_no_bound_once_ended() {
        let mut entry = Entry::<()>::bound();
        assert!(entry.hold());
        entry.end();
        assert!(!entry.hold());
        entry.release(Release::Ended);
        assert!(entry.unconfirmed(), "one Bound still holds it");
        entry.release(Release::Ended);
        assert!(entry.settled());
    }
}
