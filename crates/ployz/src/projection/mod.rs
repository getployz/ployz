//! Product projection ports.

use crate::error::ProjectionFailure;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductViewKey(String);

impl ProductViewKey {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceWatermark(u64);

impl SourceWatermark {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

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
pub struct ProductView<T> {
    pub value: T,
    pub freshness: ProjectionFreshness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionRead<T> {
    Found(ProductView<T>),
    Missing,
    Stale(ProjectionFreshness),
}

pub trait ProjectionPort<T> {
    fn read_view(&self, key: &ProductViewKey) -> Result<ProjectionRead<T>, ProjectionFailure>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_freshness_is_not_fresh() {
        let freshness = ProjectionFreshness::Unknown;

        assert!(!matches!(freshness, ProjectionFreshness::Fresh(_)));
    }
}
