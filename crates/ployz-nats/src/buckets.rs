use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::kv;
use async_nats::jetstream::stream;
use ployz_types::error::{Error, Result};

use crate::role::{ReplicaPreference, desired_replicas};
use crate::subjects::{CERT_JOBS_STREAM, DEPLOY_COMMITS_STREAM, REVISIONS_STREAM};

pub const MACHINES_BUCKET: &str = "machines";
pub const INVITES_BUCKET: &str = "invites";
pub const DEPLOY_STATUS_BUCKET: &str = "deploy_status";
pub const INSTANCES_BUCKET: &str = "instances";
pub const ACME_ACCOUNTS_BUCKET: &str = "acme_accounts";
pub const CERTIFICATES_BUCKET: &str = "certificates";
pub const ACME_CHALLENGES_BUCKET: &str = "acme_challenges";
pub const ACME_CHALLENGE_READINESS_BUCKET: &str = "acme_challenge_readiness";
pub const LOCKS_BUCKET: &str = "locks";
pub const COORDINATOR_LEASE_BUCKET: &str = "coordinator_lease";

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
    pub revisions: stream::Config,
    pub cert_jobs: stream::Config,
    pub durable_kv: Vec<kv::Config>,
    pub lease_kv: Vec<kv::Config>,
}

#[must_use]
pub fn asset_configs(policy: AssetPolicy) -> AssetConfigs {
    let replicas = policy.replicas();
    AssetConfigs {
        deploy_commits: deploy_commits_stream(replicas),
        revisions: revisions_stream(replicas),
        cert_jobs: cert_jobs_stream(replicas),
        durable_kv: durable_buckets(replicas),
        lease_kv: lease_buckets(replicas),
    }
}

pub async fn ensure_assets(js: &jetstream::Context, policy: AssetPolicy) -> Result<()> {
    let configs = asset_configs(policy);
    ensure_stream(js, configs.deploy_commits).await?;
    ensure_stream(js, configs.revisions).await?;
    ensure_stream(js, configs.cert_jobs).await?;
    for config in configs.durable_kv.into_iter().chain(configs.lease_kv) {
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
        Ok(_) => Ok(()),
        Err(_) => js
            .create_key_value(config)
            .await
            .map(|_| ())
            .map_err(|error| Error::operation("nats_ensure_kv", format!("{error:?}"))),
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
        ..Default::default()
    }
}

fn durable_buckets(replicas: usize) -> Vec<kv::Config> {
    [
        MACHINES_BUCKET,
        INVITES_BUCKET,
        DEPLOY_STATUS_BUCKET,
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

fn lease_buckets(replicas: usize) -> Vec<kv::Config> {
    [LOCKS_BUCKET, COORDINATOR_LEASE_BUCKET]
        .into_iter()
        .map(|bucket| kv::Config {
            bucket: bucket.into(),
            history: 1,
            storage: stream::StorageType::File,
            num_replicas: replicas,
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
            storage_candidates: 3,
            replica_preference: ReplicaPreference::Default,
        })
        .deploy_commits;
        assert_eq!(config.retention, stream::RetentionPolicy::Limits);
        assert_eq!(config.max_age, Duration::ZERO);
        assert_eq!(config.max_messages_per_subject, -1);
        assert_eq!(config.num_replicas, 3);
    }

    #[test]
    fn durable_buckets_have_no_ttl() {
        let configs = asset_configs(AssetPolicy {
            storage_candidates: 2,
            replica_preference: ReplicaPreference::Default,
        });
        assert_eq!(configs.deploy_commits.num_replicas, 1);
        for bucket in configs.durable_kv {
            assert_eq!(bucket.max_age, Duration::ZERO);
            assert_eq!(bucket.history, 1);
        }
    }
}
