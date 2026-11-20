//! What a drain's start of its new copy on the destination says of that Server, read from
//! every send of it, and so what the drain removes after the start failed.

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
    /// `self`, after a send that heard `heard` and was sent again.
    #[must_use]
    pub fn then<Id>(self, heard: &Heard<Id>) -> Self {
        match heard {
            Heard::Unanswered => Self::MaybeActed,
            Heard::Present(_) | Heard::Absent | Heard::Refused | Heard::Unsent => self,
        }
    }
}

/// What the whole start says of the destination: its last answer, read against every send
/// before it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Outcome<Id> {
    /// The Container the answer named. A resend that finds the Container a lost send made
    /// adopts it by its creation key, so this names it too.
    Present(Id),
    /// Nothing of this start is left: the Container it acted on is gone.
    Absent,
    /// Nothing of this start was made: every send was refused or never sent.
    Refused,
    /// The slot may hold this start's Container, or one still coming.
    Unknown,
}

impl<Id> Outcome<Id> {
    /// The outcome of a start whose last send heard `last`. Only `Present` speaks for a send
    /// before it that may have acted unanswered: an answer that made nothing says nothing
    /// of what that send made.
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
