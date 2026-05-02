use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::kv;
use async_nats::jetstream::stream;
use ployz_types::error::{Error, Result};
use ployz_types::model::MachineRole;

use crate::NatsStore;
use crate::config::HUB_DOMAIN;
use crate::role::{ReplicaPreference, desired_replicas};
use crate::subjects::{
    CERT_JOBS_STREAM, DEPLOY_COMMITS_STREAM, INSTANCE_STATE_VIEW_STREAM, MACHINE_DIRECTORY_BUCKET,
    MACHINE_STATE_VIEW_STREAM, REVISIONS_STREAM, ROUTING_EVENTS_STREAM,
    instance_state_stream, instance_state_stream_subjects, machine_state_stream,
    machine_state_stream_subjects,
};
use ployz_types::model::MachineId;

// `machines` and `instances` KV buckets used to live here. They're gone
// because per-machine streams (`state_machine_<id>`, `state_instances_<id>`)
// own that data now and the local view streams (`machine_state_view`,
// `instance_state_view`) materialize it for read paths. See `subjects.rs`
// and `state_view.rs`.
pub const INVITES_BUCKET: &str = "invites";
pub const DEPLOY_STATUS_BUCKET: &str = "deploy_status";
pub const ACME_ACCOUNTS_BUCKET: &str = "acme_accounts";
pub const CERTIFICATES_BUCKET: &str = "certificates";
pub const ACME_CHALLENGES_BUCKET: &str = "acme_challenges";
pub const ACME_CHALLENGE_READINESS_BUCKET: &str = "acme_challenge_readiness";
pub const LOCKS_BUCKET: &str = "locks";
pub const COORDINATOR_LEASE_BUCKET: &str = "coordinator_lease";
const LEASE_DELETE_MARKER_TTL: Duration = Duration::from_secs(60 * 60);

/// JetStream prefixes a `KV_` namespace onto the underlying stream that backs
/// each KV bucket. When a leaf mirrors a hub bucket, the `Source.name` must
/// reference that underlying stream — *not* the bucket alias.
fn kv_underlying_stream_name(bucket: &str) -> String {
    format!("KV_{bucket}")
}

/// Stream name for a leaf-side mirror of a hub stream. Suffixed so the leaf
/// domain can hold both the local view (read path) and any same-named hub
/// asset visible over leafnode.
#[must_use]
pub fn mirror_stream_name(stream_name: &str) -> String {
    format!("{stream_name}_mirror")
}

/// Resolve the stream name a *reader* on `store` should consume from. On a
/// `Mirror` node this is the leaf-side mirror (local read); on a
/// `StorageCandidate` it's the hub stream itself.
#[must_use]
pub fn local_read_stream_name(store: &NatsStore, hub_stream: &str) -> String {
    match store.role() {
        MachineRole::Mirror => mirror_stream_name(hub_stream),
        MachineRole::StorageCandidate | MachineRole::Leaf => hub_stream.to_string(),
    }
}

/// Hub-domain durable bucket names. Locks and the coordinator lease are
/// intentionally excluded from this list — those need linearizable reads
/// against the hub and must never be mirrored.
const DURABLE_BUCKETS: &[&str] = &[
    INVITES_BUCKET,
    DEPLOY_STATUS_BUCKET,
    ACME_ACCOUNTS_BUCKET,
    CERTIFICATES_BUCKET,
    ACME_CHALLENGES_BUCKET,
    ACME_CHALLENGE_READINESS_BUCKET,
    MACHINE_DIRECTORY_BUCKET,
];

const LEASE_BUCKETS: &[&str] = &[LOCKS_BUCKET, COORDINATOR_LEASE_BUCKET];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetPolicy {
    pub storage_candidates: usize,
    pub replica_preference: ReplicaPreference,
}

impl AssetPolicy {
    #[must_use]
    pub fn replicas(self) -> usize {
        desired_replicas(self.storage_candidates, self.replica_preference)
    }
}

#[derive(Debug, Clone)]
pub struct AssetConfigs {
    pub deploy_commits: stream::Config,
    pub routing_events: stream::Config,
    pub revisions: stream::Config,
    pub cert_jobs: stream::Config,
    pub durable_kv: Vec<kv::Config>,
    pub lease_kv: Vec<kv::Config>,
}

#[derive(Debug, Clone)]
pub struct LeafMirrorConfigs {
    pub mirrored_streams: Vec<stream::Config>,
    pub mirrored_kv: Vec<kv::Config>,
}

