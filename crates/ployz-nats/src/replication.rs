//! Shared JetStream replication settings.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationFactor {
    One,
    Three,
    Five,
}

impl ReplicationFactor {
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Three => 3,
            Self::Five => 5,
        }
    }
}
