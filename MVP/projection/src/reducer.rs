use std::collections::BTreeMap;

use mvp_bus::{FactContentHash, FactPayload, IslandId};

use crate::facts::{
    DnsCommitFact, GatewayCommitFact, NodeId, NodeTombstonedFact, ProjectionFactPayload,
    RouteCommitFact, ServiceName, ServingCommitFact,
};
use crate::model::{
    DnsProjection, DnsRecordProjection, GatewayProjection, GatewayRouteProjection, NodeProjection,
    ProjectionIgnoreReason, ProjectionState, ProjectionStatus, ServiceProjection,
};
use crate::source::{CandidateStatus, FactCandidate, FactKind};

pub fn reduce_facts(
    island: &IslandId,
    candidates: &[FactCandidate],
    payloads: &BTreeMap<FactContentHash, FactPayload>,
) -> ProjectionState {
    let mut reducer = Reducer::new(island.clone(), payloads);
    let mut ordered = candidates.to_vec();
    ordered.sort_by(|left, right| {
        (
            left.island(),
            left.key(),
            left.content_hash(),
            left.author(),
            left.epoch(),
        )
            .cmp(&(
                right.island(),
                right.key(),
                right.content_hash(),
                right.author(),
                right.epoch(),
            ))
    });
    for candidate in &ordered {
        reducer.apply(candidate);
    }
    reducer.finish()
}

struct Reducer<'a> {
    state: ProjectionState,
    payloads: &'a BTreeMap<FactContentHash, FactPayload>,
    route_commits: BTreeMap<String, RouteCommitFact>,
    serving_commits: BTreeMap<String, CommitCandidate<ServingCommitFact>>,
    gateway_commits: BTreeMap<String, CommitCandidate<GatewayCommitFact>>,
    dns_commits: BTreeMap<String, CommitCandidate<DnsCommitFact>>,
    node_tombstones: BTreeMap<NodeId, u64>,
    node_conflicts: BTreeMap<NodeId, u64>,
    service_conflicts: BTreeMap<(ServiceName, NodeId), u64>,
    status_counts: BTreeMap<ProjectionIgnoreReason, usize>,
}

impl<'a> Reducer<'a> {
    fn new(island: IslandId, payloads: &'a BTreeMap<FactContentHash, FactPayload>) -> Self {
        Self {
            state: ProjectionState::for_island(island),
            payloads,
            route_commits: BTreeMap::new(),
            serving_commits: BTreeMap::new(),
            gateway_commits: BTreeMap::new(),
            dns_commits: BTreeMap::new(),
            node_tombstones: BTreeMap::new(),
            node_conflicts: BTreeMap::new(),
            service_conflicts: BTreeMap::new(),
            status_counts: BTreeMap::new(),
        }
    }

    fn apply(&mut self, candidate: &FactCandidate) {
        if candidate.island() != &self.state.island {
            self.ignore(ProjectionIgnoreReason::CrossIsland);
            return;
        }
        if let Some(reason) = rejection_reason(candidate.status()) {
            self.ignore(reason);
            return;
        }
        if candidate.kind() == FactKind::Unsupported {
            self.ignore(ProjectionIgnoreReason::UnsupportedFactKind);
            return;
        }
        let Some(payload) = self.payloads.get(candidate.content_hash()) else {
            self.ignore(ProjectionIgnoreReason::MissingPayload);
            return;
        };
        let Ok(payload) = ProjectionFactPayload::from_fact_bytes(payload.as_bytes()) else {
            self.ignore(ProjectionIgnoreReason::MalformedPayload);
            return;
        };
        if !payload_matches_key(candidate, &payload) {
            self.ignore(ProjectionIgnoreReason::MalformedPayload);
            return;
        }
        self.apply_payload(candidate.kind(), candidate.content_hash().clone(), payload);
    }

    fn apply_payload(
        &mut self,
        kind: FactKind,
        content_hash: FactContentHash,
        payload: ProjectionFactPayload,
    ) {
        match (kind, payload) {
            (FactKind::NodeJoined, ProjectionFactPayload::NodeJoined(fact)) => {
                let node = NodeProjection {
                    node_id: fact.node_id,
                    epoch: fact.epoch,
                    overlay_ip: fact.overlay_ip,
                    iroh_endpoint_id: fact.iroh_endpoint_id,
                    wg_public_key: fact.wg_public_key,
                };
                self.apply_node(node);
            }
            (FactKind::NodeTombstoned, ProjectionFactPayload::NodeTombstoned(fact)) => {
                self.apply_node_tombstone(fact);
            }
            (FactKind::ServiceRegistered, ProjectionFactPayload::ServiceRegistered(fact)) => {
                let key = (fact.service.clone(), fact.node_id.clone());
                let service = ServiceProjection {
                    service: fact.service,
                    node_id: fact.node_id,
                    version: fact.version,
                    endpoint_subject: fact.endpoint_subject,
                    epoch: fact.epoch,
                };
                self.apply_service(key, service);
            }
            (FactKind::RouteCommit, ProjectionFactPayload::RouteCommit(fact)) => {
                self.route_commits
                    .insert(fact.route_commit_id.clone(), fact);
            }
            (FactKind::ServingCommit, ProjectionFactPayload::ServingCommit(fact)) => {
                self.serving_commits.insert(
                    fact.serving_commit_id.clone(),
                    CommitCandidate { content_hash, fact },
                );
            }
            (FactKind::GatewayCommit, ProjectionFactPayload::GatewayCommit(fact)) => {
                self.gateway_commits.insert(
                    fact.gateway_commit_id.clone(),
                    CommitCandidate { content_hash, fact },
                );
            }
            (FactKind::DnsCommit, ProjectionFactPayload::DnsCommit(fact)) => {
                self.dns_commits.insert(
                    fact.dns_commit_id.clone(),
                    CommitCandidate { content_hash, fact },
                );
            }
            _ => self.ignore(ProjectionIgnoreReason::MalformedPayload),
        }
    }

