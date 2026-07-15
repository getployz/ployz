//! KV state-key scaffolding: the shared path renderer and the newtype macro.
//!
//! These keys address records in-process and never cross the JSON wire, so the
//! generated newtypes carry no serde derives.

/// Renders a KV state-key path from its prefix and segments.
pub(crate) fn state_key_path(prefix: &str, segments: &[&str]) -> String {
    let mut path = String::from(prefix);
    for segment in segments {
        path.push('.');
        path.push_str(segment);
    }
    path
}

/// Defines a KV storage-key newtype derived from a single typed id as
/// `"{PREFIX}.{id}"`. Keys with extra path segments, hashing, or wire
/// serialization stay explicit.
macro_rules! id_prefixed_state_key {
    (
        pub struct $name:ident;
        prefix: $prefix:ident;
        fn $ctor:ident(&$idty:ty);
    ) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn $ctor(id: &$idty) -> Self {
                Self($crate::state_key::state_key_path($prefix, &[id.as_str()]))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

pub(crate) use id_prefixed_state_key;