/// Builder that decides which assets to declare based on the local node's
/// role. `StorageCandidate` owns the hub assets. `Mirror` owns leaf-side
/// mirrors. `Leaf` owns nothing — it is a NATS-core proxy with no local
/// JetStream.
#[derive(Debug, Clone, Copy)]
pub struct NatsAssets {
    pub role: MachineRole,
    pub policy: AssetPolicy,
}

impl NatsAssets {
    #[must_use]
    pub fn new(role: MachineRole, policy: AssetPolicy) -> Self {
        Self { role, policy }
    }

    /// Hub-side asset declarations (streams + KVs). Created on
    /// `StorageCandidate` only; on other roles this is a planning artefact
    /// for tests or operator tooling.
    #[must_use]
    pub fn hub_configs(&self) -> AssetConfigs {
        let replicas = self.policy.replicas();
        AssetConfigs {
            deploy_commits: deploy_commits_stream(replicas),
            routing_events: routing_events_stream(replicas),
            revisions: revisions_stream(replicas),
            cert_jobs: cert_jobs_stream(replicas),
            durable_kv: durable_buckets(replicas),
            lease_kv: lease_buckets(replicas),
        }
    }

    /// Leaf-domain mirror declarations. Returns `None` on non-`Mirror`
    /// roles so callers don't have to reason about empty plans for the
    /// roles that have no leaf JetStream.
    ///
    /// Excluded by design:
    /// - `cert_jobs` (WorkQueue retention; JetStream rejects mirror config
    ///   on a WorkQueue stream because ack ownership has to be unique).
    /// - `LOCKS_BUCKET`, `COORDINATOR_LEASE_BUCKET` (linearizable lease
    ///   semantics depend on hub-side CAS; mirroring them would introduce
    ///   stale-read split-brain).
    #[must_use]
    pub fn leaf_mirror_configs(&self) -> Option<LeafMirrorConfigs> {
        match self.role {
            MachineRole::Mirror => Some(LeafMirrorConfigs {
                mirrored_streams: vec![
                    mirror_stream_config(DEPLOY_COMMITS_STREAM),
                    mirror_stream_config(ROUTING_EVENTS_STREAM),
                    mirror_stream_config(REVISIONS_STREAM),
                ],
                mirrored_kv: DURABLE_BUCKETS
                    .iter()
                    .map(|bucket| mirror_kv_config(bucket))
                    .collect(),
            }),
            MachineRole::StorageCandidate | MachineRole::Leaf => None,
        }
    }
}

/// Reconcile the JetStream assets required by `store`'s role.
///
/// - `StorageCandidate`: ensures hub-side streams and KVs against
///   `store.hub_jetstream()`.
/// - `Mirror`: ensures leaf-side mirror streams and mirrored KVs against
///   `store.local_jetstream()`. Locks, coordinator_lease, and the cert_jobs
///   WorkQueue stay hub-only and are reached via leafnode forwarding.
/// - `Leaf`: no-op. A leaf node has no local JetStream context.
pub async fn ensure_assets(store: &NatsStore) -> Result<()> {
    let assets = NatsAssets::new(store.role(), store.asset_policy());
    match store.role() {
        MachineRole::StorageCandidate => ensure_hub_assets(store.hub_jetstream(), assets).await,
        MachineRole::Mirror => ensure_leaf_mirrors(store.local_jetstream(), assets).await,
        MachineRole::Leaf => Ok(()),
    }
}

async fn ensure_hub_assets(js: &jetstream::Context, assets: NatsAssets) -> Result<()> {
    let configs = assets.hub_configs();
    ensure_stream(js, configs.deploy_commits).await?;
    ensure_stream(js, configs.routing_events).await?;
    ensure_stream(js, configs.revisions).await?;
    ensure_stream(js, configs.cert_jobs).await?;
    for config in configs.durable_kv.into_iter().chain(configs.lease_kv) {
        ensure_kv(js, config).await?;
    }
    Ok(())
}

/// Create the two hub-owned per-machine state streams. Idempotent — safe
/// to call repeatedly on machine bootstrap or after a coordinator restart.
/// Writes go through `store.hub_jetstream()` regardless of role; only the
/// founder/coordinator path should call this.
pub async fn ensure_machine_state_assets(store: &NatsStore, machine_id: &MachineId) -> Result<()> {
    let replicas = store.asset_policy().replicas();
    ensure_stream(
        store.hub_jetstream(),
        per_machine_state_stream(machine_id, replicas),
    )
    .await?;
    ensure_stream(
        store.hub_jetstream(),
        per_machine_instances_stream(machine_id, replicas),
    )
    .await
}

