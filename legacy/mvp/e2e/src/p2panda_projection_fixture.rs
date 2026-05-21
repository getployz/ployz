use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use mvp_bus::{BusSession, IslandId, PrincipalId};
use mvp_identity::NodeId;
use mvp_p2panda_authz::{
    IslandAuthoritySnapshot, IslandAuthzStore, IslandMemberAuthorKey, IslandMemberEpoch,
    IslandMemberKeyBinding, IslandRootAuthority, ReplicaImportAccess,
};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactAuthoritySource, PandaFactStore, PandaFactWriteOutcome,
};
use mvp_projection::{
    BackendEndpoint, DnsCommitFact, DnsRecordFact, GatewayCommitFact, NodeJoinedFact,
    ProjectionFactPayload, ProjectionIgnoreReason, RouteCommitFact, RouteId, ServiceName,
    ServiceRegistrationFact,
};
use p2panda_core::SigningKey;

use crate::bus_syntax::fact_key;

pub(crate) struct P2pandaMembershipFixture {
    path: PathBuf,
    root: IslandMemberKeyBinding,
    root_author_key_hex: String,
}

impl P2pandaMembershipFixture {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn root_principal(&self) -> &PrincipalId {
        self.root.principal()
    }

    pub(crate) fn root_author_key_hex(&self) -> &str {
        &self.root_author_key_hex
    }

    pub(crate) async fn authority_source(
        &self,
        island: &IslandId,
    ) -> Result<PandaFactAuthoritySource, String> {
        Ok(PandaFactAuthoritySource::from_snapshots(vec![
            self.authority_snapshot(island).await?,
        ]))
    }

    pub(crate) async fn authority_snapshot(
        &self,
        island: &IslandId,
    ) -> Result<IslandAuthoritySnapshot, String> {
        let authority_store = IslandAuthzStore::open(
            &self.path,
            island.clone(),
            IslandRootAuthority::new(self.root.clone()),
        )
        .await
        .map_err(|error| format!("open p2panda membership authority: {error}"))?;
        authority_store
            .authority_snapshot()
            .await
            .map_err(|error| format!("load p2panda membership authority: {error}"))
    }
}

pub(crate) struct P2pandaReplicaImporterFixture {
    author: PandaFactAuthor,
}

pub(crate) fn p2panda_read_replica_importers<const N: usize>(
    importers: [(&BusSession, [u8; 32]); N],
) -> Vec<P2pandaReplicaImporterFixture> {
    importers
        .into_iter()
        .map(
            |(session, private_key_bytes)| P2pandaReplicaImporterFixture {
                author: PandaFactAuthor::from_private_key_bytes(
                    session.principal().clone(),
                    private_key_bytes,
                ),
            },
        )
        .collect()
}

pub(crate) fn p2panda_replica_importer_members(
    importers: &[P2pandaReplicaImporterFixture],
) -> Vec<(&PandaFactAuthor, ReplicaImportAccess)> {
    importers
        .iter()
        .map(|importer| (&importer.author, ReplicaImportAccess::Read))
        .collect()
}

pub(crate) async fn create_p2panda_membership_fixture(
    path: &Path,
    island: &IslandId,
    writers: &[&PandaFactAuthor],
    replica_importers: &[(&PandaFactAuthor, ReplicaImportAccess)],
) -> Result<P2pandaMembershipFixture, String> {
    let root_author = PandaFactAuthor::from_private_key_bytes(
        PrincipalId::new("p2panda-membership-root"),
        [9; 32],
    );
    let root_private_key = SigningKey::from_bytes(&[9; 32]);
    let root = IslandMemberKeyBinding::new(
        island.clone(),
        root_author.principal().clone(),
        IslandMemberEpoch::new(NonZeroU64::MIN),
        IslandMemberAuthorKey::from(root_author.author_key()),
    );
    let store =
        IslandAuthzStore::open(path, island.clone(), IslandRootAuthority::new(root.clone()))
            .await
            .map_err(|error| format!("open p2panda membership store: {error}"))?;
    let mut authz = store
        .create_root(root.clone(), &root_private_key)
        .await
        .map_err(|error| format!("create p2panda membership root: {error}"))?;
    for writer in writers {
        store
            .add_writer(
                &mut authz,
                &root,
                &root_private_key,
                member_binding(island, writer),
            )
            .await
            .map_err(|error| format!("add p2panda writer membership: {error}"))?;
    }
    for (replica, access) in replica_importers {
        store
            .add_replica_importer(
                &mut authz,
                &root,
                &root_private_key,
                member_binding(island, replica),
                *access,
            )
            .await
            .map_err(|error| format!("add p2panda replica membership: {error}"))?;
    }
    Ok(P2pandaMembershipFixture {
        path: path.to_path_buf(),
        root,
        root_author_key_hex: root_author.author_key().as_hex(),
    })
}

fn member_binding(island: &IslandId, author: &PandaFactAuthor) -> IslandMemberKeyBinding {
    IslandMemberKeyBinding::new(
        island.clone(),
        author.principal().clone(),
        IslandMemberEpoch::new(NonZeroU64::MIN),
        IslandMemberAuthorKey::from(author.author_key()),
    )
}

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
