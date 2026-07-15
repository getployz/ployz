//! Control-owned NATS authorization: durable client grants and internal
//! authorities, their rendered projection, and per-machine credential minting.
//!
//! The core store is durable intent. `/etc/nats/authorized-users.conf` is its
//! rendered projection into the running NATS server.
//!
//! Fencing (ADR-0015): all read-set -> render -> reload -> verify work
//! serializes through one single-writer task owning the file. Concurrent
//! machine-adds queue render requests; no two renders interleave.

mod machine_seed;
mod mint;
mod reload;
mod writer;

pub use machine_seed::{MachineSeedWriteError, write_machine_seed_file};
pub use mint::{MachineCredentialMint, MintRequest, MintResumeError, MintVerifyEndpoint};
pub use reload::{HostNatsReloadRunner, NatsReloadRunner};
#[cfg(test)]
pub use reload::{NatsReloadEvidence, NatsReloadOutcome, SignalNatsReloadRunner};
pub use writer::{
    CredentialMutationChange, CredentialMutationFailure, CredentialMutationRejection,
    NatsAuthorizationHandle, NatsAuthorizationWriter, RenderFailure, RenderPrepareFailure,
};
