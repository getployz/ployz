//! What the Store's behaviour suites, and ployz-agent-tools', open a Store with.
#![expect(
    clippy::must_use_candidate,
    reason = "test fixtures, each called for what it returns"
)]

pub mod backend;
pub mod shop;

/// A registry login for `ghcr.io` as a client sends it sealed, told apart by `tag`. The
/// Store never opens one, so its ciphertext is `tag`'s bytes and nothing opens it.
pub fn sealed_login(tag: &str) -> serde_json::Value {
    serde_json::to_value(login(tag)).expect("a login is JSON")
}

/// The [`ployz_core::SealedLogin`] [`sealed_login`] sends.
pub fn login(tag: &str) -> ployz_core::SealedLogin {
    ployz_core::SealedLogin {
        host: "ghcr.io".into(),
        sealed: ployz_core::Ciphertext::from_bytes(format!("{tag:-<48}").into_bytes()),
    }
}
