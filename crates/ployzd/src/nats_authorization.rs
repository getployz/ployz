//! Control-owned NATS authorization: the durable authorized principal set,
//! its on-disk recovery evidence, and the per-machine credential mint.
//!
//! Truth model: `/etc/nats/authorized-users.conf` is the core-local durable
//! authority for NATS principals. Machine-add appends or replaces its machine
//! principal through the single writer; revocation returns with the
//! machine-remove operation when it exists.
//!
//! Fencing (ADR-0015): all read-set -> render -> reload -> verify work
//! serializes through one single-writer task owning the file. Concurrent
//! machine-adds queue render requests; no two renders interleave.

mod machine_seed;
mod mint;
mod reload;
mod writer;

pub use machine_seed::{MachineSeedWriteError, write_machine_seed_file};
pub use mint::{
    MachineCredentialMintRuntime, MintOutcome, MintRequest, MintResumeError, MintVerifyEndpoint,
};
pub use reload::{
    NatsReloadEvidence, NatsReloadFailure, NatsReloadOutcome, NatsReloadRunner,
    SignalNatsReloadRunner, SystemctlNatsReloadRunner,
};
pub use writer::{
    AuthorizedUsersFileError, NatsAuthorizationHandle, NatsAuthorizationRuntime, RenderFailure,
    RenderPrepareFailure, RenderedAuthorization,
};
