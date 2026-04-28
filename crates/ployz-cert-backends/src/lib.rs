//! Concrete ACME issuer backend.
//!
//! `ployz-orchestrator` defines the `AcmeIssuer` and `AcmeIssuerFactory`
//! traits along with all coordination logic. This crate provides the
//! `instant-acme`-backed implementation that talks to a real ACME directory
//! (Let's Encrypt, Pebble, etc.) and is wired in by `ployzd`.

mod instant_acme_issuer;

pub use instant_acme_issuer::{InstantAcmeIssuer, InstantAcmeIssuerFactory};
