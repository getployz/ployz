//! What a drain's start of its new copy on the destination says of that Server, read from
//! every send of it, and so what the drain removes after the start failed.

use ployz_core::{ENSURE_PINNED_CONTAINER_CAPABILITY, RpcError};

/// What one send of the start heard back.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Heard<Id> {
    /// An answer naming the Container the Machine acted on.
    Present(Id),
    /// An answer that the Container it acted on is gone, or that it made none.
    Absent,
    /// The Machine's own answer that this send made nothing: a typed refusal, an error the
    /// Machine marked as refused before it acted, or its refusal of the route itself.
    Refused,
    /// The send never reached the Machine: the client refused it before sending, or the
    /// connection refused it first.
    Unsent,
    /// The send may have reached the Machine and nothing says what it did: a lost answer,
    /// a timeout, a cancel before the answer, or any error the Machine didn't mark.
    Unanswered,
}

/// What a send that failed with an error did on the Machine, as far as the error tells.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Failed {
    /// The Machine's own word that it made nothing: an error it marked as refused before
    /// it acted, or its refusal of the pinned route itself.
    Refused,
    /// The send never reached the Machine: it never left this client, or the entry
    /// couldn't dial the Machine.
    Unsent,
    /// The send may have reached the Machine and nothing says what it did.
    Unanswered,
}

impl Failed {
    /// What `error` says its send did. Only the Machine's own word that it acted on nothing,
    /// or a send that never reached it, says nothing was made; any other error may follow
    /// work the Machine did.
    #[must_use]
    pub fn of(error: &RpcError) -> Self {
        if error.reply_lost() {
            Self::Unanswered
        } else if error.never_reached() || error.on_closed_session() || error.undialed() {
            Self::Unsent
        } else if error.never_acted()
            || error
                .refusal()
                .is_some_and(|refusal| refusal.capability == ENSURE_PINNED_CONTAINER_CAPABILITY)
        {
            Self::Refused
        } else {
            Self::Unanswered
        }
    }
}

impl<Id> From<Failed> for Heard<Id> {
    fn from(failed: Failed) -> Self {
        match failed {
            Failed::Refused => Self::Refused,
            Failed::Unsent => Self::Unsent,
            Failed::Unanswered => Self::Unanswered,
        }
    }
}

/// What the start's sends before its last one may have done.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Earlier {
    /// There were none, or each was answered, refused before acting, or never sent.
    #[default]
    Nothing,
    /// One may have reached the Machine and went unanswered, so it may have made a
    /// Container no later answer speaks for.
    MaybeActed,
}

impl Earlier {
    /// `self`, after a send that failed as `failed` and was sent again.
    #[must_use]
    pub fn then(self, failed: Failed) -> Self {
        match failed {
            Failed::Unanswered => Self::MaybeActed,
            Failed::Refused | Failed::Unsent => self,
        }
    }
}

/// What the whole start says of the destination: its last answer, read against every send
/// before it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Outcome<Id> {
    /// The Container the latest answer named, which a failed start removes by this ID. A
    /// resend adopts whatever Container of the same spec holds its creation key: the one a
    /// lost send made while it is still there, or another caller's that replaced it. So this
    /// is the ID the Machine acted on last, not proof of what an earlier send made.
    Present(Id),
    /// Nothing of this start is left: the Container it acted on is gone.
    Absent,
    /// Nothing of this start was made: every send was refused or never sent.
    Refused,
    /// The slot may hold this start's Container, or one still coming.
    Unknown,
}

impl<Id> Outcome<Id> {
    /// The outcome of a start whose last send heard `last`. After a send that may have acted
    /// unanswered, only `Present` stands: it names the Container in the slot the drain
    /// removes by ID. An answer that made nothing says nothing of what that send made.
    #[must_use]
    pub fn of(earlier: Earlier, last: Heard<Id>) -> Self {
        match (earlier, last) {
            (_, Heard::Present(id)) => Self::Present(id),
            (_, Heard::Unanswered) | (Earlier::MaybeActed, _) => Self::Unknown,
            (Earlier::Nothing, Heard::Absent) => Self::Absent,
            (Earlier::Nothing, Heard::Refused | Heard::Unsent) => Self::Refused,
        }
    }
}
