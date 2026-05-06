use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::kv;
use async_nats::jetstream::stream;
use ployz_types::error::{Error, Result};

use crate::role::{ReplicaPreference, desired_replicas};
use crate::subjects::{
    self, CERT_JOBS_STREAM, DEPLOY_COMMITS_STREAM, NatsScope, REVISIONS_STREAM,
    ROUTING_EVENTS_STREAM,
};

pub const AUTHORITIES_BUCKET: &str = "authorities_local";
pub const REGIONS_BUCKET: &str = "regions_local";
pub const MACHINES_BUCKET: &str = "machines_local";
pub const GATEWAYS_BUCKET: &str = "gateways_local";
pub const DNS_BUCKET: &str = "dns_local";
pub const INVITES_BUCKET: &str = "cp_invites_auth-default";
pub const DEPLOY_STATUS_BUCKET: &str = "cp_deploy_status_auth-default";
pub const PARTICIPANTS_BUCKET: &str = "cp_participants_auth-default";
pub const INSTANCES_BUCKET: &str = "cp_instances_auth-default";
pub const ACME_ACCOUNTS_BUCKET: &str = "cp_acme_accounts_auth-default";
pub const CERTIFICATES_BUCKET: &str = "cp_certificates_auth-default";
pub const ACME_CHALLENGES_BUCKET: &str = "cp_acme_challenges_auth-default";
pub const ACME_CHALLENGE_READINESS_BUCKET: &str = "cp_acme_challenge_readiness_auth-default";
pub const LOCKS_BUCKET: &str = "cp_locks_auth-default";
pub const COORDINATOR_LEASE_BUCKET: &str = "cp_coordinator_lease_auth-default";
const LEASE_DELETE_MARKER_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatsAssetRole {
    AuthorityLocal,
    InstallationRoot,
}

impl NatsAssetRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthorityLocal => "authority_local",
            Self::InstallationRoot => "installation_root",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NatsAssetSpec {
    pub name: &'static str,
    pub kind: &'static str,
    pub role: NatsAssetRole,
}

pub const NATS_STREAM_ASSETS: &[NatsAssetSpec] = &[
    NatsAssetSpec {
        name: DEPLOY_COMMITS_STREAM,
        kind: "stream",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: ROUTING_EVENTS_STREAM,
        kind: "stream",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: REVISIONS_STREAM,
        kind: "stream",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: CERT_JOBS_STREAM,
        kind: "stream",
        role: NatsAssetRole::AuthorityLocal,
    },
];