/// Delete a machine's per-node state streams. Called from the coordinator
/// during `handle_machine_remove` after the routing tombstone is published.
pub async fn delete_machine_state_assets(
    store: &NatsStore,
    machine_id: &MachineId,
) -> Result<()> {
    delete_stream_if_exists(store.hub_jetstream(), &machine_state_stream(machine_id)).await?;
    delete_stream_if_exists(store.hub_jetstream(), &instance_state_stream(machine_id)).await
}

async fn delete_stream_if_exists(js: &jetstream::Context, name: &str) -> Result<()> {
    match js.delete_stream(name).await {
        Ok(_) => Ok(()),
        Err(error) => {
            // The stream not existing is fine — this is part of an idempotent
            // cleanup. Any other error is surfaced.
            let message = format!("{error:?}");
            if message.contains("stream not found") || message.contains("10059") {
                return Ok(());
            }
            Err(Error::operation(
                "nats_delete_machine_state_stream",
                message,
            ))
        }
    }
}

fn per_machine_state_stream(machine_id: &MachineId, replicas: usize) -> stream::Config {
    stream::Config {
        name: machine_state_stream(machine_id),
        subjects: machine_state_stream_subjects(machine_id),
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        max_messages_per_subject: 1,
        discard: stream::DiscardPolicy::New,
        duplicate_window: Duration::from_secs(60 * 60),
        allow_direct: true,
        ..Default::default()
    }
}

fn per_machine_instances_stream(machine_id: &MachineId, replicas: usize) -> stream::Config {
    stream::Config {
        name: instance_state_stream(machine_id),
        subjects: instance_state_stream_subjects(machine_id),
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        max_messages_per_subject: 1,
        discard: stream::DiscardPolicy::New,
        duplicate_window: Duration::from_secs(60 * 60),
        allow_direct: true,
        ..Default::default()
    }
}

/// Build the local view-stream config that aggregates a known set of
/// per-machine source streams. Call sites maintain the source list by
/// reconciling against the `machine_directory` KV.
#[must_use]
pub fn machine_state_view_config(sources: Vec<stream::Source>) -> stream::Config {
    state_view_config(MACHINE_STATE_VIEW_STREAM, sources)
}

#[must_use]
pub fn instance_state_view_config(sources: Vec<stream::Source>) -> stream::Config {
    state_view_config(INSTANCE_STATE_VIEW_STREAM, sources)
}

fn state_view_config(name: &str, sources: Vec<stream::Source>) -> stream::Config {
    stream::Config {
        name: name.to_string(),
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        num_replicas: 1,
        max_messages_per_subject: 1,
        discard: stream::DiscardPolicy::New,
        sources: if sources.is_empty() { None } else { Some(sources) },
        allow_direct: true,
        ..Default::default()
    }
}

/// Build a hub-domain `Source` referencing one machine's state stream.
/// Used to assemble the `sources` list for `machine_state_view_config` /
/// `instance_state_view_config`.
#[must_use]
pub fn machine_state_source(machine_id: &MachineId) -> stream::Source {
    stream::Source {
        name: machine_state_stream(machine_id),
        domain: Some(HUB_DOMAIN.into()),
        ..Default::default()
    }
}

#[must_use]
pub fn instance_state_source(machine_id: &MachineId) -> stream::Source {
    stream::Source {
        name: instance_state_stream(machine_id),
        domain: Some(HUB_DOMAIN.into()),
        ..Default::default()
    }
}

/// Reconcile both view streams against the current directory snapshot.
/// Drives `machine_state_view` and `instance_state_view` to source from
/// exactly the per-machine streams listed in `machine_ids`.
///
/// Idempotent: on first call (or first call after restart) the streams are
/// created with the desired source set; on subsequent calls the source set
/// is updated in place via `update_stream`. JetStream tolerates source
/// list mutations and replays from each new source from sequence 1.
pub async fn reconcile_view_streams(store: &NatsStore, machine_ids: &[MachineId]) -> Result<()> {
    let machine_sources: Vec<_> = machine_ids.iter().map(machine_state_source).collect();
    let instance_sources: Vec<_> = machine_ids.iter().map(instance_state_source).collect();
    apply_view_stream(store, machine_state_view_config(machine_sources)).await?;
    apply_view_stream(store, instance_state_view_config(instance_sources)).await
}

