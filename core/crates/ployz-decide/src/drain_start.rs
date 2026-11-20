//! What a drain removes after its start of the new copy on the destination failed.

/// What the drain heard back from its start of the new copy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Heard<Id> {
    /// An answer naming the Container the Machine acted on.
    Present(Id),
    /// An answer that the Container it acted on is gone, or that it made none.
    Absent,
    /// A refusal: the Machine's typed answer, an error by which it refused before acting,
    /// or the client's own refusal before it sent anything.
    Refused,
    /// Nothing that says what happened: a lost answer, a timeout, a daemon error, or a
    /// cancel before the answer.
    Unanswered,
}

/// What a failed start may have left on the destination.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Leftover<Id> {
    /// The slot may hold this attempt's Container, or one still coming. It is discarded by
    /// the slot's key.
    Unknown,
    /// Nothing of this attempt, so nothing is removed.
    Nothing,
    /// The Container the answer named, removed by its ID.
    Id(Id),
}

/// After any answer only the Container it named is removed. A refusal removes nothing, not
/// even an occupant it reports: the slot's key is the caller's to choose, so that occupant
/// may be another caller's Container. Only a start with no answer discards by key.
#[must_use]
pub fn leftover<Id>(heard: Heard<Id>) -> Leftover<Id> {
    match heard {
        Heard::Present(id) => Leftover::Id(id),
        Heard::Absent | Heard::Refused => Leftover::Nothing,
        Heard::Unanswered => Leftover::Unknown,
    }
}
