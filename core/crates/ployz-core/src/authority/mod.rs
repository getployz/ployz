//! Statements a customer's own key signs and a Machine verifies for itself.
//!
//! Cloud carries these but cannot mint them: a Machine checks the signature against the
//! Devices it approved, checks every bound field against the request in hand, and keeps
//! its own record of what it already acted on. The first statement is the Service
//! restart approval; joins and other workflows add their own statement types here.

pub mod canonical;
pub mod join;
pub mod keys;
pub mod refusal;
pub mod restart;
pub mod validity;

pub use canonical::{CanonicalError, Timestamp};
pub use join::JoinApproval;
pub use keys::{KeySignature, Purpose, Signed, Signer, SigningPublicKey, Statement};
pub use refusal::AuthorityRefusal;
pub use restart::{RestartApproval, RestartFacts, RestartStep, WorkflowKind};
