use std::collections::BTreeMap;

use mvp_acme::{
    AcmeChallengeId, AcmeChallengeToken, AcmeHostname, AcmeHttp01ClearedFact,
    AcmeHttp01PresentedFact,
};
use mvp_bus::{FactContentHash, FactPayload, IslandId, PrincipalId};
use mvp_lease::{
    LeaseClaimed, LeaseContentHash, LeaseEpoch, LeaseFact, LeaseHolder, LeaseRelease,
    LeaseReleased, LeaseRenewed, LeaseResource, LeaseTimestamp,
};

use crate::facts::{
    DnsCommitFact, GatewayCommitFact, NodeRemovalStartedFact, NodeTombstonedFact,
    ProjectionFactPayload, RouteCommitFact, ServiceName, ServingCommitFact,
};
use crate::model::{
    AcmeHttp01ChallengeKey, AcmeHttp01ChallengeProjection, DnsProjection, DnsRecordProjection,
    GatewayProjection, GatewayRouteProjection, NodeProjection, ProjectionIgnoreReason,
    ProjectionState, ProjectionStatus, RemovingNodeProjection, ServiceProjection,
};
use crate::source::{CandidateStatus, FactCandidate, FactKind, is_reducible_conflict_kind};
use mvp_identity::NodeId;

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
    lease_claims: BTreeMap<LeaseResource, Vec<CommitCandidate<LeaseClaimed>>>,
    lease_renewals: BTreeMap<LeaseResource, Vec<CommitCandidate<LeaseRenewed>>>,
    lease_releases: BTreeMap<LeaseResource, Vec<CommitCandidate<LeaseReleased>>>,
    acme_presented: BTreeMap<AcmeHttp01ChallengeKey, Vec<CommitCandidate<AcmeHttp01PresentedFact>>>,
    acme_cleared: BTreeMap<AcmeHttp01ChallengeKey, Vec<CommitCandidate<AcmeHttp01ClearedFact>>>,
    node_removals: BTreeMap<NodeId, Vec<CommitCandidate<NodeRemovalStartedFact>>>,
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
            lease_claims: BTreeMap::new(),
            lease_renewals: BTreeMap::new(),
            lease_releases: BTreeMap::new(),
            acme_presented: BTreeMap::new(),
            acme_cleared: BTreeMap::new(),
            node_removals: BTreeMap::new(),
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
        if let Some(reason) = rejection_reason(candidate) {
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
        self.apply_payload(
            candidate.kind(),
            candidate.author().clone(),
            candidate.content_hash().clone(),
            payload,
        );
    }

    fn apply_payload(
        &mut self,
        kind: FactKind,
        author: PrincipalId,
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
            (FactKind::NodeRemovalStarted, ProjectionFactPayload::NodeRemovalStarted(fact)) => {
                self.node_removals
                    .entry(fact.node_id.clone())
                    .or_default()
                    .push(CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    });
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
                    CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    },
                );
            }
            (FactKind::GatewayCommit, ProjectionFactPayload::GatewayCommit(fact)) => {
                self.gateway_commits.insert(
                    fact.gateway_commit_id.clone(),
                    CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    },
                );
            }
            (FactKind::DnsCommit, ProjectionFactPayload::DnsCommit(fact)) => {
                self.dns_commits.insert(
                    fact.dns_commit_id.clone(),
                    CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    },
                );
            }
            (FactKind::LeaseClaimed, ProjectionFactPayload::LeaseClaimed(fact)) => {
                self.lease_claims
                    .entry(fact.resource().clone())
                    .or_default()
                    .push(CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    });
            }
            (FactKind::LeaseRenewed, ProjectionFactPayload::LeaseRenewed(fact)) => {
                self.lease_renewals
                    .entry(fact.resource().clone())
                    .or_default()
                    .push(CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    });
            }
            (FactKind::LeaseReleased, ProjectionFactPayload::LeaseReleased(fact)) => {
                self.lease_releases
                    .entry(fact.resource().clone())
                    .or_default()
                    .push(CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    });
            }
            (FactKind::AcmeHttp01Presented, ProjectionFactPayload::AcmeHttp01Presented(fact)) => {
                let key = acme_key(fact.id());
                self.acme_presented
                    .entry(key)
                    .or_default()
                    .push(CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    });
            }
            (FactKind::AcmeHttp01Cleared, ProjectionFactPayload::AcmeHttp01Cleared(fact)) => {
                let key = acme_key(fact.id());
                self.acme_cleared
                    .entry(key)
                    .or_default()
                    .push(CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    });
            }
            _ => self.ignore(ProjectionIgnoreReason::MalformedPayload),
        }
    }

    fn finish(mut self) -> ProjectionState {
        self.state.removing_nodes = self.project_node_removals();
        self.state.tombstoned_nodes = std::mem::take(&mut self.node_tombstones);
        self.state.acme_http01 = self.project_acme_http01();
        self.record_lease_supersession();
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

    fn project_acme_http01(
        &mut self,
    ) -> BTreeMap<AcmeHttp01ChallengeKey, AcmeHttp01ChallengeProjection> {
        let mut projected = BTreeMap::new();
        let keys = self.acme_presented.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let Some(presentations) = self.acme_presented.get(&key) else {
                continue;
            };
            let active_lease = presentations.first().and_then(|candidate| {
                self.active_lease_for_resource(candidate.fact.id().lease_resource())
            });
            let (presented, rejected) = {
                let mut rejected = 0;
                let valid_presentations = presentations
                    .iter()
                    .filter(|candidate| {
                        let valid = active_lease.as_ref().is_some_and(|lease| {
                            acme_presentation_matches_current_lease(candidate, lease)
                        });
                        if !valid {
                            rejected += 1;
                        }
                        valid
                    })
                    .collect::<Vec<_>>();
                (
                    select_head(
                        valid_presentations,
                        |fact| fact.epoch().value(),
                        |fact| fact.key_authorization().as_str(),
                    ),
                    rejected,
                )
            };
            self.ignore_many(ProjectionIgnoreReason::Superseded, rejected);
            let Some(presented) = presented else {
                continue;
            };
            self.ignore_many(ProjectionIgnoreReason::Superseded, presented.superseded);
            if self.is_acme_presentation_cleared(&key, &presented) {
                continue;
            }
            let Some(active_lease) = active_lease else {
                continue;
            };
            projected.insert(
                key,
                AcmeHttp01ChallengeProjection {
                    hostname: presented.fact.id().hostname().clone(),
                    token: presented.fact.id().token().clone(),
                    key_authorization: presented.fact.key_authorization().clone(),
                    holder: presented.fact.holder().clone(),
                    lease_epoch: presented.fact.epoch(),
                    claim_hash: presented.fact.claim_hash(),
                    published_at: presented.fact.published_at(),
                    expires_at: active_lease.expires_at,
                },
            );
        }
        projected
    }

    fn is_acme_presentation_cleared(
        &mut self,
        key: &AcmeHttp01ChallengeKey,
        presented: &HeadSelection<AcmeHttp01PresentedFact>,
    ) -> bool {
        let (matching_clear, rejected) = {
            let Some(clears) = self.acme_cleared.get(key) else {
                return false;
            };
            let matching = clears
                .iter()
                .filter(|clear| clear_matches_presentation(clear, presented))
                .collect::<Vec<_>>();
            let rejected = clears.len().saturating_sub(matching.len());
            (
                select_head(
                    matching,
                    |fact| fact.epoch().value(),
                    |fact| fact.holder().as_str(),
                ),
                rejected,
            )
        };
        self.ignore_many(ProjectionIgnoreReason::Superseded, rejected);
        let Some(clear) = matching_clear else {
            return false;
        };
        self.ignore_many(ProjectionIgnoreReason::Superseded, clear.superseded);
        true
    }

    fn active_lease_for_resource(&self, resource: &LeaseResource) -> Option<ActiveLeaseHead> {
        let claims = self.lease_claims.get(resource)?;
        let winner = select_lease_claim_head(claims.iter().filter(|candidate| {
            author_matches_holder(&candidate.author, candidate.fact.holder())
        }))?;
        let claim_hash = lease_claim_hash(&winner.fact);
        let expires_at = self.latest_lease_expiry(resource, &winner.fact, claim_hash);
        if self.has_matching_lease_release(resource, &winner.fact, claim_hash, expires_at) {
            return None;
        }
        Some(ActiveLeaseHead {
            holder: winner.fact.holder().clone(),
            epoch: winner.fact.epoch(),
            claim_hash,
            acquired_at: winner.fact.acquired_at(),
            expires_at,
        })
    }

    fn latest_lease_expiry(
        &self,
        resource: &LeaseResource,
        claim: &LeaseClaimed,
        claim_hash: LeaseContentHash,
    ) -> LeaseTimestamp {
        let mut renewals = self
            .lease_renewals
            .get(resource)
            .into_iter()
            .flat_map(|renewals| renewals.iter())
            .filter(|renewed| {
                author_matches_holder(&renewed.author, renewed.fact.holder())
                    && renewed.fact.holder() == claim.holder()
                    && renewed.fact.epoch() == claim.epoch()
                    && renewed.fact.claim_hash() == claim_hash
            })
            .collect::<Vec<_>>();
        renewals.sort_by_key(|renewed| (renewed.fact.renewed_at(), renewed.content_hash.clone()));

        let mut expires_at = claim.expires_at();
        for renewed in renewals {
            if renewed.fact.renewed_at() >= claim.acquired_at()
                && renewed.fact.renewed_at() < expires_at
                && renewed.fact.expires_at() > renewed.fact.renewed_at()
            {
                expires_at = renewed.fact.expires_at();
            }
        }
        expires_at
    }

    fn has_matching_lease_release(
        &self,
        resource: &LeaseResource,
        claim: &LeaseClaimed,
        claim_hash: LeaseContentHash,
        expires_at: LeaseTimestamp,
    ) -> bool {
        self.lease_releases
            .get(resource)
            .into_iter()
            .flat_map(|releases| releases.iter())
            .any(|released| {
                author_matches_holder(&released.author, released.fact.holder())
                    && released.fact.holder() == claim.holder()
                    && released.fact.epoch() == claim.epoch()
                    && released.fact.claim_hash() == claim_hash
                    && release_applies_to_claim(released.fact.release(), claim, expires_at)
            })
    }

    fn record_lease_supersession(&mut self) {
        let superseded = superseded_count(&self.lease_claims)
            + superseded_count(&self.lease_renewals)
            + superseded_count(&self.lease_releases);
        self.ignore_many(ProjectionIgnoreReason::Superseded, superseded);
    }

    fn project_node_removals(&mut self) -> BTreeMap<NodeId, RemovingNodeProjection> {
        let mut removing = BTreeMap::new();
        let node_ids = self.node_removals.keys().cloned().collect::<Vec<_>>();
        for node_id in node_ids {
            let Some(candidates) = self.node_removals.get(&node_id).cloned() else {
                continue;
            };
            if self.node_tombstones.contains_key(&node_id) {
                self.ignore_many(ProjectionIgnoreReason::Superseded, candidates.len());
                continue;
            }
            let Some(selected) = select_head(
                candidates.iter(),
                |fact| fact.epoch,
                |fact| fact.reason.as_str(),
            ) else {
                continue;
            };
            self.ignore_many(ProjectionIgnoreReason::Superseded, selected.superseded);
            let fact = selected.fact;
            removing.insert(
                fact.node_id.clone(),
                RemovingNodeProjection {
                    node_id: fact.node_id,
                    epoch: fact.epoch,
                    reason: fact.reason,
                },
            );
        }
        removing
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

fn acme_key(id: &AcmeChallengeId) -> AcmeHttp01ChallengeKey {
    AcmeHttp01ChallengeKey::new(id.hostname().clone(), id.token().clone())
}

#[derive(Debug, Clone)]
struct ActiveLeaseHead {
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
    acquired_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
}

#[derive(Debug, Clone)]
struct CommitCandidate<T> {
    author: PrincipalId,
    content_hash: FactContentHash,
    fact: T,
}

#[derive(Debug, Clone)]
struct HeadSelection<T> {
    author: PrincipalId,
    fact: T,
    superseded: usize,
}

fn select_lease_claim_head<'a, I>(candidates: I) -> Option<HeadSelection<LeaseClaimed>>
where
    I: IntoIterator<Item = &'a CommitCandidate<LeaseClaimed>>,
{
    let mut selected: Option<&CommitCandidate<LeaseClaimed>> = None;
    let mut selected_epoch = LeaseEpoch::first();
    let mut selected_claim_hash = None;
    let mut candidate_count: usize = 0;
    for candidate in candidates {
        candidate_count += 1;
        let candidate_epoch = candidate.fact.epoch();
        let candidate_claim_hash = lease_claim_hash(&candidate.fact);
        let replace = match (selected, selected_claim_hash) {
            (None, _) => true,
            (Some(_), Some(_)) if candidate_epoch > selected_epoch => true,
            (Some(current), Some(current_hash)) if candidate_epoch == selected_epoch => {
                candidate_claim_hash
                    .cmp(&current_hash)
                    .then_with(|| candidate.fact.holder().cmp(current.fact.holder()))
                    .is_lt()
            }
            (Some(_), _) => false,
        };
        if replace {
            selected = Some(candidate);
            selected_epoch = candidate_epoch;
            selected_claim_hash = Some(candidate_claim_hash);
        }
    }
    selected.map(|candidate| HeadSelection {
        author: candidate.author.clone(),
        fact: candidate.fact.clone(),
        superseded: candidate_count.saturating_sub(1),
    })
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
        author: candidate.author.clone(),
        fact: candidate.fact.clone(),
        superseded: candidate_count.saturating_sub(1),
    })
}

