use std::collections::BTreeMap;

use mvp_bus::{
    BusSession, Fact, FactContentHash, FactKeyPattern, FactPayload, IslandId, harness::InMemoryBus,
};

use crate::source::{
    CandidateStatus, FactCandidate, FactSource, FactSourceResult, classify_fact_key,
};

#[derive(Clone)]
pub struct BusFactFixtureSource {
    bus: InMemoryBus,
}

impl BusFactFixtureSource {
    #[must_use]
    pub fn new(bus: InMemoryBus) -> Self {
        Self { bus }
    }
}

impl FactSource for BusFactFixtureSource {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        if island != session.island() {
            return Ok(Vec::new());
        }
        let facts = self.bus.list_facts(session, pattern)?;
        let mut counts = BTreeMap::new();
        for fact in &facts {
            *counts
                .entry((fact.island().clone(), fact.key().clone()))
                .or_insert(0usize) += 1;
        }
        let candidates = facts
            .into_iter()
            .map(|fact| {
                let status = if counts[&(fact.island().clone(), fact.key().clone())] > 1 {
                    CandidateStatus::Conflict
                } else {
                    CandidateStatus::Verified
                };
                fact_to_candidate(fact, status)
            })
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

fn fact_to_candidate(fact: Fact, status: CandidateStatus) -> FactCandidate {
    let classification = classify_fact_key(fact.key());
    FactCandidate::new(
        fact.island().clone(),
        fact.key().clone(),
        fact.author().clone(),
        fact.content_hash().clone(),
        classification.kind(),
        classification.epoch(),
        status,
    )
}

#[cfg(test)]
mod tests {
    use super::BusFactFixtureSource;
    use crate::facts::{NodeJoinedFact, ProjectionFactPayload};
    use crate::source::{FactKind, FactSource};
    use mvp_bus::{
        BusAuthority, FactContentHash, FactKey, FactKeyPattern, Grant, IslandId, PrincipalId,
        harness::InMemoryBus,
    };
    use mvp_identity::NodeId;

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

    fn bus_with_source() -> (InMemoryBus, BusAuthority, BusFactFixtureSource) {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let source = BusFactFixtureSource::new(bus.clone());
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
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
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
