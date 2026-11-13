//! What `ployz mcp login` does with the agent token `mcp.json` held before it.

/// Where the previous token lives, against the sign-in this login acts with.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Previous {
    /// The Cloud and Organization of this login's sign-in.
    Same,
    /// A Cloud other than the one this login acts in. Its Organization slug is not compared.
    OtherCloud,
    /// This login's Cloud, an Organization other than the one it acts in.
    OtherOrganization,
}

/// Whether the new token is durable in `mcp.json`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Durability {
    Durable,
    /// The new token is written, but its directory could not be flushed.
    Unflushed,
}

/// Why the previous token is left live.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Why {
    Cloud,
    Organization,
    /// A crash may bring the previous token back into `mcp.json`.
    Unflushed,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Fate {
    Revoke,
    Left(Why),
}

/// The previous token is revoked with this login's sign-in only in the Cloud and
/// Organization that sign-in acts in, and only once the new token is durable.
#[must_use]
pub const fn fate(previous: Previous, saved: Durability) -> Fate {
    match (previous, saved) {
        (Previous::OtherCloud, _) => Fate::Left(Why::Cloud),
        (Previous::OtherOrganization, _) => Fate::Left(Why::Organization),
        (Previous::Same, Durability::Unflushed) => Fate::Left(Why::Unflushed),
        (Previous::Same, Durability::Durable) => Fate::Revoke,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_elsewhere_is_left_whether_or_not_the_new_one_is_durable() {
        for saved in [Durability::Durable, Durability::Unflushed] {
            assert_eq!(fate(Previous::OtherCloud, saved), Fate::Left(Why::Cloud));
            assert_eq!(
                fate(Previous::OtherOrganization, saved),
                Fate::Left(Why::Organization)
            );
        }
    }

    #[test]
    fn a_token_here_is_revoked_only_once_the_new_one_is_durable() {
        assert_eq!(fate(Previous::Same, Durability::Durable), Fate::Revoke);
        assert_eq!(
            fate(Previous::Same, Durability::Unflushed),
            Fate::Left(Why::Unflushed)
        );
    }
}
