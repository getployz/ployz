//! Control-owned NATS authorization: the durable authorized principal set,
//! its on-disk recovery evidence, and the per-machine credential mint.
//!
//! Truth model (ADR-0001 classification): the authorized principal set in
//! `KV_CORE` is durable authority; `/etc/nats/authorized-users.conf` is its
//! recovery evidence and survives JetStream loss. On control start, before
//! any render, the existing file is read and unknown entries are adopted into
//! KV as observations. Renders never shrink the principal set except as a step
//! of an explicit machine-remove operation.
//!
//! Fencing (ADR-0015): all read-set -> render -> reload -> verify work
//! serializes through one single-writer task owning the file. Concurrent
//! machine-adds queue render requests; no two renders interleave.

mod mint;
mod node_seed;
mod reload;
mod tasks;
mod writer;

pub use mint::{MachineCredentialMintRuntime, MintOutcome, MintRequest, MintVerifyEndpoint};
pub use node_seed::{NodeSeedWriteError, write_node_seed_file};
pub use reload::{
    NatsReloadEvidence, NatsReloadOutcome, NatsReloadRunner, SignalNatsReloadRunner,
    SystemctlNatsReloadRunner,
};
pub use tasks::MintTaskRegistry;
pub use writer::{
    NatsAuthorizationError, NatsAuthorizationHandle, NatsAuthorizationRuntime,
    NatsAuthorizationStartError, RenderMode, RenderedAuthorization,
};
