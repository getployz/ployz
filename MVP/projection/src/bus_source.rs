use std::collections::BTreeMap;

use mvp_bus::{
    BusSession, Fact, FactContentHash, FactKey, FactKeyPattern, FactPayload, IslandId,
    harness::InMemoryBus,
};

use crate::source::{CandidateStatus, FactCandidate, FactKind, FactSource, FactSourceResult};

#[derive(Clone)]
pub struct BusFactSource {
    bus: InMemoryBus,
}

impl BusFactSource {
    #[must_use]
    pub fn new(bus: InMemoryBus) -> Self {
        Self { bus }
    }
}

impl FactSource for BusFactSource {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        if island != session.island() {
            return Ok(Vec::new());
        }
        let candidates = self
            .bus
            .list_facts(session, pattern)?
            .into_iter()
            .map(fact_to_candidate)
            .collect();
        Ok(candidates)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        if island != session.island() {
            return Ok(BTreeMap::new());
        }
        let requests = candidates
            .iter()
            .map(|candidate| (candidate.key().clone(), candidate.content_hash().clone()))
            .collect::<Vec<_>>();
        Ok(self.bus.read_fact_payloads(session, &requests)?)
    }
}

fn fact_to_candidate(fact: Fact) -> FactCandidate {
    FactCandidate::new(
        fact.island().clone(),
        fact.key().clone(),
        fact.author().clone(),
        fact.content_hash().clone(),
        kind_for_key(fact.key()),
        epoch_for_key(fact.key()),
        CandidateStatus::Verified,
    )
}

fn kind_for_key(key: &FactKey) -> FactKind {
    let segments = key.segments().collect::<Vec<_>>();
    match segments.as_slice() {
        ["facts", "node", _node_id, "joined", _epoch]
        | ["facts", "node", _node_id, "joined", _epoch, _] => FactKind::NodeJoined,
        ["facts", "service", _service, _node_id, "registered", _epoch]
        | [
            "facts",
            "service",
            _service,
            _node_id,
            "registered",
            _epoch,
            _,
        ] => FactKind::ServiceRegistered,
        ["facts", "routes", _route_commit] => FactKind::RouteCommit,
        ["facts", "gateway", _gateway_commit] => FactKind::GatewayCommit,
        ["facts", "dns", _dns_commit] => FactKind::DnsCommit,
        _ => FactKind::Unsupported,
    }
}

fn epoch_for_key(key: &FactKey) -> u64 {
    let segments = key.segments().collect::<Vec<_>>();
    match segments.as_slice() {
        ["facts", "node", _node_id, "joined", epoch]
        | ["facts", "node", _node_id, "joined", epoch, _]
        | ["facts", "service", _, _node_id, "registered", epoch]
        | ["facts", "service", _, _node_id, "registered", epoch, _] => epoch.parse().unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::BusFactSource;
    use crate::facts::{NodeId, NodeJoinedFact, ProjectionFactPayload};
    use crate::source::{FactKind, FactSource};
    use mvp_bus::{
        BusAuthority, FactContentHash, FactKey, FactKeyPattern, Grant, IslandId, PrincipalId,
        harness::InMemoryBus,
    };

    fn island(value: &str) -> IslandId {
        IslandId::new(value)
    }

    fn principal(value: &str) -> PrincipalId {
        PrincipalId::new(value)
    }

    fn key(value: &str) -> FactKey {
        FactKey::parse(value).expect("fact key parses")
    }

    fn pattern(value: &str) -> FactKeyPattern {
        FactKeyPattern::parse(value).expect("fact pattern parses")
    }

    fn bus_with_source() -> (InMemoryBus, BusAuthority, BusFactSource) {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let source = BusFactSource::new(bus.clone());
        (bus, authority, source)
    }

    #[test]
    fn bus_source_lists_verified_candidates_and_reads_payloads() {
        let (bus, authority, source) = bus_with_source();
        let session = authority.grant_in(
            island("prod"),
            principal("projection"),
            Grant::empty()
                .with_fact_write(pattern("/facts/node/>"))
                .with_fact_read(pattern("/facts/node/>")),
        );
        let payload = ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
        })
        .to_fact_bytes()
        .expect("payload serializes");
        bus.write_fact_payload(&session, key("/facts/node/node-1/joined/1"), payload)
            .expect("write payload fact");

        let candidates = source
            .list_candidates(&island("prod"), &pattern("/facts/node/>"), &session)
            .expect("list candidates");
        let bodies = source
            .read_payloads(&island("prod"), &candidates, &session)
            .expect("read payloads");
        let body = bodies
            .get(candidates[0].content_hash())
            .expect("payload exists");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].kind(), FactKind::NodeJoined);
        assert_eq!(
            FactContentHash::for_payload(body),
            *candidates[0].content_hash()
        );
    }

    #[test]
    fn bus_source_does_not_cross_islands() {
        let (bus, authority, source) = bus_with_source();
        let prod = authority.grant_in(
            island("prod"),
            principal("projection"),
            Grant::empty()
                .with_fact_write(pattern("/facts/node/>"))
                .with_fact_read(pattern("/facts/node/>")),
        );
        bus.write_fact_payload(
            &prod,
            key("/facts/node/node-1/joined/1"),
            b"{\"kind\":\"node_joined\"}".to_vec(),
        )
        .expect("write payload fact");

        let candidates = source
            .list_candidates(&island("laptop"), &pattern("/facts/node/>"), &prod)
            .expect("list candidates");

        assert!(candidates.is_empty());
    }
}