    fn finish(mut self) -> ProjectionState {
        self.state.tombstoned_nodes = std::mem::take(&mut self.node_tombstones);
        if let Some(commit) = self.serving_head() {
            self.state.gateway = Some(project_gateway_from_serving(&commit));
            self.state.dns = Some(project_dns_from_serving(commit));
        } else {
            self.state.gateway = self.project_gateway();
            self.state.dns = self.project_dns();
        }
        self.state.statuses = self
            .status_counts
            .into_iter()
            .map(|(reason, count)| ProjectionStatus { reason, count })
            .collect();
        self.state
    }

    fn project_gateway(&mut self) -> Option<GatewayProjection> {
        let commit = self.gateway_head()?;
        let Some(route) = self.route_commits.get(&commit.route_commit_id) else {
            self.ignore(ProjectionIgnoreReason::MissingPayload);
            return None;
        };
        let mut hostnames = route.hostnames.clone();
        hostnames.sort();
        let mut backends = route.backends.clone();
        backends.sort();
        let mut old_backends_to_drain = route.old_backends_to_drain.clone();
        old_backends_to_drain.sort();
        Some(GatewayProjection {
            gateway_commit_id: commit.gateway_commit_id.clone(),
            route_commit_id: commit.route_commit_id.clone(),
            routes: vec![GatewayRouteProjection {
                route_id: route.route_id.clone(),
                hostnames,
                backends,
                old_backends_to_drain,
            }],
        })
    }

    fn project_dns(&mut self) -> Option<DnsProjection> {
        let commit = self.dns_head()?;
        let mut records = commit
            .records
            .clone()
            .into_iter()
            .map(DnsRecordProjection::from)
            .collect::<Vec<_>>();
        records.sort();
        Some(DnsProjection {
            dns_commit_id: commit.dns_commit_id.clone(),
            records,
        })
    }

    fn ignore(&mut self, reason: ProjectionIgnoreReason) {
        *self.status_counts.entry(reason).or_insert(0) += 1;
    }

    fn ignore_many(&mut self, reason: ProjectionIgnoreReason, count: usize) {
        if count == 0 {
            return;
        }
        *self.status_counts.entry(reason).or_insert(0) += count;
    }

    fn apply_node(&mut self, node: NodeProjection) {
        if self.node_tombstones.contains_key(&node.node_id) {
            self.ignore(ProjectionIgnoreReason::Superseded);
            return;
        }

        let conflict_epoch = self.node_conflicts.get(&node.node_id).copied();
        if conflict_epoch.is_some_and(|epoch| epoch > node.epoch) {
            return;
        }
        if conflict_epoch.is_some_and(|epoch| epoch == node.epoch) {
            self.ignore(ProjectionIgnoreReason::Conflict);
            return;
        }
        if conflict_epoch.is_some_and(|epoch| epoch < node.epoch) {
            self.node_conflicts.remove(&node.node_id);
        }

        match self.state.nodes.get(&node.node_id) {
            Some(existing) if existing.epoch > node.epoch => {}
            Some(existing) if existing.epoch == node.epoch && existing != &node => {
                let node_id = node.node_id.clone();
                self.state.nodes.remove(&node_id);
                self.node_conflicts.insert(node_id, node.epoch);
                self.ignore(ProjectionIgnoreReason::Conflict);
            }
            Some(existing) if existing.epoch == node.epoch => {}
            _ => {
                self.state.nodes.insert(node.node_id.clone(), node);
            }
        }
    }

    fn apply_node_tombstone(&mut self, tombstone: NodeTombstonedFact) {
        let existing_tombstone_epoch = self.node_tombstones.get(&tombstone.node_id).copied();
        if existing_tombstone_epoch.is_some_and(|epoch| epoch > tombstone.epoch) {
            self.ignore(ProjectionIgnoreReason::Superseded);
            return;
        }
        self.node_tombstones
            .insert(tombstone.node_id.clone(), tombstone.epoch);
        self.node_conflicts.remove(&tombstone.node_id);

        if self.state.nodes.remove(&tombstone.node_id).is_some() {
            self.ignore(ProjectionIgnoreReason::Superseded);
        }
        let services_before = self.state.services.len();
        self.state
            .services
            .retain(|(_service, node_id), _projection| node_id != &tombstone.node_id);
        let removed_services = services_before.saturating_sub(self.state.services.len());
        self.ignore_many(ProjectionIgnoreReason::Superseded, removed_services);
    }

