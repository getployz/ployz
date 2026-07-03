//! KV state-key scaffolding: the shared path renderer and the newtype macro.
//!
//! Key constructors and their permission wildcard patterns must both render
//! through [`state_key_path`] so a key format and its grant derive from one
//! place. These keys address KV records in-process and never cross the JSON
//! wire, so the generated newtypes carry no serde derives.

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
/// `"{PREFIX}.{id}"`, plus the `wildcard_pattern` spanning every key of that
/// shape. Both render through [`state_key_path`] in the same expansion, so
/// the constructor and the pattern cannot disagree on arity. Keys with extra
/// path segments, hashing, or wire serialization stay explicit.
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

            /// The NATS subject pattern spanning every key of this shape,
            /// rendered by the same helper as the constructor.
            #[must_use]
            pub fn wildcard_pattern() -> String {
                $crate::state_key::state_key_path($prefix, &["*"])
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

pub(crate) use id_prefixed_state_key;
