//! What the Store's behaviour suites, and ployz-agent-tools', open a Store with.
#![expect(
    clippy::must_use_candidate,
    reason = "test fixtures, each called for what it returns"
)]

pub mod backend;
pub mod shop;