fn clear_matches_presentation(
    clear: &CommitCandidate<AcmeHttp01ClearedFact>,
    presented: &HeadSelection<AcmeHttp01PresentedFact>,
) -> bool {
    author_matches_holder(&clear.author, clear.fact.holder())
        && clear.author == presented.author
        && clear.fact.epoch() >= presented.fact.epoch()
        && clear.fact.claim_hash() == presented.fact.claim_hash()
        && clear.fact.holder() == presented.fact.holder()
        && clear.fact.cleared_at() >= presented.fact.published_at()
}

fn acme_presentation_matches_current_lease(
    presented: &CommitCandidate<AcmeHttp01PresentedFact>,
    lease: &ActiveLeaseHead,
) -> bool {
    let fact = &presented.fact;
    author_matches_holder(&presented.author, fact.holder())
        && lease.holder == *fact.holder()
        && lease.epoch == fact.epoch()
        && lease.claim_hash == fact.claim_hash()
        && fact.published_at() >= lease.acquired_at
        && fact.published_at() < lease.expires_at
}

fn author_matches_holder(author: &PrincipalId, holder: &LeaseHolder) -> bool {
    author.as_str() == holder.as_str()
}

fn lease_claim_hash(claim: &LeaseClaimed) -> LeaseContentHash {
    LeaseFact::Claimed(claim.clone()).content_hash()
}

