//! Shared role contracts.

pub use crate::machine::roles::*;

/// One supervised process role provided by the `ployzd` artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PloyzdRole {
    Keeper,
    Api,
    Gateway,
    Dns,
}

impl PloyzdRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keeper => "keeper",
            Self::Api => "api",
            Self::Gateway => "gateway",
            Self::Dns => "dns",
        }
    }

    /// The `ployzd` arguments that select this supervised process role.
    #[must_use]
    pub fn argv(self) -> Vec<String> {
        vec![self.as_str().to_owned()]
    }
}
