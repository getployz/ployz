mod actor;
mod bus_source;
mod error;
mod facts;
mod model;
mod reducer;
mod snapshot;
mod source;
mod sqlite;

pub use actor::{
    ProjectionActorHandle, ProjectionActorStatus, ProjectionFailureKind, ProjectionFailureStatus,
    ProjectionReport, ProjectionSuccessStatus,
};
pub use bus_source::BusFactSource;
pub use error::{ProjectionError, ProjectionResult};
pub use facts::{
    BackendEndpoint, DnsCommitFact, DnsRecordFact, GatewayCommitFact, NodeJoinedFact,
    NodeRemovalStartedFact, NodeTombstonedFact, ProjectionFactPayload, RouteCommitFact, RouteId,
    ServiceName, ServiceRegistrationFact, ServingCommitFact,
};
pub use model::{
    AcmeHttp01ChallengeKey, AcmeHttp01ChallengeProjection, DnsProjection, DnsRecordProjection,
    GatewayProjection, GatewayRouteProjection, NodeProjection, ProjectionIgnoreReason,
    ProjectionState, ProjectionStatus, RemovingNodeProjection, ServiceProjection,
};
pub use reducer::{payload_matches_key, reduce_facts};
pub use snapshot::{
    DnsSnapshotFile, GatewaySnapshotFile, ProjectionSnapshotWriteReport, SnapshotWriteReport,
    load_dns_snapshot, load_gateway_snapshot, write_projection_snapshots,
};
pub use source::{
    CandidateStatus, FactCandidate, FactKeyClassification, FactKind, FactSource, FactSourceError,
    FactSourceResult, classify_fact_key,
};
pub use sqlite::{ProjectionRowCounts, SqliteProjectionStore};
