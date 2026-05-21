//! Product-neutral projection substrate.

use crate::identity::SourceWatermark;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionFreshness {
    Fresh(SourceWatermark),
    Stale {
        last_seen: SourceWatermark,
        required: SourceWatermark,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSnapshot<View> {
    pub view: View,
    pub freshness: ProjectionFreshness,
}

impl<View> ProjectionSnapshot<View> {
    #[must_use]
    pub fn new(view: View, freshness: ProjectionFreshness) -> Self {
        Self { view, freshness }
    }

    #[must_use]
    pub fn is_fresh(&self) -> bool {
        matches!(self.freshness, ProjectionFreshness::Fresh(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_projection_freshness_is_not_fresh() {
        let freshness = ProjectionFreshness::Unknown;

        assert!(!matches!(freshness, ProjectionFreshness::Fresh(_)));
    }

    #[test]
    fn projection_snapshot_preserves_unknown_freshness() {
        let snapshot = ProjectionSnapshot::new("routes", ProjectionFreshness::Unknown);

        assert!(!snapshot.is_fresh());
    }
}
