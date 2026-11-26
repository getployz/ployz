//! What the Store's behaviour suites, and ployz-agent-tools', open a Store with.
#![expect(
    clippy::must_use_candidate,
    reason = "test fixtures, each called for what it returns"
)]

pub mod backend;
pub mod shop;

/// A registry login for Docker Hub as a client sends it sealed, told apart by `tag`.
/// The Store never opens one, so its ciphertext is `tag`'s bytes and nothing opens it.
pub fn sealed_login(tag: &str) -> serde_json::Value {
    sealed_login_for("docker.io", tag)
}

/// The [`ployz_core::SealedLogin`] [`sealed_login`] sends.
pub fn login(tag: &str) -> ployz_core::SealedLogin {
    login_for("docker.io", tag)
}

/// [`sealed_login`] for the registry at `host`.
pub fn sealed_login_for(host: &str, tag: &str) -> serde_json::Value {
    serde_json::to_value(login_for(host, tag)).expect("a login is JSON")
}

/// The [`ployz_core::SealedLogin`] [`sealed_login_for`] sends.
pub fn login_for(host: &str, tag: &str) -> ployz_core::SealedLogin {
    ployz_core::SealedLogin {
        host: host.into(),
        sealed: ployz_core::Ciphertext::from_bytes(format!("{tag:-<48}").into_bytes()),
    }
}
