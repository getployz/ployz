use std::collections::BTreeMap;

use mvp_acme::{
    AcmeCertificateActivatedFact, AcmeChallengeId, AcmeHostname, AcmeHttp01ClearedFact,
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
    ServingCertificateProjection,
};
use crate::source::{FactCandidate, FactKind};
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
    certificates: BTreeMap<AcmeHostname, Vec<CommitCandidate<AcmeCertificateActivatedFact>>>,
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
            certificates: BTreeMap::new(),
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
            (FactKind::PeerAdmitted, ProjectionFactPayload::PeerAdmitted(_fact)) => {}
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
                    .entry(fact.resource.clone())
                    .or_default()
                    .push(CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    });
            }
            (FactKind::LeaseRenewed, ProjectionFactPayload::LeaseRenewed(fact)) => {
                self.lease_renewals
                    .entry(fact.resource.clone())
                    .or_default()
                    .push(CommitCandidate {
                        author,
                        content_hash,
                        fact,
                    });
            }
            (FactKind::LeaseReleased, ProjectionFactPayload::LeaseReleased(fact)) => {
                self.lease_releases
                    .entry(fact.resource.clone())
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
            (
                FactKind::AcmeCertificateActivated,
                ProjectionFactPayload::AcmeCertificateActivated(fact),
            ) => {
                self.certificates
                    .entry(fact.hostname.clone())
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
        self.state.certificates = self.project_certificates();
        self.record_lease_supersession();
        let serving_commits = self.serving_heads_by_route();
        if !serving_commits.is_empty() {
            self.state.gateway = Some(project_gateway_from_serving_commits(&serving_commits));
            self.state.dns = Some(project_dns_from_serving_commits(serving_commits));
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

    fn project_certificates(&mut self) -> BTreeMap<AcmeHostname, ServingCertificateProjection> {
        let mut projected = BTreeMap::new();
        let hostnames = self.certificates.keys().cloned().collect::<Vec<_>>();
        for hostname in hostnames {
            let Some(candidates) = self.certificates.get(&hostname) else {
                continue;
            };
            let Some(selected) = select_head(
                candidates.iter(),
                |fact| fact.issued_at_secs,
                |fact| fact.order_url.as_str(),
            ) else {
                continue;
            };
            self.ignore_many(ProjectionIgnoreReason::Superseded, selected.superseded);
            projected.insert(
                hostname,
                ServingCertificateProjection {
                    hostname: selected.fact.hostname.clone(),
                    order_url: selected.fact.order_url.clone(),
                    fullchain_pem: selected.fact.fullchain_pem.clone(),
                    private_key_pem: selected.fact.private_key_pem.clone(),
                    issued_at_secs: selected.fact.issued_at_secs,
                    not_before_secs: selected.fact.not_before_secs,
                    not_after_secs: selected.fact.not_after_secs,
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
        let winner =
            select_lease_claim_head(claims.iter().filter(|candidate| {
                author_matches_holder(&candidate.author, &candidate.fact.holder)
            }))?;
        let claim_hash = lease_claim_hash(&winner.fact);
        let expires_at = self.latest_lease_expiry(resource, &winner.fact, claim_hash);
        if self.has_matching_lease_release(resource, &winner.fact, claim_hash, expires_at) {
            return None;
        }
        Some(ActiveLeaseHead {
            holder: winner.fact.holder.clone(),
            epoch: winner.fact.epoch,
            claim_hash,
            acquired_at: winner.fact.acquired_at,
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
                author_matches_holder(&renewed.author, &renewed.fact.holder)
                    && renewed.fact.holder == claim.holder
                    && renewed.fact.epoch == claim.epoch
                    && renewed.fact.claim_hash == claim_hash
            })
            .collect::<Vec<_>>();
        renewals.sort_by_key(|renewed| (renewed.fact.renewed_at, renewed.content_hash.clone()));

        let mut expires_at = claim.expires_at;
        for renewed in renewals {
            if renewed.fact.renewed_at >= claim.acquired_at
                && renewed.fact.renewed_at < expires_at
                && renewed.fact.expires_at > renewed.fact.renewed_at
            {
                expires_at = renewed.fact.expires_at;
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
                author_matches_holder(&released.author, &released.fact.holder)
                    && released.fact.holder == claim.holder
                    && released.fact.epoch == claim.epoch
                    && released.fact.claim_hash == claim_hash
                    && release_applies_to_claim(released.fact.release, claim, expires_at)
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

    fn serving_heads_by_route(&mut self) -> Vec<ServingCommitFact> {
        let mut by_route = BTreeMap::new();
        for commit in self.serving_commits.values() {
            by_route
                .entry(commit.fact.route_id.clone())
                .or_insert_with(Vec::new)
                .push(commit.clone());
        }
        by_route
            .into_values()
            .filter_map(|commits| {
                let selection = select_head(
                    commits.iter(),
                    |fact| fact.epoch,
                    |fact| fact.serving_commit_id.as_str(),
                )?;
                self.ignore_many(ProjectionIgnoreReason::Superseded, selection.superseded);
                Some(selection.fact)
            })
            .collect()
    }
}

fn project_gateway_from_serving_commits(commits: &[ServingCommitFact]) -> GatewayProjection {
    let head = serving_projection_head(commits);
    let mut routes = commits
        .iter()
        .map(project_route_from_serving)
        .collect::<Vec<_>>();
    routes.sort();
    GatewayProjection {
        gateway_commit_id: head.gateway_commit_id.clone(),
        route_commit_id: head.route_commit_id.clone(),
        routes,
    }
}

fn project_route_from_serving(commit: &ServingCommitFact) -> GatewayRouteProjection {
    let mut hostnames = commit.hostnames.clone();
    hostnames.sort();
    let mut backends = commit.backends.clone();
    backends.sort();
    let mut old_backends_to_drain = commit.old_backends_to_drain.clone();
    old_backends_to_drain.sort();
    GatewayRouteProjection {
        route_id: commit.route_id.clone(),
        hostnames,
        backends,
        old_backends_to_drain,
    }
}

fn project_dns_from_serving_commits(commits: Vec<ServingCommitFact>) -> DnsProjection {
    let dns_commit_id = serving_projection_head(&commits).dns_commit_id.clone();
    let mut records = commits
        .into_iter()
        .flat_map(|commit| commit.dns_records)
        .map(DnsRecordProjection::from)
        .collect::<Vec<_>>();
    records.sort();
    DnsProjection {
        dns_commit_id,
        records,
    }
}

fn serving_projection_head(commits: &[ServingCommitFact]) -> &ServingCommitFact {
    commits
        .iter()
        .max_by_key(|commit| (commit.epoch, commit.serving_commit_id.as_str()))
        .expect("serving projection head requires at least one commit")
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
        let candidate_epoch = candidate.fact.epoch;
        let candidate_claim_hash = lease_claim_hash(&candidate.fact);
        let replace = match (selected, selected_claim_hash) {
            (None, _) => true,
            (Some(_), Some(_)) if candidate_epoch > selected_epoch => true,
            (Some(current), Some(current_hash)) if candidate_epoch == selected_epoch => {
                candidate_claim_hash
                    .cmp(&current_hash)
                    .then_with(|| candidate.fact.holder.cmp(&current.fact.holder))
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
            released_at >= claim.acquired_at && released_at < expires_at
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

mod key_expectation;

pub use key_expectation::payload_matches_key;
use key_expectation::rejection_reason;
#[cfg(test)]
mod tests;