async fn apply_view_stream(store: &NatsStore, config: stream::Config) -> Result<()> {
    let js = store.local_jetstream();
    match js.get_stream(config.name.clone()).await {
        Ok(_) => js
            .update_stream(&config)
            .await
            .map(|_| ())
            .map_err(|error| {
                Error::operation("nats_state_view_update", format!("{error:?}"))
            }),
        Err(_) => js
            .create_stream(config)
            .await
            .map(|_| ())
            .map_err(|error| {
                Error::operation("nats_state_view_create", format!("{error:?}"))
            }),
    }
}

async fn ensure_leaf_mirrors(js: &jetstream::Context, assets: NatsAssets) -> Result<()> {
    let Some(configs) = assets.leaf_mirror_configs() else {
        return Ok(());
    };
    for config in configs.mirrored_streams {
        ensure_stream(js, config).await?;
    }
    for config in configs.mirrored_kv {
        ensure_kv(js, config).await?;
    }
    Ok(())
}

async fn ensure_stream(js: &jetstream::Context, config: stream::Config) -> Result<()> {
    js.get_or_create_stream(config)
        .await
        .map(|_| ())
        .map_err(|error| Error::operation("nats_ensure_stream", format!("{error:?}")))
}

async fn ensure_kv(js: &jetstream::Context, config: kv::Config) -> Result<()> {
    match js.get_key_value(config.bucket.clone()).await {
        Ok(bucket) => ensure_existing_kv(js, bucket, config).await,
        Err(_) => js
            .create_key_value(config)
            .await
            .map(|_| ())
            .map_err(|error| Error::operation("nats_ensure_kv", format!("{error:?}"))),
    }
}

async fn ensure_existing_kv(
    js: &jetstream::Context,
    bucket: kv::Store,
    config: kv::Config,
) -> Result<()> {
    if !existing_kv_needs_update(&bucket, &config).await? {
        return Ok(());
    }
    js.update_key_value(config)
        .await
        .map(|_| ())
        .map_err(|error| Error::operation("nats_update_kv", format!("{error:?}")))
}

async fn existing_kv_needs_update(bucket: &kv::Store, desired: &kv::Config) -> Result<bool> {
    let mut stream = bucket.stream.clone();
    let info = stream
        .info()
        .await
        .map_err(|error| Error::operation("nats_kv_info", format!("{error:?}")))?;
    Ok(ttl_policy_needs_update(
        info.config.allow_message_ttl,
        info.config.subject_delete_marker_ttl,
        desired.limit_markers,
    ))
}

fn ttl_policy_needs_update(
    allow_message_ttl: bool,
    subject_delete_marker_ttl: Option<Duration>,
    desired_limit_markers: Option<Duration>,
) -> bool {
    match desired_limit_markers {
        Some(desired) => !allow_message_ttl || subject_delete_marker_ttl != Some(desired),
        None => false,
    }
}

fn deploy_commits_stream(replicas: usize) -> stream::Config {
    stream::Config {
        name: DEPLOY_COMMITS_STREAM.into(),
        subjects: vec!["deploy_commits.>".into()],
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        max_age: Duration::ZERO,
        max_messages_per_subject: -1,
        max_messages: -1,
        discard: stream::DiscardPolicy::New,
        duplicate_window: Duration::from_secs(60 * 60),
        allow_direct: true,
        ..Default::default()
    }
}

fn routing_events_stream(replicas: usize) -> stream::Config {
    stream::Config {
        name: ROUTING_EVENTS_STREAM.into(),
        subjects: vec!["routing.events.>".into()],
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        max_age: Duration::ZERO,
        max_messages_per_subject: -1,
        max_messages: -1,
        discard: stream::DiscardPolicy::New,
        duplicate_window: Duration::from_secs(60 * 60),
        allow_direct: true,
        allow_atomic_publish: true,
        ..Default::default()
    }
}

fn revisions_stream(replicas: usize) -> stream::Config {
    stream::Config {
        name: REVISIONS_STREAM.into(),
        subjects: vec!["revisions.>".into()],
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        max_messages_per_subject: 1,
        discard: stream::DiscardPolicy::New,
        duplicate_window: Duration::from_secs(60 * 60),
        ..Default::default()
    }
}

fn cert_jobs_stream(replicas: usize) -> stream::Config {
    stream::Config {
        name: CERT_JOBS_STREAM.into(),
        subjects: vec!["cert.jobs.>".into()],
        retention: stream::RetentionPolicy::WorkQueue,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        duplicate_window: Duration::from_secs(60 * 60),
        allow_message_schedules: true,
        ..Default::default()
    }
}