    fn apply_service(&mut self, key: (ServiceName, NodeId), service: ServiceProjection) {
        if self.node_tombstones.contains_key(&service.node_id) {
            self.ignore(ProjectionIgnoreReason::Superseded);
            return;
        }

        let conflict_epoch = self.service_conflicts.get(&key).copied();
        if conflict_epoch.is_some_and(|epoch| epoch > service.epoch) {
            return;
        }
        if conflict_epoch.is_some_and(|epoch| epoch == service.epoch) {
            self.ignore(ProjectionIgnoreReason::Conflict);
            return;
        }
        if conflict_epoch.is_some_and(|epoch| epoch < service.epoch) {
            self.service_conflicts.remove(&key);
        }

        match self.state.services.get(&key) {
            Some(existing) if existing.epoch > service.epoch => {}
            Some(existing) if existing.epoch == service.epoch && existing != &service => {
                self.state.services.remove(&key);
                self.service_conflicts.insert(key, service.epoch);
                self.ignore(ProjectionIgnoreReason::Conflict);
            }
            Some(existing) if existing.epoch == service.epoch => {}
            _ => {
                self.state.services.insert(key, service);
            }
        }
    }

    fn gateway_head(&mut self) -> Option<GatewayCommitFact> {
        let selection = select_head(
            self.gateway_commits.values(),
            |fact| fact.epoch,
            |fact| fact.gateway_commit_id.as_str(),
        )?;
        self.ignore_many(ProjectionIgnoreReason::Superseded, selection.superseded);
        Some(selection.fact)
    }

    fn dns_head(&mut self) -> Option<DnsCommitFact> {
        let selection = select_head(
            self.dns_commits.values(),
            |fact| fact.epoch,
            |fact| fact.dns_commit_id.as_str(),
        )?;
        self.ignore_many(ProjectionIgnoreReason::Superseded, selection.superseded);
        Some(selection.fact)
    }

    fn serving_head(&mut self) -> Option<ServingCommitFact> {
        let selection = select_head(
            self.serving_commits.values(),
            |fact| fact.epoch,
            |fact| fact.serving_commit_id.as_str(),
        )?;
        self.ignore_many(ProjectionIgnoreReason::Superseded, selection.superseded);
        Some(selection.fact)
    }
}

fn project_gateway_from_serving(commit: &ServingCommitFact) -> GatewayProjection {
    let mut hostnames = commit.hostnames.clone();
    hostnames.sort();
    let mut backends = commit.backends.clone();
    backends.sort();
    let mut old_backends_to_drain = commit.old_backends_to_drain.clone();
    old_backends_to_drain.sort();
    GatewayProjection {
        gateway_commit_id: commit.gateway_commit_id.clone(),
        route_commit_id: commit.route_commit_id.clone(),
        routes: vec![GatewayRouteProjection {
            route_id: commit.route_id.clone(),
            hostnames,
            backends,
            old_backends_to_drain,
        }],
    }
}

fn project_dns_from_serving(commit: ServingCommitFact) -> DnsProjection {
    let mut records = commit
        .dns_records
        .into_iter()
        .map(DnsRecordProjection::from)
        .collect::<Vec<_>>();
    records.sort();
    DnsProjection {
        dns_commit_id: commit.dns_commit_id,
        records,
    }
}

#[derive(Debug, Clone)]
struct CommitCandidate<T> {
    content_hash: FactContentHash,
    fact: T,
}

#[derive(Debug, Clone)]
struct HeadSelection<T> {
    fact: T,
    superseded: usize,
}

fn select_head<'a, T, I, Epoch, Id>(candidates: I, epoch: Epoch, id: Id) -> Option<HeadSelection<T>>
where
    T: Clone + 'a,
    I: IntoIterator<Item = &'a CommitCandidate<T>>,
    Epoch: Fn(&T) -> u64,
    Id: Fn(&T) -> &str,
{
    let mut selected: Option<&CommitCandidate<T>> = None;
    let mut selected_epoch = 0;
    let mut candidate_count: usize = 0;
    for candidate in candidates {
        candidate_count += 1;
        let candidate_epoch = epoch(&candidate.fact);
        if selected.is_none() || candidate_epoch > selected_epoch {
            selected = Some(candidate);
            selected_epoch = candidate_epoch;
            continue;
        }
        if candidate_epoch == selected_epoch {
            let Some(current) = selected else {
                selected = Some(candidate);
                continue;
            };
            if compare_commit_candidates(candidate, current, &id).is_lt() {
                selected = Some(candidate);
            }
        }
    }
    selected.map(|candidate| HeadSelection {
        fact: candidate.fact.clone(),
        superseded: candidate_count.saturating_sub(1),
    })
}

