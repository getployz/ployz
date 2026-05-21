//! Product-neutral projection substrate.

use crate::Result;
use crate::identity::SourceWatermark;
use crate::records::AuthorizedRecord;

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
pub struct ProjectionInput {
    pub record: AuthorizedRecord,
}

impl ProjectionInput {
    #[must_use]
    pub fn new(record: AuthorizedRecord) -> Self {
        Self { record }
    }
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

pub trait Reducer<View> {
    fn reduce(&self, input: ProjectionInput) -> Result<View>;
}

pub trait ProjectionStore<View> {
    fn read(&self) -> Result<ProjectionRead<View>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionRead<View> {
    Found {
        view: View,
        freshness: ProjectionFreshness,
    },
    Missing,
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