fn release_applies_to_claim(
    release: LeaseRelease,
    claim: &LeaseClaimed,
    expires_at: LeaseTimestamp,
) -> bool {
    match release {
        LeaseRelease::DroppedWithoutTimestamp => true,
        LeaseRelease::At(released_at) => {
            released_at >= claim.acquired_at() && released_at < expires_at
        }
    }
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

fn superseded_count<K, T>(candidates: &BTreeMap<K, Vec<CommitCandidate<T>>>) -> usize {
    candidates
        .values()
        .map(|values| values.len().saturating_sub(1))
        .sum()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyExpectation {
    NodeJoined {
        node_id: String,
        epoch: u64,
    },
    NodeRemovalStarted {
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
    LeaseClaimed {
        resource: String,
        epoch: LeaseEpoch,
    },
    LeaseRenewed {
        resource: String,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        renewed_at: LeaseTimestamp,
    },
    LeaseReleased {
        resource: String,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        release: LeaseRelease,
    },
    AcmeHttp01Presented {
        hostname: AcmeHostname,
        token: AcmeChallengeToken,
        epoch: LeaseEpoch,
    },
    AcmeHttp01Cleared {
        hostname: AcmeHostname,
        token: AcmeChallengeToken,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
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
            Some(KeyExpectation::NodeRemovalStarted { node_id, epoch }),
            FactKind::NodeRemovalStarted,
            ProjectionFactPayload::NodeRemovalStarted(fact),
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
        (
            Some(KeyExpectation::LeaseClaimed { resource, epoch }),
            FactKind::LeaseClaimed,
            ProjectionFactPayload::LeaseClaimed(fact),
        ) => fact.resource().as_str() == resource && fact.epoch() == epoch,
        (
            Some(KeyExpectation::LeaseRenewed {
                resource,
                epoch,
                claim_hash,
                renewed_at,
            }),
            FactKind::LeaseRenewed,
            ProjectionFactPayload::LeaseRenewed(fact),
        ) => {
            fact.resource().as_str() == resource
                && fact.epoch() == epoch
                && fact.claim_hash() == claim_hash
                && fact.renewed_at() == renewed_at
        }
        (
            Some(KeyExpectation::LeaseReleased {
                resource,
                epoch,
                claim_hash,
                release,
            }),
            FactKind::LeaseReleased,
            ProjectionFactPayload::LeaseReleased(fact),
        ) => {
            fact.resource().as_str() == resource
                && fact.epoch() == epoch
                && fact.claim_hash() == claim_hash
                && fact.release() == release
        }
        (
            Some(KeyExpectation::AcmeHttp01Presented {
                hostname,
                token,
                epoch,
            }),
            FactKind::AcmeHttp01Presented,
            ProjectionFactPayload::AcmeHttp01Presented(fact),
        ) => {
            fact.id().hostname() == &hostname
                && fact.id().token() == &token
                && fact.epoch() == epoch
        }
        (
            Some(KeyExpectation::AcmeHttp01Cleared {
                hostname,
                token,
                epoch,
                claim_hash,
            }),
            FactKind::AcmeHttp01Cleared,
            ProjectionFactPayload::AcmeHttp01Cleared(fact),
        ) => {
            fact.id().hostname() == &hostname
                && fact.id().token() == &token
                && fact.epoch() == epoch
                && fact.claim_hash() == claim_hash
        }
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
        ["facts", "node", node_id, "removal_started", epoch]
        | ["facts", "node", node_id, "removal_started", epoch, _] => {
            Some(KeyExpectation::NodeRemovalStarted {
                node_id: (*node_id).to_string(),
                epoch: epoch.parse().ok()?,
            })
        }
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
        ["facts", "lease", resource, "claimed", epoch]
        | ["facts", "lease", resource, "claimed", epoch, _] => Some(KeyExpectation::LeaseClaimed {
            resource: (*resource).to_string(),
            epoch: parse_lease_epoch(epoch)?,
        }),
        [
            "facts",
            "lease",
            resource,
            "renewed",
            epoch,
            claim_hash,
            renewed_at,
        ]
        | [
            "facts",
            "lease",
            resource,
            "renewed",
            epoch,
            claim_hash,
            renewed_at,
            _,
        ] => Some(KeyExpectation::LeaseRenewed {
            resource: (*resource).to_string(),
            epoch: parse_lease_epoch(epoch)?,
            claim_hash: LeaseContentHash::from_hex(claim_hash).ok()?,
            renewed_at: parse_lease_timestamp(renewed_at)?,
        }),
        [
            "facts",
            "lease",
            resource,
            "released",
            epoch,
            claim_hash,
            release,
        ]
        | [
            "facts",
            "lease",
            resource,
            "released",
            epoch,
            claim_hash,
            release,
            _,
        ] => Some(KeyExpectation::LeaseReleased {
            resource: (*resource).to_string(),
            epoch: parse_lease_epoch(epoch)?,
            claim_hash: LeaseContentHash::from_hex(claim_hash).ok()?,
            release: parse_lease_release(release)?,
        }),
        [
            "facts",
            "acme",
            "http01",
            hostname,
            token,
            "presented",
            epoch,
        ]
        | [
            "facts",
            "acme",
            "http01",
            hostname,
            token,
            "presented",
            epoch,
            _,
        ] => Some(KeyExpectation::AcmeHttp01Presented {
            hostname: AcmeHostname::parse(*hostname).ok()?,
            token: AcmeChallengeToken::parse(*token).ok()?,
            epoch: parse_lease_epoch(epoch)?,
        }),
        [
            "facts",
            "acme",
            "http01",
            hostname,
            token,
            "cleared",
            epoch,
            claim_hash,
        ]
        | [
            "facts",
            "acme",
            "http01",
            hostname,
            token,
            "cleared",
            epoch,
            claim_hash,
            _,
        ] => Some(KeyExpectation::AcmeHttp01Cleared {
            hostname: AcmeHostname::parse(*hostname).ok()?,
            token: AcmeChallengeToken::parse(*token).ok()?,
            epoch: parse_lease_epoch(epoch)?,
            claim_hash: LeaseContentHash::from_hex(claim_hash).ok()?,
        }),
        _ => None,
    }
}

fn parse_lease_epoch(value: &str) -> Option<LeaseEpoch> {
    LeaseEpoch::from_u64(value.parse().ok()?).ok()
}

fn parse_lease_timestamp(value: &str) -> Option<LeaseTimestamp> {
    Some(LeaseTimestamp::from_secs(value.parse().ok()?))
}

fn parse_lease_release(value: &str) -> Option<LeaseRelease> {
    if value == "drop" {
        return Some(LeaseRelease::DroppedWithoutTimestamp);
    }
    parse_lease_timestamp(value).map(LeaseRelease::At)
}

fn rejection_reason(candidate: &FactCandidate) -> Option<ProjectionIgnoreReason> {
    match candidate.status() {
        CandidateStatus::Verified => None,
        CandidateStatus::Conflict if is_reducible_conflict_kind(candidate.kind()) => None,
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
        BackendEndpoint, DnsCommitFact, DnsRecordFact, GatewayCommitFact, NodeJoinedFact,
        NodeRemovalStartedFact, NodeTombstonedFact, ProjectionFactPayload, RouteCommitFact,
        RouteId, ServiceName, ServiceRegistrationFact, ServingCommitFact,
    };
    use crate::model::{ProjectionIgnoreReason, ProjectionState};
    use crate::source::{CandidateStatus, FactCandidate, FactKind};
    use mvp_acme::{
        AcmeChallengeId, AcmeChallengeToken, AcmeHostname, AcmeHttp01ClearedFact,
        AcmeHttp01PresentedFact, AcmeKeyAuthorization,
    };
    use mvp_bus::{FactContentHash, FactKey, FactPayload, IslandId, PrincipalId};
    use mvp_identity::NodeId;
    use mvp_lease::{
        LeaseClaimed, LeaseEpoch, LeaseFact, LeaseHolder, LeaseReleased, LeaseRenewed,
        LeaseResource, LeaseTimestamp,
    };
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
        candidate_as("writer", key_value, kind, hash)
    }

    fn candidate_as(
        author: &str,
        key_value: &str,
        kind: FactKind,
        hash: FactContentHash,
    ) -> FactCandidate {
        FactCandidate::verified(
            island("prod"),
            key(key_value),
            principal(author),
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

    fn acme_id() -> AcmeChallengeId {
        AcmeChallengeId::new(
            AcmeHostname::parse("example.test").expect("hostname"),
            AcmeChallengeToken::parse("tokAcme0123456789abcdef").expect("token"),
        )
    }

    fn acme_claim(id: &AcmeChallengeId, holder: &str, epoch: LeaseEpoch) -> LeaseClaimed {
        LeaseClaimed::new(
            id.lease_resource().clone(),
            LeaseHolder::new(holder),
            epoch,
            LeaseTimestamp::from_secs(100),
            LeaseTimestamp::from_secs(160),
        )
    }

    fn acme_presented(
        id: AcmeChallengeId,
        holder: &str,
        epoch: LeaseEpoch,
        claim_hash: mvp_lease::LeaseContentHash,
        thumbprint: &str,
    ) -> AcmeHttp01PresentedFact {
        let key_authorization = AcmeKeyAuthorization::parse_for_token(
            id.token(),
            format!("{}.{thumbprint}", id.token().as_str()),
        )
        .expect("key authorization");
        AcmeHttp01PresentedFact::from_parts(
            id,
            key_authorization,
            LeaseHolder::new(holder),
            epoch,
            claim_hash,
            LeaseTimestamp::from_secs(110),
        )
        .expect("presented fact")
    }

    fn acme_cleared(
        id: AcmeChallengeId,
        holder: &str,
        epoch: LeaseEpoch,
        claim_hash: mvp_lease::LeaseContentHash,
        cleared_at: LeaseTimestamp,
    ) -> AcmeHttp01ClearedFact {
        AcmeHttp01ClearedFact::from_parts(
            id,
            LeaseHolder::new(holder),
            epoch,
            claim_hash,
            cleared_at,
        )
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
    fn reducer_projects_removal_started_without_removing_live_state() {
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
        let removal_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeRemovalStarted(NodeRemovalStartedFact {
                node_id: NodeId::new("node-1"),
                epoch: 2,
                reason: "graceful-remove".to_string(),
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
            candidate(
                "/facts/node/node-1/removal_started/2",
                FactKind::NodeRemovalStarted,
                removal_hash,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.nodes.contains_key(&NodeId::new("node-1")));
        assert!(
            state
                .services
                .contains_key(&(ServiceName::new("web"), NodeId::new("node-1")))
        );
        let removing = state
            .removing_nodes
            .get(&NodeId::new("node-1"))
            .expect("node marked removing");
        assert_eq!(removing.epoch, 2);
        assert_eq!(removing.reason, "graceful-remove");
    }

    #[test]
    fn reducer_tombstone_supersedes_removal_started() {
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
        let removal_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeRemovalStarted(NodeRemovalStartedFact {
                node_id: NodeId::new("node-1"),
                epoch: 2,
                reason: "graceful-remove".to_string(),
            }),
        );
        let tombstone_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
                node_id: NodeId::new("node-1"),
                epoch: 3,
                reason: "removed".to_string(),
            }),
        );
        let candidates = vec![
            candidate(
                "/facts/node/node-1/joined/1",
                FactKind::NodeJoined,
                node_hash,
            ),
            candidate(
                "/facts/node/node-1/removal_started/2",
                FactKind::NodeRemovalStarted,
                removal_hash,
            ),
            candidate(
                "/facts/node/node-1/tombstoned/3",
                FactKind::NodeTombstoned,
                tombstone_hash,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.nodes.is_empty());
        assert!(state.removing_nodes.is_empty());
        assert_eq!(state.tombstoned_nodes.get(&NodeId::new("node-1")), Some(&3));
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 2);
    }

    #[test]
    fn reducer_selects_removal_started_candidate_deterministically() {
        let mut payloads = BTreeMap::new();
        let first_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeRemovalStarted(NodeRemovalStartedFact {
                node_id: NodeId::new("node-1"),
                epoch: 2,
                reason: "first".to_string(),
            }),
        );
        let second_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::NodeRemovalStarted(NodeRemovalStartedFact {
                node_id: NodeId::new("node-1"),
                epoch: 2,
                reason: "second".to_string(),
            }),
        );
        let expected_reason = if first_hash <= second_hash {
            "first"
        } else {
            "second"
        };
        let candidates = vec![
            candidate(
                "/facts/node/node-1/removal_started/2/a",
                FactKind::NodeRemovalStarted,
                first_hash,
            ),
            candidate(
                "/facts/node/node-1/removal_started/2/b",
                FactKind::NodeRemovalStarted,
                second_hash,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        let removing = state
            .removing_nodes
            .get(&NodeId::new("node-1"))
            .expect("node marked removing");
        assert_eq!(removing.epoch, 2);
        assert_eq!(removing.reason, expected_reason);
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 1);
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
    fn reducer_projects_acme_http01_only_for_current_lease_holder() {
        let mut payloads = BTreeMap::new();
        let id = acme_id();
        let claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
        let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
        let claim_payload =
            insert_payload(&mut payloads, ProjectionFactPayload::LeaseClaimed(claim));
        let presented = acme_presented(
            id.clone(),
            "issuer-a",
            LeaseEpoch::first(),
            claim_hash,
            "thumbprintA",
        );
        let presented_payload = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Presented(presented),
        );
        let candidates = vec![
            candidate_as(
                "issuer-a",
                &id.lease_claimed_fact_key(LeaseEpoch::first()),
                FactKind::LeaseClaimed,
                claim_payload,
            ),
            candidate_as(
                "issuer-a",
                &id.presented_fact_key(LeaseEpoch::first()),
                FactKind::AcmeHttp01Presented,
                presented_payload,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        let challenge = state
            .acme_http01
            .values()
            .next()
            .expect("challenge projects");
        assert_eq!(challenge.holder, LeaseHolder::new("issuer-a"));
        assert_eq!(challenge.claim_hash, claim_hash);
        assert_eq!(challenge.expires_at, LeaseTimestamp::from_secs(160));
    }

    #[test]
    fn reducer_rejects_acme_http01_without_matching_lease() {
        let mut payloads = BTreeMap::new();
        let id = acme_id();
        let claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
        let claim_hash = LeaseFact::Claimed(claim).content_hash();
        let presented = acme_presented(
            id.clone(),
            "issuer-a",
            LeaseEpoch::first(),
            claim_hash,
            "thumbprintA",
        );
        let presented_payload = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Presented(presented),
        );
        let candidates = vec![candidate_as(
            "issuer-a",
            &id.presented_fact_key(LeaseEpoch::first()),
            FactKind::AcmeHttp01Presented,
            presented_payload,
        )];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.acme_http01.is_empty());
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 1);
    }

    #[test]
    fn reducer_rejects_acme_http01_written_by_non_holder() {
        let mut payloads = BTreeMap::new();
        let id = acme_id();
        let claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
        let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
        let claim_payload =
            insert_payload(&mut payloads, ProjectionFactPayload::LeaseClaimed(claim));
        let presented = acme_presented(
            id.clone(),
            "issuer-a",
            LeaseEpoch::first(),
            claim_hash,
            "thumbprintA",
        );
        let presented_payload = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Presented(presented),
        );
        let candidates = vec![
            candidate_as(
                "issuer-a",
                &id.lease_claimed_fact_key(LeaseEpoch::first()),
                FactKind::LeaseClaimed,
                claim_payload,
            ),
            candidate_as(
                "issuer-b",
                &id.presented_fact_key(LeaseEpoch::first()),
                FactKind::AcmeHttp01Presented,
                presented_payload,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.acme_http01.is_empty());
        assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 1);
    }

    #[test]
    fn reducer_rejects_acme_fact_key_with_untyped_hostname_or_token() {
        let mut payloads = BTreeMap::new();
        let id = acme_id();
        let claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
        let claim_hash = LeaseFact::Claimed(claim).content_hash();
        let presented_payload = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Presented(acme_presented(
                id,
                "issuer-a",
                LeaseEpoch::first(),
                claim_hash,
                "thumbprintA",
            )),
        );
        let candidates = vec![candidate_as(
            "issuer-a",
            "/facts/acme/http01/bad name.example/short/presented/1",
            FactKind::AcmeHttp01Presented,
            presented_payload,
        )];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.acme_http01.is_empty());
        assert_eq!(
            status_count(&state, ProjectionIgnoreReason::MalformedPayload),
            1
        );
    }

    #[test]
    fn reducer_ignores_forged_lease_claim_author_without_poisoning_valid_claim() {
        let mut payloads = BTreeMap::new();
        let id = acme_id();
        let valid_claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
        let valid_claim_hash = LeaseFact::Claimed(valid_claim.clone()).content_hash();
        let forged_epoch = LeaseEpoch::from_u64(2).expect("forged epoch");
        let forged_claim = acme_claim(&id, "issuer-b", forged_epoch);
        let valid_claim_payload = insert_payload(
            &mut payloads,
            ProjectionFactPayload::LeaseClaimed(valid_claim),
        );
        let forged_claim_payload = insert_payload(
            &mut payloads,
            ProjectionFactPayload::LeaseClaimed(forged_claim),
        );
        let presented_payload = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Presented(acme_presented(
                id.clone(),
                "issuer-a",
                LeaseEpoch::first(),
                valid_claim_hash,
                "thumbprintA",
            )),
        );
        let candidates = vec![
            candidate_as(
                "issuer-a",
                &id.lease_claimed_fact_key(LeaseEpoch::first()),
                FactKind::LeaseClaimed,
                valid_claim_payload,
            ),
            candidate_as(
                "issuer-a",
                &id.lease_claimed_fact_key(forged_epoch),
                FactKind::LeaseClaimed,
                forged_claim_payload,
            ),
            candidate_as(
                "issuer-a",
                &id.presented_fact_key(LeaseEpoch::first()),
                FactKind::AcmeHttp01Presented,
                presented_payload,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        let challenge = state
            .acme_http01
            .values()
            .next()
            .expect("valid holder still projects");
        assert_eq!(challenge.holder, LeaseHolder::new("issuer-a"));
    }

    #[test]
    fn reducer_picks_acme_same_epoch_winner_by_content_hash() {
        let mut payloads = BTreeMap::new();
        let id = acme_id();
        let claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
        let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
        let claim_payload =
            insert_payload(&mut payloads, ProjectionFactPayload::LeaseClaimed(claim));
        let presented_a = acme_presented(
            id.clone(),
            "issuer-a",
            LeaseEpoch::first(),
            claim_hash,
            "thumbprintA",
        );
        let presented_b = acme_presented(
            id.clone(),
            "issuer-a",
            LeaseEpoch::first(),
            claim_hash,
            "thumbprintB",
        );
        let hash_a = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Presented(presented_a),
        );
        let hash_b = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Presented(presented_b),
        );
        let expected = if hash_a <= hash_b {
            "tokAcme0123456789abcdef.thumbprintA"
        } else {
            "tokAcme0123456789abcdef.thumbprintB"
        };
        let ordered = vec![
            candidate_as(
                "issuer-a",
                &id.lease_claimed_fact_key(LeaseEpoch::first()),
                FactKind::LeaseClaimed,
                claim_payload.clone(),
            ),
            candidate_as(
                "issuer-a",
                &id.presented_fact_key(LeaseEpoch::first()),
                FactKind::AcmeHttp01Presented,
                hash_a.clone(),
            ),
            candidate_as(
                "issuer-a",
                &id.presented_fact_key(LeaseEpoch::first()),
                FactKind::AcmeHttp01Presented,
                hash_b.clone(),
            ),
        ];
        let shuffled = vec![
            candidate_as(
                "issuer-a",
                &id.presented_fact_key(LeaseEpoch::first()),
                FactKind::AcmeHttp01Presented,
                hash_b,
            ),
            candidate_as(
                "issuer-a",
                &id.lease_claimed_fact_key(LeaseEpoch::first()),
                FactKind::LeaseClaimed,
                claim_payload,
            ),
            candidate_as(
                "issuer-a",
                &id.presented_fact_key(LeaseEpoch::first()),
                FactKind::AcmeHttp01Presented,
                hash_a,
            ),
        ];

        let ordered_state = reduce_facts(&island("prod"), &ordered, &payloads);
        let shuffled_state = reduce_facts(&island("prod"), &shuffled, &payloads);

        assert_eq!(ordered_state, shuffled_state);
        assert_eq!(
            ordered_state
                .acme_http01
                .values()
                .next()
                .expect("challenge")
                .key_authorization
                .as_str(),
            expected
        );
    }

    #[test]
    fn reducer_only_matching_acme_clear_removes_presentation() {
        let mut payloads = BTreeMap::new();
        let id = acme_id();
        let epoch = LeaseEpoch::from_u64(2).expect("epoch");
        let claim = acme_claim(&id, "issuer-a", epoch);
        let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
        let other_claim_hash =
            LeaseFact::Claimed(acme_claim(&id, "issuer-b", epoch)).content_hash();
        let claim_payload =
            insert_payload(&mut payloads, ProjectionFactPayload::LeaseClaimed(claim));
        let presented_payload = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Presented(acme_presented(
                id.clone(),
                "issuer-a",
                epoch,
                claim_hash,
                "thumbprintA",
            )),
        );
        let stale_clear = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Cleared(acme_cleared(
                id.clone(),
                "issuer-a",
                LeaseEpoch::first(),
                claim_hash,
                LeaseTimestamp::from_secs(120),
            )),
        );
        let non_matching_high_clear = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Cleared(acme_cleared(
                id.clone(),
                "issuer-b",
                LeaseEpoch::from_u64(3).expect("epoch"),
                other_claim_hash,
                LeaseTimestamp::from_secs(120),
            )),
        );
        let matching_clear = insert_payload(
            &mut payloads,
            ProjectionFactPayload::AcmeHttp01Cleared(acme_cleared(
                id.clone(),
                "issuer-a",
                epoch,
                claim_hash,
                LeaseTimestamp::from_secs(120),
            )),
        );

        let without_match = reduce_facts(
            &island("prod"),
            &[
                candidate_as(
                    "issuer-a",
                    &id.lease_claimed_fact_key(epoch),
                    FactKind::LeaseClaimed,
                    claim_payload.clone(),
                ),
                candidate_as(
                    "issuer-a",
                    &id.presented_fact_key(epoch),
                    FactKind::AcmeHttp01Presented,
                    presented_payload.clone(),
                ),
                candidate_as(
                    "issuer-a",
                    &id.cleared_fact_key(LeaseEpoch::first(), claim_hash),
                    FactKind::AcmeHttp01Cleared,
                    stale_clear,
                ),
                candidate_as(
                    "issuer-b",
                    &id.cleared_fact_key(LeaseEpoch::from_u64(3).expect("epoch"), other_claim_hash),
                    FactKind::AcmeHttp01Cleared,
                    non_matching_high_clear,
                ),
            ],
            &payloads,
        );
        let with_match = reduce_facts(
            &island("prod"),
            &[
                candidate_as(
                    "issuer-a",
                    &id.lease_claimed_fact_key(epoch),
                    FactKind::LeaseClaimed,
                    claim_payload,
                ),
                candidate_as(
                    "issuer-a",
                    &id.presented_fact_key(epoch),
                    FactKind::AcmeHttp01Presented,
                    presented_payload,
                ),
                candidate_as(
                    "issuer-a",
                    &id.cleared_fact_key(epoch, claim_hash),
                    FactKind::AcmeHttp01Cleared,
                    matching_clear,
                ),
            ],
            &payloads,
        );

        assert_eq!(without_match.acme_http01.len(), 1);
        assert!(with_match.acme_http01.is_empty());
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
        let lease_resource = LeaseResource::from_segments(["acme", "http01", "example", "token"]);
        let claim_hash = LeaseFact::Claimed(LeaseClaimed::new(
            lease_resource.clone(),
            LeaseHolder::new("issuer-a"),
            LeaseEpoch::first(),
            LeaseTimestamp::from_secs(100),
            LeaseTimestamp::from_secs(160),
        ))
        .content_hash();
        let other_claim_hash = LeaseFact::Claimed(LeaseClaimed::new(
            lease_resource.clone(),
            LeaseHolder::new("issuer-b"),
            LeaseEpoch::first(),
            LeaseTimestamp::from_secs(100),
            LeaseTimestamp::from_secs(160),
        ))
        .content_hash();
        let renewed_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::LeaseRenewed(LeaseRenewed::new(
                lease_resource.clone(),
                LeaseHolder::new("issuer-a"),
                LeaseEpoch::first(),
                claim_hash,
                LeaseTimestamp::from_secs(120),
                LeaseTimestamp::from_secs(180),
            )),
        );
        let released_hash = insert_payload(
            &mut payloads,
            ProjectionFactPayload::LeaseReleased(LeaseReleased::new_at(
                lease_resource.clone(),
                LeaseHolder::new("issuer-a"),
                LeaseEpoch::first(),
                claim_hash,
                LeaseTimestamp::from_secs(130),
            )),
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
            candidate(
                &format!(
                    "/facts/lease/{}/renewed/1/{}/120",
                    lease_resource, other_claim_hash
                ),
                FactKind::LeaseRenewed,
                renewed_hash,
            ),
            candidate(
                &format!(
                    "/facts/lease/{}/released/1/{}/999",
                    lease_resource, claim_hash
                ),
                FactKind::LeaseReleased,
                released_hash,
            ),
        ];

        let state = reduce_facts(&island("prod"), &candidates, &payloads);

        assert!(state.nodes.is_empty());
        assert!(state.services.is_empty());
        assert!(state.gateway.is_none());
        assert_eq!(
            status_count(&state, ProjectionIgnoreReason::MalformedPayload),
            5
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
