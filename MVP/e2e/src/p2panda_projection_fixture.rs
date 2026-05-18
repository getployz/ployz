use mvp_bus::BusSession;
use mvp_identity::NodeId;
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore, PandaFactWriteOutcome};
use mvp_projection::{
    BackendEndpoint, DnsCommitFact, DnsRecordFact, GatewayCommitFact, NodeJoinedFact,
    ProjectionFactPayload, ProjectionIgnoreReason, RouteCommitFact, RouteId, ServiceName,
    ServiceRegistrationFact,
};

use crate::bus_syntax::fact_key;

pub(crate) async fn seed_projection_facts(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
) -> Result<(), String> {
    write_projection_fact(
        store,
        session,
        author,
        "/facts/node/node-1/joined/1",
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    )
    .await?;
    write_projection_fact(
        store,
        session,
        author,
        "/facts/service/web/node-1/registered/1",
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "1.0.0".to_string(),
            endpoint_subject: "service.web.changed".to_string(),
            epoch: 1,
        }),
    )
    .await?;
    write_projection_fact(
        store,
        session,
        author,
        "/facts/routes/route-1",
        ProjectionFactPayload::RouteCommit(RouteCommitFact {
            route_commit_id: "route-1".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["web.example.com".to_string()],
            backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-1"),
                address: "10.0.0.1:8080".to_string(),
            }],
            old_backends_to_drain: vec![BackendEndpoint {
                node_id: NodeId::new("node-old"),
                address: "10.0.0.9:8080".to_string(),
            }],
        }),
    )
    .await?;
    write_projection_fact(
        store,
        session,
        author,
        "/facts/gateway/gateway-1",
        ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
            gateway_commit_id: "gateway-1".to_string(),
            route_commit_id: "route-1".to_string(),
            epoch: 1,
        }),
    )
    .await?;
    write_projection_fact(
        store,
        session,
        author,
        "/facts/dns/dns-1",
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-1".to_string(),
            epoch: 1,
            records: vec![DnsRecordFact {
                name: "web.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::1".to_string(),
                ttl_seconds: 30,
            }],
        }),
    )
    .await
    .map(|_| ())
}

pub(crate) async fn write_projection_fact(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
    key: &str,
    payload: ProjectionFactPayload,
) -> Result<PandaFactWriteOutcome, String> {
    let fact_key = fact_key(key)?;
    let bytes = payload
        .to_fact_bytes()
        .map_err(|error| format!("serialize p2panda projection fact '{key}': {error}"))?;
    store
        .write_fact_payload(session, author, fact_key, bytes.into())
        .await
        .map_err(|error| format!("write p2panda projection fact '{key}': {error}"))
}

pub(crate) fn status_count(
    statuses: &[mvp_projection::ProjectionStatus],
    reason: ProjectionIgnoreReason,
) -> usize {
    statuses
        .iter()
        .find(|status| status.reason == reason)
        .map_or(0, |status| status.count)
}