fn compare_commit_candidates<T, Id>(
    left: &CommitCandidate<T>,
    right: &CommitCandidate<T>,
    id: &Id,
) -> std::cmp::Ordering
where
    Id: Fn(&T) -> &str,
{
    left.content_hash
        .cmp(&right.content_hash)
        .then_with(|| id(&left.fact).cmp(id(&right.fact)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyExpectation {
    NodeJoined {
        node_id: String,
        epoch: u64,
    },
    NodeTombstoned {
        node_id: String,
        epoch: u64,
    },
    ServiceRegistered {
        service: String,
        node_id: String,
        epoch: u64,
    },
    RouteCommit {
        route_commit_id: String,
    },
    ServingCommit {
        serving_commit_id: String,
    },
    GatewayCommit {
        gateway_commit_id: String,
    },
    DnsCommit {
        dns_commit_id: String,
    },
}

fn payload_matches_key(candidate: &FactCandidate, payload: &ProjectionFactPayload) -> bool {
    match (key_expectation(candidate), candidate.kind(), payload) {
        (
            Some(KeyExpectation::NodeJoined { node_id, epoch }),
            FactKind::NodeJoined,
            ProjectionFactPayload::NodeJoined(fact),
        ) => fact.node_id.as_str() == node_id && fact.epoch == epoch,
        (
            Some(KeyExpectation::NodeTombstoned { node_id, epoch }),
            FactKind::NodeTombstoned,
            ProjectionFactPayload::NodeTombstoned(fact),
        ) => fact.node_id.as_str() == node_id && fact.epoch == epoch,
        (
            Some(KeyExpectation::ServiceRegistered {
                service,
                node_id,
                epoch,
            }),
            FactKind::ServiceRegistered,
            ProjectionFactPayload::ServiceRegistered(fact),
        ) => {
            fact.service.as_str() == service
                && fact.node_id.as_str() == node_id
                && fact.epoch == epoch
        }
        (
            Some(KeyExpectation::RouteCommit { route_commit_id }),
            FactKind::RouteCommit,
            ProjectionFactPayload::RouteCommit(fact),
        ) => fact.route_commit_id == route_commit_id,
        (
            Some(KeyExpectation::ServingCommit { serving_commit_id }),
            FactKind::ServingCommit,
            ProjectionFactPayload::ServingCommit(fact),
        ) => fact.serving_commit_id == serving_commit_id,
        (
            Some(KeyExpectation::GatewayCommit { gateway_commit_id }),
            FactKind::GatewayCommit,
            ProjectionFactPayload::GatewayCommit(fact),
        ) => fact.gateway_commit_id == gateway_commit_id,
        (
            Some(KeyExpectation::DnsCommit { dns_commit_id }),
            FactKind::DnsCommit,
            ProjectionFactPayload::DnsCommit(fact),
        ) => fact.dns_commit_id == dns_commit_id,
        _ => false,
    }
}

fn key_expectation(candidate: &FactCandidate) -> Option<KeyExpectation> {
    let segments = candidate.key().segments().collect::<Vec<_>>();
    match segments.as_slice() {
        ["facts", "node", node_id, "joined", epoch]
        | ["facts", "node", node_id, "joined", epoch, _] => Some(KeyExpectation::NodeJoined {
            node_id: (*node_id).to_string(),
            epoch: epoch.parse().ok()?,
        }),
        ["facts", "node", node_id, "tombstoned", epoch]
        | ["facts", "node", node_id, "tombstoned", epoch, _] => {
            Some(KeyExpectation::NodeTombstoned {
                node_id: (*node_id).to_string(),
                epoch: epoch.parse().ok()?,
            })
        }
        ["facts", "service", service, node_id, "registered", epoch]
        | ["facts", "service", service, node_id, "registered", epoch, _] => {
            Some(KeyExpectation::ServiceRegistered {
                service: (*service).to_string(),
                node_id: (*node_id).to_string(),
                epoch: epoch.parse().ok()?,
            })
        }
        ["facts", "routes", route_commit_id] => Some(KeyExpectation::RouteCommit {
            route_commit_id: (*route_commit_id).to_string(),
        }),
        ["facts", "serving", serving_commit_id] => Some(KeyExpectation::ServingCommit {
            serving_commit_id: (*serving_commit_id).to_string(),
        }),
        ["facts", "gateway", gateway_commit_id] => Some(KeyExpectation::GatewayCommit {
            gateway_commit_id: (*gateway_commit_id).to_string(),
        }),
        ["facts", "dns", dns_commit_id] => Some(KeyExpectation::DnsCommit {
            dns_commit_id: (*dns_commit_id).to_string(),
        }),
        _ => None,
    }
}

fn rejection_reason(status: CandidateStatus) -> Option<ProjectionIgnoreReason> {
    match status {
        CandidateStatus::Verified => None,
        CandidateStatus::Unverified => Some(ProjectionIgnoreReason::Unverified),
        CandidateStatus::Unauthorized => Some(ProjectionIgnoreReason::Unauthorized),
        CandidateStatus::CrossIsland => Some(ProjectionIgnoreReason::CrossIsland),
        CandidateStatus::Conflict => Some(ProjectionIgnoreReason::Conflict),
    }
}

#[cfg(test)]
mod tests {
    use super::reduce_facts;
    use crate::facts::{
        BackendEndpoint, DnsCommitFact, DnsRecordFact, GatewayCommitFact, NodeId, NodeJoinedFact,
        NodeTombstonedFact, ProjectionFactPayload, RouteCommitFact, RouteId, ServiceName,
        ServiceRegistrationFact, ServingCommitFact,
    };
    use crate::model::{ProjectionIgnoreReason, ProjectionState};
    use crate::source::{CandidateStatus, FactCandidate, FactKind};
    use mvp_bus::{FactContentHash, FactKey, FactPayload, IslandId, PrincipalId};
    use std::collections::BTreeMap;

    fn island(value: &str) -> IslandId {
        IslandId::new(value)
    }

    fn principal(value: &str) -> PrincipalId {
        PrincipalId::new(value)
    }

    fn key(value: &str) -> FactKey {
        FactKey::parse(value).expect("fact key parses")
    }

    fn insert_payload(
        payloads: &mut BTreeMap<FactContentHash, FactPayload>,
        payload: ProjectionFactPayload,
    ) -> FactContentHash {
        let bytes = payload.to_fact_bytes().expect("payload serializes");
        let fact_payload: FactPayload = bytes.into();
        let hash = FactContentHash::for_payload(&fact_payload);
        payloads.insert(hash.clone(), fact_payload);
        hash
    }

    fn candidate(key_value: &str, kind: FactKind, hash: FactContentHash) -> FactCandidate {
        FactCandidate::verified(
            island("prod"),
            key(key_value),
            principal("writer"),
            hash,
            kind,
            0,
        )
    }

    fn status_count(state: &ProjectionState, reason: ProjectionIgnoreReason) -> usize {
        state
            .statuses
            .iter()
            .find(|status| status.reason == reason)
            .map_or(0, |status| status.count)
    }

    #[test]
    fn reducer_projects_node_service_gateway_and_dns_state() {
        let mut payloads = BTreeMap::new();
        let node_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 1,
                overlay_ip: "fd00::1".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let service_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
                service: ServiceName::new("web"),
                node_id: NodeId::new("node-1"),
                version: "1.0.0".to_string(),
                endpoint_subject: "node.node-1.rpc.inspect".to_string(),
                epoch: 1,
            }),
        );
        let route_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::RouteCommit(RouteCommitFact {
                route_commit_id: "route-1".to_string(),
                route_id: RouteId::new("web-http"),
                hostnames: vec!["b.example.com".to_string(), "a.example.com".to_string()],
                backends: vec![BackendEndpoint {
                    node_id: NodeId::new("node-1"),
                    address: "10.0.0.1:8080".to_string(),
                }],
                old_backends_to_drain: Vec::new(),
            }),
        );
        let gateway_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
                gateway_commit_id: "gateway-1".to_string(),
                route_commit_id: "route-1".to_string(),
                epoch: 1,
            }),
        );
        let dns_hash = insert_payload(
            &mut payloads,
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
        );
        let candidates = vec![
            candidate("/facts/dns/dns-1", FactKind::DnsCommit, dns_hash),
            candidate(
                "/facts/service/web/node-1/registered/1",
                FactKind::ServiceRegistered,
                service_hash,
            ),
            candidate("/facts/routes/route-1", FactKind::RouteCommit, route_hash),
            candidate(
                "/facts/gateway/gateway-1",
                FactKind::GatewayCommit,
                gateway_hash,
            ),
            candidate(
                "/facts/node/node-1/joined/1",
                FactKind::NodeJoined,
                node_hash,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert_eq!(state.nodes.len(), 1);
        assert_eq!(state.services.len(), 1);
        assert_eq!(
            state.gateway.as_ref().expect("gateway projects").routes[0].hostnames,
            vec!["a.example.com", "b.example.com"]
        );
        assert_eq!(
            state.dns.as_ref().expect("dns projects").records[0].value,
            "fd00::1"
        );
        assert!(state.statuses.is_empty());
    }

    #[test]
    fn reducer_is_deterministic_for_shuffled_candidates() {
        let mut payloads = BTreeMap::new();
        let one = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 1,
                overlay_ip: "fd00::1".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let two = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-2"),
                epoch: 1,
                overlay_ip: "fd00::2".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let ordered = vec![
            candidate(
                "/facts/node/node-1/joined/1",
                FactKind::NodeJoined,
                one.clone(),
            ),
            candidate(
                "/facts/node/node-2/joined/1",
                FactKind::NodeJoined,
                two.clone(),
            ),
        ];
        let shuffled = vec![
            candidate("/facts/node/node-2/joined/1", FactKind::NodeJoined, two),
            candidate("/facts/node/node-1/joined/1", FactKind::NodeJoined, one),
        ];

        assert_eq!(
            reduce_facts(&island("prod"), &ordered, &payloads),
            reduce_facts(&island("prod"), &shuffled, &payloads)
        );
    }

    #[test]
    fn reducer_uses_numeric_epochs_for_node_and_service_heads() {
        let mut payloads = BTreeMap::new();
        let node_10 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 10,
                overlay_ip: "fd00::10".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let node_9 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 9,
                overlay_ip: "fd00::9".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let service_10 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
                service: ServiceName::new("web"),
                node_id: NodeId::new("node-1"),
                version: "10.0.0".to_string(),
                endpoint_subject: "node.node-1.web.v10".to_string(),
                epoch: 10,
            }),
        );
        let service_9 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
                service: ServiceName::new("web"),
                node_id: NodeId::new("node-1"),
                version: "9.0.0".to_string(),
                endpoint_subject: "node.node-1.web.v9".to_string(),
                epoch: 9,
            }),
        );
        let candidates = vec![
            candidate("/facts/node/node-1/joined/9", FactKind::NodeJoined, node_9),
            candidate(
                "/facts/node/node-1/joined/10",
                FactKind::NodeJoined,
                node_10,
            ),
            candidate(
                "/facts/service/web/node-1/registered/9",
                FactKind::ServiceRegistered,
                service_9,
            ),
            candidate(
                "/facts/service/web/node-1/registered/10",
                FactKind::ServiceRegistered,
                service_10,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);
        let node = state.nodes.get(&NodeId::new("node-1")).expect("node");
        let service = state
            .services
            .get(&(ServiceName::new("web"), NodeId::new("node-1")))
            .expect("service");

        assert_eq!(node.epoch, 10);
        assert_eq!(node.overlay_ip, "fd00::10");
        assert_eq!(service.epoch, 10);
        assert_eq!(service.version, "10.0.0");
    }

    #[test]
    fn reducer_excludes_tombstoned_nodes_and_services() {
        let mut payloads = BTreeMap::new();
        let node_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 1,
                overlay_ip: "fd00::1".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let service_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
                service: ServiceName::new("web"),
                node_id: NodeId::new("node-1"),
                version: "1.0.0".to_string(),
                endpoint_subject: "node.node-1.web".to_string(),
                epoch: 1,
            }),
        );
        let tombstone_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
                node_id: NodeId::new("node-1"),
                epoch: 1,
                reason: "force-remove".to_string(),
            }),
        );
        let candidates = vec![
            candidate(
                "/facts/node/node-1/joined/1",
                FactKind::NodeJoined,
                node_hash,
            ),
            candidate(
                "/facts/node/node-1/tombstoned/1",
                FactKind::NodeTombstoned,
                tombstone_hash,
            ),
            candidate(
                "/facts/service/web/node-1/registered/1",
                FactKind::ServiceRegistered,
                service_hash,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.nodes.is_empty());
        assert_eq!(state.tombstoned_nodes.get(&NodeId::new("node-1")), Some(&1));
        assert!(state.services.is_empty());
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 2);
    }

    #[test]
    fn reducer_excludes_tombstoned_node_services_even_with_newer_service_epoch() {
        let mut payloads = BTreeMap::new();
        let node_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 1,
                overlay_ip: "fd00::1".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let service_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
                service: ServiceName::new("web"),
                node_id: NodeId::new("node-1"),
                version: "3.0.0".to_string(),
                endpoint_subject: "node.node-1.web".to_string(),
                epoch: 3,
            }),
        );
        let tombstone_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
                node_id: NodeId::new("node-1"),
                epoch: 2,
                reason: "force-remove".to_string(),
            }),
        );
        let candidates = vec![
            candidate(
                "/facts/node/node-1/joined/1",
                FactKind::NodeJoined,
                node_hash,
            ),
            candidate(
                "/facts/service/web/node-1/registered/3",
                FactKind::ServiceRegistered,
                service_hash,
            ),
            candidate(
                "/facts/node/node-1/tombstoned/2",
                FactKind::NodeTombstoned,
                tombstone_hash,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.nodes.is_empty());
        assert_eq!(state.tombstoned_nodes.get(&NodeId::new("node-1")), Some(&2));
        assert!(state.services.is_empty());
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 2);
    }

    #[test]
    fn reducer_keeps_tombstone_until_explicit_reinvite_exists() {
        let mut payloads = BTreeMap::new();
        let node_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 2,
                overlay_ip: "fd00::1".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let tombstone_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
                node_id: NodeId::new("node-1"),
                epoch: 1,
                reason: "old-remove".to_string(),
            }),
        );
        let candidates = vec![
            candidate(
                "/facts/node/node-1/joined/2",
                FactKind::NodeJoined,
                node_hash,
            ),
            candidate(
                "/facts/node/node-1/tombstoned/1",
                FactKind::NodeTombstoned,
                tombstone_hash,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.nodes.is_empty());
        assert_eq!(state.tombstoned_nodes.get(&NodeId::new("node-1")), Some(&1));
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 1);
    }

    #[test]
    fn reducer_uses_commit_epochs_for_gateway_and_dns_heads() {
        let mut payloads = BTreeMap::new();
        let route_9 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::RouteCommit(RouteCommitFact {
                route_commit_id: "route-9".to_string(),
                route_id: RouteId::new("web-http"),
                hostnames: vec!["old.example.com".to_string()],
                backends: Vec::new(),
                old_backends_to_drain: Vec::new(),
            }),
        );
        let route_10 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::RouteCommit(RouteCommitFact {
                route_commit_id: "route-10".to_string(),
                route_id: RouteId::new("web-http"),
                hostnames: vec!["new.example.com".to_string()],
                backends: Vec::new(),
                old_backends_to_drain: Vec::new(),
            }),
        );
        let gateway_9 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
                gateway_commit_id: "gateway-9".to_string(),
                route_commit_id: "route-9".to_string(),
                epoch: 9,
            }),
        );
        let gateway_10 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
                gateway_commit_id: "gateway-10".to_string(),
                route_commit_id: "route-10".to_string(),
                epoch: 10,
            }),
        );
        let dns_9 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::DnsCommit(DnsCommitFact {
                dns_commit_id: "dns-9".to_string(),
                epoch: 9,
                records: vec![DnsRecordFact {
                    name: "web.example.com".to_string(),
                    record_type: "AAAA".to_string(),
                    value: "fd00::9".to_string(),
                    ttl_seconds: 30,
                }],
            }),
        );
        let dns_10 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::DnsCommit(DnsCommitFact {
                dns_commit_id: "dns-10".to_string(),
                epoch: 10,
                records: vec![DnsRecordFact {
                    name: "web.example.com".to_string(),
                    record_type: "AAAA".to_string(),
                    value: "fd00::10".to_string(),
                    ttl_seconds: 30,
                }],
            }),
        );
        let candidates = vec![
            candidate("/facts/routes/route-9", FactKind::RouteCommit, route_9),
            candidate("/facts/routes/route-10", FactKind::RouteCommit, route_10),
            candidate(
                "/facts/gateway/gateway-9",
                FactKind::GatewayCommit,
                gateway_9,
            ),
            candidate(
                "/facts/gateway/gateway-10",
                FactKind::GatewayCommit,
                gateway_10,
            ),
            candidate("/facts/dns/dns-9", FactKind::DnsCommit, dns_9),
            candidate("/facts/dns/dns-10", FactKind::DnsCommit, dns_10),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert_eq!(
            state.gateway.expect("gateway").gateway_commit_id,
            "gateway-10"
        );
        assert_eq!(state.dns.expect("dns").dns_commit_id, "dns-10");
    }

    #[test]
    fn reducer_marks_same_epoch_identity_conflicts_instead_of_picking_by_sort_order() {
        let mut payloads = BTreeMap::new();
        let node_a = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 1,
                overlay_ip: "fd00::1".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let node_b = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 1,
                overlay_ip: "fd00::2".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let service_a = insert_payload(
            &mut payloads,
            ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
                service: ServiceName::new("web"),
                node_id: NodeId::new("node-1"),
                version: "1.0.0".to_string(),
                endpoint_subject: "node.node-1.web.a".to_string(),
                epoch: 1,
            }),
        );
        let service_b = insert_payload(
            &mut payloads,
            ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
                service: ServiceName::new("web"),
                node_id: NodeId::new("node-1"),
                version: "1.0.1".to_string(),
                endpoint_subject: "node.node-1.web.b".to_string(),
                epoch: 1,
            }),
        );
        let candidates = vec![
            candidate(
                "/facts/node/node-1/joined/1/a",
                FactKind::NodeJoined,
                node_a,
            ),
            candidate(
                "/facts/node/node-1/joined/1/b",
                FactKind::NodeJoined,
                node_b,
            ),
            candidate(
                "/facts/service/web/node-1/registered/1/a",
                FactKind::ServiceRegistered,
                service_a,
            ),
            candidate(
                "/facts/service/web/node-1/registered/1/b",
                FactKind::ServiceRegistered,
                service_b,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.nodes.is_empty());
        assert!(state.services.is_empty());
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Conflict), 2);
    }

    #[test]
    fn reducer_supersedes_same_epoch_gateway_and_dns_heads_deterministically() {
        let mut payloads = BTreeMap::new();
        let route_1 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::RouteCommit(RouteCommitFact {
                route_commit_id: "route-1".to_string(),
                route_id: RouteId::new("web-http"),
                hostnames: vec!["one.example.com".to_string()],
                backends: Vec::new(),
                old_backends_to_drain: Vec::new(),
            }),
        );
        let route_2 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::RouteCommit(RouteCommitFact {
                route_commit_id: "route-2".to_string(),
                route_id: RouteId::new("web-http"),
                hostnames: vec!["two.example.com".to_string()],
                backends: Vec::new(),
                old_backends_to_drain: Vec::new(),
            }),
        );
        let gateway_1 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
                gateway_commit_id: "gateway-1".to_string(),
                route_commit_id: "route-1".to_string(),
                epoch: 1,
            }),
        );
        let gateway_2 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
                gateway_commit_id: "gateway-2".to_string(),
                route_commit_id: "route-2".to_string(),
                epoch: 1,
            }),
        );
        let dns_1 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::DnsCommit(DnsCommitFact {
                dns_commit_id: "dns-1".to_string(),
                epoch: 1,
                records: Vec::new(),
            }),
        );
        let dns_2 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::DnsCommit(DnsCommitFact {
                dns_commit_id: "dns-2".to_string(),
                epoch: 1,
                records: Vec::new(),
            }),
        );
        let expected_gateway = if gateway_1 <= gateway_2 {
            ("gateway-1", "one.example.com")
        } else {
            ("gateway-2", "two.example.com")
        };
        let expected_dns = if dns_1 <= dns_2 { "dns-1" } else { "dns-2" };
        let candidates = vec![
            candidate("/facts/routes/route-1", FactKind::RouteCommit, route_1),
            candidate("/facts/routes/route-2", FactKind::RouteCommit, route_2),
            candidate(
                "/facts/gateway/gateway-1",
                FactKind::GatewayCommit,
                gateway_1,
            ),
            candidate(
                "/facts/gateway/gateway-2",
                FactKind::GatewayCommit,
                gateway_2,
            ),
            candidate("/facts/dns/dns-1", FactKind::DnsCommit, dns_1),
            candidate("/facts/dns/dns-2", FactKind::DnsCommit, dns_2),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        let gateway = state.gateway.as_ref().expect("gateway");
        assert_eq!(gateway.gateway_commit_id, expected_gateway.0);
        assert_eq!(gateway.routes[0].hostnames, vec![expected_gateway.1]);
        assert_eq!(state.dns.as_ref().expect("dns").dns_commit_id, expected_dns);
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 2);
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Conflict), 0);
    }

    #[test]
    fn reducer_projects_serving_commit_as_single_gateway_dns_boundary() {
        let mut payloads = BTreeMap::new();
        let serving_1 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::ServingCommit(ServingCommitFact {
                serving_commit_id: "serving-1".to_string(),
                route_commit_id: "route-1".to_string(),
                gateway_commit_id: "gateway-1".to_string(),
                dns_commit_id: "dns-1".to_string(),
                route_id: RouteId::new("web-http"),
                hostnames: vec!["one.example.com".to_string()],
                backends: vec![BackendEndpoint {
                    node_id: NodeId::new("node-1"),
                    address: "fd00::1:8080".to_string(),
                }],
                old_backends_to_drain: Vec::new(),
                dns_records: vec![DnsRecordFact {
                    name: "one.example.com".to_string(),
                    record_type: "AAAA".to_string(),
                    value: "fd00::1".to_string(),
                    ttl_seconds: 30,
                }],
                epoch: 1,
            }),
        );
        let serving_2 = insert_payload(
            &mut payloads,
            ProjectionFactPayload::ServingCommit(ServingCommitFact {
                serving_commit_id: "serving-2".to_string(),
                route_commit_id: "route-2".to_string(),
                gateway_commit_id: "gateway-2".to_string(),
                dns_commit_id: "dns-2".to_string(),
                route_id: RouteId::new("web-http"),
                hostnames: vec!["two.example.com".to_string()],
                backends: vec![BackendEndpoint {
                    node_id: NodeId::new("node-2"),
                    address: "fd00::2:8080".to_string(),
                }],
                old_backends_to_drain: Vec::new(),
                dns_records: vec![DnsRecordFact {
                    name: "two.example.com".to_string(),
                    record_type: "AAAA".to_string(),
                    value: "fd00::2".to_string(),
                    ttl_seconds: 30,
                }],
                epoch: 1,
            }),
        );
        let expected = if serving_1 <= serving_2 {
            ("gateway-1", "dns-1", "one.example.com", "fd00::1")
        } else {
            ("gateway-2", "dns-2", "two.example.com", "fd00::2")
        };
        let candidates = vec![
            candidate(
                "/facts/serving/serving-1",
                FactKind::ServingCommit,
                serving_1,
            ),
            candidate(
                "/facts/serving/serving-2",
                FactKind::ServingCommit,
                serving_2,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        let gateway = state.gateway.as_ref().expect("gateway");
        let dns = state.dns.as_ref().expect("dns");
        assert_eq!(gateway.gateway_commit_id, expected.0);
        assert_eq!(gateway.routes[0].hostnames, vec![expected.2]);
        assert_eq!(dns.dns_commit_id, expected.1);
        assert_eq!(dns.records[0].value, expected.3);
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 1);
    }

    #[test]
    fn reducer_rejects_payload_identity_that_does_not_match_fact_key() {
        let mut payloads = BTreeMap::new();
        let node_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-2"),
                epoch: 1,
                overlay_ip: "fd00::2".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let service_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
                service: ServiceName::new("queue"),
                node_id: NodeId::new("node-1"),
                version: "1.0.0".to_string(),
                endpoint_subject: "node.node-1.queue".to_string(),
                epoch: 1,
            }),
        );
        let route_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::RouteCommit(RouteCommitFact {
                route_commit_id: "route-2".to_string(),
                route_id: RouteId::new("web-http"),
                hostnames: Vec::new(),
                backends: Vec::new(),
                old_backends_to_drain: Vec::new(),
            }),
        );
        let candidates = vec![
            candidate(
                "/facts/node/node-1/joined/1",
                FactKind::NodeJoined,
                node_hash,
            ),
            candidate(
                "/facts/service/web/node-1/registered/1",
                FactKind::ServiceRegistered,
                service_hash,
            ),
            candidate("/facts/routes/route-1", FactKind::RouteCommit, route_hash),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.nodes.is_empty());
        assert!(state.services.is_empty());
        assert!(state.gateway.is_none());
        assert_eq!(
            status_count(&state, ProjectionIgnoreReason::MalformedPayload),
            3
        );
    }

    #[test]
    fn reducer_reports_ignored_candidates_without_raw_metadata() {
        let mut payloads = BTreeMap::new();
        let hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 1,
                overlay_ip: "fd00::1".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let candidates = vec![
            FactCandidate::new(
                island("laptop"),
                key("/facts/node/secret/joined/1"),
                principal("laptop-admin"),
                hash.clone(),
                FactKind::NodeJoined,
                0,
                CandidateStatus::Verified,
            ),
            FactCandidate::new(
                island("prod"),
                key("/facts/node/node-2/joined/1"),
                principal("intruder"),
                hash,
                FactKind::NodeJoined,
                0,
                CandidateStatus::Unauthorized,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.nodes.is_empty());
        assert_eq!(state.statuses.len(), 2);
        assert!(state.statuses.iter().any(|status| {
            status.reason == ProjectionIgnoreReason::CrossIsland && status.count == 1
        }));
        assert!(state.statuses.iter().any(|status| {
            status.reason == ProjectionIgnoreReason::Unauthorized && status.count == 1
        }));
    }

    #[test]
    fn reducer_reports_malformed_payloads() {
        let mut payloads = BTreeMap::new();
        let payload: FactPayload = b"not-json".to_vec().into();
        let hash = FactContentHash::for_payload(&payload);
        payloads.insert(hash.clone(), payload);
        let candidates = vec![candidate(
            "/facts/node/node-1/joined/1",
            FactKind::NodeJoined,
            hash,
        )];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert_eq!(
            state.statuses,
            vec![crate::model::ProjectionStatus {
                reason: ProjectionIgnoreReason::MalformedPayload,
                count: 1,
            }]
        );
    }
}
