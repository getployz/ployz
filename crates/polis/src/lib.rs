//! Internal distributed-control support primitives for Ployz.
//!
//! Polis owns product-neutral substrate mechanics such as Corrosion access,
//! iroh identity, peer RPC, probes, tickets, deadlines, and membership rows. It
//! must not know Ployz product domains such as deploys, certificates, routes,
//! runtime participants, or volumes.
//!
//! Production rules:
//! - primitives expose typed failures, not display-string contracts;
//! - Corrosion row APIs expose store mechanics, not product services;
//! - external I/O must be deadline-bounded by the adapter using the primitive;
//! - peer and membership substrates stay independent of Ployz product policy.

pub mod corrosion_agent;
pub mod error;
pub mod identity;
pub mod membership;
pub mod peers;
pub mod schema;
pub mod store;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use corrosion_agent::{
    CorrosionAdoption, CorrosionAgentAddresses, CorrosionAgentConfig, CorrosionAgentError,
    CorrosionExit, CorrosionOwnerId, CorrosionShutdown, LocalCorrosionAgent,
};
pub use error::{Error, Result};
pub use identity::IrohEndpointId;
pub use membership::{
    IslandId, MachineRow, MachineRowQuery, MembershipLifecycle, OverlayIp, RowEpoch,
    StoreMachineId, WireGuardPublicKey, membership_replication_schema_sql,
    membership_schema_statements, upsert_machine_statement, verify_membership_replication_schema,
    verify_membership_schema,
};
pub use peers::{
    FakePeerProbe, PLOYZ_PEER_ALPN, PeerEndpoint, PeerError, PeerIdentity, PeerProbe,
    PeerProbeDeadline, PeerProbeReceipt, PeerProbeResult, PeerRpcClient, PeerRpcListener,
    PeerRpcProbe, PeerRuntime, PeerTicket, PeerTicketPath, bind_peer_endpoint, import_ticket,
    issue_endpoint_ticket, issue_ticket, load_or_create_identity,
};
pub use schema::{
    StorePrimaryKey, StoreTableColumn, create_table_sql, schema_statements, select_columns,
    verify_table_schema,
};
pub use store::{
    CorrosionStore, StoreChangeId, StoreChangeType, StoreError, StoreParam, StoreQueryRows,
    StoreResult, StoreRow, StoreStatement, StoreSubscription, StoreSubscriptionEvent, StoreTimeout,
    StoreUpdate, StoreUpdateEvent, StoreValue, TransactionReceipt,
};
#[cfg(any(test, feature = "test-support"))]
pub use test_support::CorrosionAgentFixtureConfig;
#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn crate_has_no_product_modules() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for removed in [
            "authority",
            "claims",
            "external_attempt",
            "facts",
            "operations",
            "projection",
            "deploy",
            "acme",
            "serving",
            "runtime",
            "volume",
        ] {
            assert!(
                !src.join(format!("{removed}.rs")).exists(),
                "{removed}.rs must not re-enter polis"
            );
            assert!(
                !src.join(removed).exists(),
                "{removed}/ must not re-enter polis"
            );
        }
    }
}
