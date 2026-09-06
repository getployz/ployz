//! Typed Rust evidence for the heterogeneous SDK payload catalog.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

pub(super) trait TaggedEvidence {
    fn examples(&self) -> Vec<Value>;
    fn decodes(&self, value: Value) -> bool;
}

/// One Rust type supplies both serialization examples and deserialization.
pub(super) struct RustEvidence<T>(pub(super) fn() -> Vec<T>);

impl<T: Serialize + DeserializeOwned> TaggedEvidence for RustEvidence<T> {
    fn examples(&self) -> Vec<Value> {
        (self.0)()
            .into_iter()
            .map(|value| serde_json::to_value(value).expect("SDK evidence serializes"))
            .collect()
    }

    fn decodes(&self, value: Value) -> bool {
        serde_json::from_value::<T>(value).is_ok()
    }
}
