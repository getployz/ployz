//! What a Machine checks with GitHub itself: that a token GitHub signed says what a rule
//! approves, and which commit a branch points at now.

pub mod head;
pub mod oidc;

#[cfg(test)]
pub(crate) mod test_support;