fn durable_buckets(replicas: usize) -> Vec<kv::Config> {
    DURABLE_BUCKETS
        .iter()
        .map(|bucket| kv::Config {
            bucket: (*bucket).into(),
            history: 1,
            max_age: Duration::ZERO,
            storage: stream::StorageType::File,
            num_replicas: replicas,
            ..Default::default()
        })
        .collect()
}

fn lease_buckets(replicas: usize) -> Vec<kv::Config> {
    LEASE_BUCKETS
        .iter()
        .map(|bucket| kv::Config {
            bucket: (*bucket).into(),
            history: 1,
            storage: stream::StorageType::File,
            num_replicas: replicas,
            limit_markers: Some(LEASE_DELETE_MARKER_TTL),
            ..Default::default()
        })
        .collect()
}

fn mirror_stream_config(source_stream: &str) -> stream::Config {
    stream::Config {
        name: mirror_stream_name(source_stream),
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        num_replicas: 1,
        mirror: Some(stream::Source {
            name: source_stream.into(),
            domain: Some(HUB_DOMAIN.into()),
            ..Default::default()
        }),
        allow_direct: true,
        ..Default::default()
    }
}

fn mirror_kv_config(bucket: &str) -> kv::Config {
    kv::Config {
        bucket: bucket.into(),
        history: 1,
        max_age: Duration::ZERO,
        storage: stream::StorageType::File,
        num_replicas: 1,
        mirror: Some(stream::Source {
            name: kv_underlying_stream_name(bucket),
            domain: Some(HUB_DOMAIN.into()),
            ..Default::default()
        }),
        mirror_direct: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three_storage() -> AssetPolicy {
        AssetPolicy {
            storage_candidates: 3,
            replica_preference: ReplicaPreference::Three,
        }
    }

    fn one_storage() -> AssetPolicy {
        AssetPolicy {
            storage_candidates: 2,
            replica_preference: ReplicaPreference::Default,
        }
    }

    fn hub_configs(policy: AssetPolicy) -> AssetConfigs {
        NatsAssets::new(MachineRole::StorageCandidate, policy).hub_configs()
    }

    #[test]
    fn deploy_commit_stream_is_unpruned_and_not_collapsed() {
        let config = hub_configs(three_storage()).deploy_commits;
        assert_eq!(config.retention, stream::RetentionPolicy::Limits);
        assert_eq!(config.max_age, Duration::ZERO);
        assert_eq!(config.max_messages_per_subject, -1);
        assert_eq!(config.num_replicas, 3);
    }

    #[test]
    fn routing_events_stream_allows_atomic_publish() {
        let config = hub_configs(three_storage()).routing_events;
        assert_eq!(config.name, ROUTING_EVENTS_STREAM);
        assert_eq!(config.subjects, vec!["routing.events.>".to_string()]);
        assert_eq!(config.retention, stream::RetentionPolicy::Limits);
        assert_eq!(config.num_replicas, 3);
        assert!(config.allow_direct);
        assert!(config.allow_atomic_publish);
    }

    #[test]
    fn cert_jobs_stream_allows_scheduled_messages() {
        let config = hub_configs(three_storage()).cert_jobs;
        assert_eq!(config.name, CERT_JOBS_STREAM);
        assert_eq!(config.subjects, vec!["cert.jobs.>".to_string()]);
        assert_eq!(config.retention, stream::RetentionPolicy::WorkQueue);
        assert_eq!(config.num_replicas, 3);
        assert_eq!(config.duplicate_window, Duration::from_secs(60 * 60));
        assert!(config.allow_message_schedules);
    }

    #[test]
    fn durable_buckets_have_no_ttl() {
        let configs = hub_configs(one_storage());
        assert_eq!(configs.deploy_commits.num_replicas, 1);
        for bucket in configs.durable_kv {
            assert_eq!(bucket.max_age, Duration::ZERO);
            assert_eq!(bucket.history, 1);
        }
    }

    #[test]
    fn lease_buckets_enable_per_message_ttl() {
        let configs = hub_configs(three_storage());
        for bucket in configs.lease_kv {
            assert_eq!(bucket.history, 1);
            assert_eq!(bucket.num_replicas, 3);
            assert_eq!(bucket.limit_markers, Some(LEASE_DELETE_MARKER_TTL));
        }
    }

    #[test]
    fn existing_lease_bucket_updates_when_message_ttl_is_disabled() {
        assert!(ttl_policy_needs_update(
            false,
            None,
            Some(LEASE_DELETE_MARKER_TTL)
        ));
        assert!(ttl_policy_needs_update(
            true,
            Some(Duration::from_secs(1)),
            Some(LEASE_DELETE_MARKER_TTL)
        ));
        assert!(!ttl_policy_needs_update(
            true,
            Some(LEASE_DELETE_MARKER_TTL),
            Some(LEASE_DELETE_MARKER_TTL)
        ));
        assert!(!ttl_policy_needs_update(false, None, None));
    }

    #[test]
    fn storage_candidate_has_no_leaf_mirror_configs() {
        let assets = NatsAssets::new(MachineRole::StorageCandidate, three_storage());
        assert!(assets.leaf_mirror_configs().is_none());
    }

    #[test]
    fn leaf_role_has_no_leaf_mirror_configs() {
        let assets = NatsAssets::new(MachineRole::Leaf, one_storage());
        assert!(assets.leaf_mirror_configs().is_none());
    }

    #[test]
    fn mirror_leaf_configs_target_hub_domain() {
        let assets = NatsAssets::new(MachineRole::Mirror, one_storage());
        let configs = assets.leaf_mirror_configs().expect("Mirror has configs");
        for stream_config in &configs.mirrored_streams {
            let source = stream_config
                .mirror
                .as_ref()
                .expect("mirrored stream carries a Source");
            assert_eq!(source.domain.as_deref(), Some(HUB_DOMAIN));
        }
        for kv_config in &configs.mirrored_kv {
            let source = kv_config
                .mirror
                .as_ref()
                .expect("mirrored KV carries a Source");
            assert_eq!(source.domain.as_deref(), Some(HUB_DOMAIN));
        }
    }

    #[test]
    fn mirror_leaf_configs_exclude_workqueue_and_locks() {
        let assets = NatsAssets::new(MachineRole::Mirror, one_storage());
        let configs = assets.leaf_mirror_configs().expect("Mirror has configs");
        let stream_names: Vec<_> = configs
            .mirrored_streams
            .iter()
            .map(|cfg| cfg.name.as_str())
            .collect();
        assert!(
            !stream_names
                .iter()
                .any(|name| name.contains(CERT_JOBS_STREAM)),
            "cert_jobs WorkQueue must not be mirrored, got {stream_names:?}",
        );
        let kv_names: Vec<_> = configs
            .mirrored_kv
            .iter()
            .map(|cfg| cfg.bucket.as_str())
            .collect();
        for forbidden in [LOCKS_BUCKET, COORDINATOR_LEASE_BUCKET] {
            assert!(
                !kv_names.contains(&forbidden),
                "{forbidden} must stay hub-only, got {kv_names:?}",
            );
        }
    }

    #[test]
    fn mirror_kv_sources_underlying_stream_name() {
        let assets = NatsAssets::new(MachineRole::Mirror, one_storage());
        let configs = assets.leaf_mirror_configs().expect("Mirror has configs");
        for kv_config in &configs.mirrored_kv {
            let source = kv_config
                .mirror
                .as_ref()
                .expect("mirrored KV carries a Source");
            // Per JetStream KV layout the underlying stream is `KV_<bucket>`.
            assert_eq!(source.name, format!("KV_{}", kv_config.bucket));
        }
    }

    #[test]
    fn mirror_streams_use_mirror_suffix() {
        let assets = NatsAssets::new(MachineRole::Mirror, one_storage());
        let configs = assets.leaf_mirror_configs().expect("Mirror has configs");
        let names: Vec<_> = configs
            .mirrored_streams
            .iter()
            .map(|cfg| cfg.name.as_str())
            .collect();
        assert!(names.iter().any(|n| n == &"deploy_commits_mirror"));
        assert!(names.iter().any(|n| n == &"routing_events_mirror"));
        assert!(names.iter().any(|n| n == &"revisions_mirror"));
    }

    #[test]
    fn mirror_streams_replicate_at_one_replica() {
        let assets = NatsAssets::new(MachineRole::Mirror, three_storage());
        let configs = assets.leaf_mirror_configs().expect("Mirror has configs");
        for stream_config in &configs.mirrored_streams {
            assert_eq!(
                stream_config.num_replicas, 1,
                "leaf-side mirrors are local materializations, never replicated",
            );
        }
        for kv_config in &configs.mirrored_kv {
            assert_eq!(kv_config.num_replicas, 1);
        }
    }
}