pub const NATS_KV_ASSETS: &[NatsAssetSpec] = &[
    NatsAssetSpec {
        name: AUTHORITIES_BUCKET,
        kind: "kv",
        role: NatsAssetRole::InstallationRoot,
    },
    NatsAssetSpec {
        name: REGIONS_BUCKET,
        kind: "kv",
        role: NatsAssetRole::InstallationRoot,
    },
    NatsAssetSpec {
        name: MACHINES_BUCKET,
        kind: "kv",
        role: NatsAssetRole::InstallationRoot,
    },
    NatsAssetSpec {
        name: GATEWAYS_BUCKET,
        kind: "kv",
        role: NatsAssetRole::InstallationRoot,
    },
    NatsAssetSpec {
        name: DNS_BUCKET,
        kind: "kv",
        role: NatsAssetRole::InstallationRoot,
    },
    NatsAssetSpec {
        name: INVITES_BUCKET,
        kind: "kv",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: DEPLOY_STATUS_BUCKET,
        kind: "kv",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: PARTICIPANTS_BUCKET,
        kind: "kv",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: INSTANCES_BUCKET,
        kind: "kv",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: ACME_ACCOUNTS_BUCKET,
        kind: "kv",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: CERTIFICATES_BUCKET,
        kind: "kv",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: ACME_CHALLENGES_BUCKET,
        kind: "kv",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: ACME_CHALLENGE_READINESS_BUCKET,
        kind: "kv",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: LOCKS_BUCKET,
        kind: "kv",
        role: NatsAssetRole::AuthorityLocal,
    },
    NatsAssetSpec {
        name: COORDINATOR_LEASE_BUCKET,
        kind: "kv",
        role: NatsAssetRole::AuthorityLocal,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetPolicy {
    pub storage_nodes: usize,
    pub replica_preference: ReplicaPreference,
}

impl AssetPolicy {
    #[must_use]
    pub fn replicas(self) -> usize {
        desired_replicas(self.storage_nodes, self.replica_preference)
    }
}

#[derive(Debug, Clone)]
pub struct AssetConfigs {
    pub deploy_commits: stream::Config,
    pub route_journal: stream::Config,
    pub revisions: stream::Config,
    pub cert_jobs: stream::Config,
    pub authority_durable_kv: Vec<kv::Config>,
    pub root_durable_kv: Vec<kv::Config>,
    pub authority_lease_kv: Vec<kv::Config>,
}

#[must_use]
pub fn asset_configs(policy: AssetPolicy) -> AssetConfigs {
    asset_configs_in(&NatsScope::default(), policy)
}

#[must_use]
pub fn asset_configs_in(scope: &NatsScope, policy: AssetPolicy) -> AssetConfigs {
    let replicas = policy.replicas();
    AssetConfigs {
        deploy_commits: deploy_commits_stream(scope, replicas),
        route_journal: route_journal_stream(scope, replicas),
        revisions: revisions_stream(scope, replicas),
        cert_jobs: cert_jobs_stream(scope, replicas),
        authority_durable_kv: authority_durable_buckets(replicas),
        root_durable_kv: root_durable_buckets(replicas),
        authority_lease_kv: authority_lease_buckets(replicas),
    }
}

pub async fn ensure_assets(js: &jetstream::Context, policy: AssetPolicy) -> Result<()> {
    ensure_assets_in(js, &NatsScope::default(), policy).await
}

pub async fn ensure_assets_in(
    js: &jetstream::Context,
    scope: &NatsScope,
    policy: AssetPolicy,
) -> Result<()> {
    let configs = asset_configs_in(scope, policy);
    ensure_stream(js, configs.deploy_commits).await?;
    ensure_stream(js, configs.route_journal).await?;
    ensure_stream(js, configs.revisions).await?;
    ensure_stream(js, configs.cert_jobs).await?;
    for config in configs
        .authority_durable_kv
        .into_iter()
        .chain(configs.root_durable_kv)
        .chain(configs.authority_lease_kv)
    {
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

fn deploy_commits_stream(scope: &NatsScope, replicas: usize) -> stream::Config {
    stream::Config {
        name: DEPLOY_COMMITS_STREAM.into(),
        subjects: vec![subjects::deploy_commit_filter_in(scope)],
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

fn route_journal_stream(scope: &NatsScope, replicas: usize) -> stream::Config {
    stream::Config {
        name: ROUTING_EVENTS_STREAM.into(),
        subjects: vec![subjects::route_journal_filter_in(scope)],
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

fn revisions_stream(scope: &NatsScope, replicas: usize) -> stream::Config {
    stream::Config {
        name: REVISIONS_STREAM.into(),
        subjects: vec![subjects::revision_filter_in(scope)],
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        max_messages_per_subject: 1,
        discard: stream::DiscardPolicy::New,
        duplicate_window: Duration::from_secs(60 * 60),
        ..Default::default()
    }
}

fn cert_jobs_stream(scope: &NatsScope, replicas: usize) -> stream::Config {
    stream::Config {
        name: CERT_JOBS_STREAM.into(),
        subjects: vec![subjects::cert_work_filter_in(scope)],
        retention: stream::RetentionPolicy::WorkQueue,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        duplicate_window: Duration::from_secs(60 * 60),
        allow_message_schedules: true,
        ..Default::default()
    }
}

fn authority_durable_buckets(replicas: usize) -> Vec<kv::Config> {
    [
        INVITES_BUCKET,
        DEPLOY_STATUS_BUCKET,
        PARTICIPANTS_BUCKET,
        INSTANCES_BUCKET,
        ACME_ACCOUNTS_BUCKET,
        CERTIFICATES_BUCKET,
        ACME_CHALLENGES_BUCKET,
        ACME_CHALLENGE_READINESS_BUCKET,
    ]
    .into_iter()
    .map(|bucket| kv::Config {
        bucket: bucket.into(),
        history: 1,
        max_age: Duration::ZERO,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        ..Default::default()
    })
    .collect()
}

fn root_durable_buckets(replicas: usize) -> Vec<kv::Config> {
    [
        AUTHORITIES_BUCKET,
        REGIONS_BUCKET,
        MACHINES_BUCKET,
        GATEWAYS_BUCKET,
        DNS_BUCKET,
    ]
    .into_iter()
    .map(|bucket| kv::Config {
        bucket: bucket.into(),
        history: 1,
        max_age: Duration::ZERO,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        ..Default::default()
    })
    .collect()
}

fn authority_lease_buckets(replicas: usize) -> Vec<kv::Config> {
    [LOCKS_BUCKET, COORDINATOR_LEASE_BUCKET]
        .into_iter()
        .map(|bucket| kv::Config {
            bucket: bucket.into(),
            history: 1,
            storage: stream::StorageType::File,
            num_replicas: replicas,
            limit_markers: Some(LEASE_DELETE_MARKER_TTL),
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_commit_stream_is_unpruned_and_not_collapsed() {
        let config = asset_configs(AssetPolicy {
            storage_nodes: 3,
            replica_preference: ReplicaPreference::Three,
        })
        .deploy_commits;
        assert_eq!(config.retention, stream::RetentionPolicy::Limits);
        assert_eq!(config.max_age, Duration::ZERO);
        assert_eq!(config.max_messages_per_subject, -1);
        assert_eq!(config.num_replicas, 3);
    }

    #[test]
    fn route_journal_stream_allows_atomic_publish() {
        let config = asset_configs(AssetPolicy {
            storage_nodes: 3,
            replica_preference: ReplicaPreference::Three,
        })
        .route_journal;
        assert_eq!(config.name, ROUTING_EVENTS_STREAM);
        assert_eq!(
            config.subjects,
            vec!["ployz.v1.local.auth-default.route.journal.>".to_string()]
        );
        assert_eq!(config.retention, stream::RetentionPolicy::Limits);
        assert_eq!(config.num_replicas, 3);
        assert!(config.allow_direct);
        assert!(config.allow_atomic_publish);
    }

    #[test]
    fn cert_jobs_stream_allows_scheduled_messages() {
        let config = asset_configs(AssetPolicy {
            storage_nodes: 3,
            replica_preference: ReplicaPreference::Three,
        })
        .cert_jobs;
        assert_eq!(config.name, CERT_JOBS_STREAM);
        assert_eq!(
            config.subjects,
            vec!["ployz.v1.local.auth-default.work.cert.>".to_string()]
        );
        assert_eq!(config.retention, stream::RetentionPolicy::WorkQueue);
        assert_eq!(config.num_replicas, 3);
        assert_eq!(config.duplicate_window, Duration::from_secs(60 * 60));
        assert!(config.allow_message_schedules);
    }

    #[test]
    fn durable_buckets_have_no_ttl() {
        let configs = asset_configs(AssetPolicy {
            storage_nodes: 2,
            replica_preference: ReplicaPreference::Default,
        });
        assert_eq!(configs.deploy_commits.num_replicas, 1);
        for bucket in configs
            .authority_durable_kv
            .into_iter()
            .chain(configs.root_durable_kv)
        {
            assert_eq!(bucket.max_age, Duration::ZERO);
            assert_eq!(bucket.history, 1);
        }
    }

    #[test]
    fn lease_buckets_enable_per_message_ttl() {
        let configs = asset_configs(AssetPolicy {
            storage_nodes: 3,
            replica_preference: ReplicaPreference::Three,
        });

        for bucket in configs.authority_lease_kv {
            assert_eq!(bucket.history, 1);
            assert_eq!(bucket.num_replicas, 3);
            assert_eq!(bucket.limit_markers, Some(LEASE_DELETE_MARKER_TTL));
        }
    }

    #[test]
    fn asset_configs_split_authority_and_installation_root_kv() {
        let configs = asset_configs(AssetPolicy {
            storage_nodes: 3,
            replica_preference: ReplicaPreference::Three,
        });

        assert!(
            configs
                .root_durable_kv
                .iter()
                .any(|bucket| bucket.bucket == MACHINES_BUCKET)
        );
        assert!(
            configs
                .authority_durable_kv
                .iter()
                .any(|bucket| bucket.bucket == PARTICIPANTS_BUCKET)
        );
        assert!(
            NATS_KV_ASSETS
                .iter()
                .any(|asset| asset.name == MACHINES_BUCKET
                    && asset.role == NatsAssetRole::InstallationRoot)
        );
    }

    #[test]
    fn asset_configs_use_provided_scope_for_subject_filters() {
        let scope = NatsScope::new(
            ployz_types::model::InstallationId("inst-acme".into()),
            ployz_types::model::AuthorityId("auth-sin".into()),
        );
        let configs = asset_configs_in(
            &scope,
            AssetPolicy {
                storage_nodes: 3,
                replica_preference: ReplicaPreference::Three,
            },
        );

        assert_eq!(
            configs.deploy_commits.subjects,
            vec!["ployz.v1.inst-acme.auth-sin.cp.deploy.commit.>".to_string()]
        );
        assert_eq!(
            configs.route_journal.subjects,
            vec!["ployz.v1.inst-acme.auth-sin.route.journal.>".to_string()]
        );
        assert_eq!(
            configs.cert_jobs.subjects,
            vec!["ployz.v1.inst-acme.auth-sin.work.cert.>".to_string()]
        );
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
}
