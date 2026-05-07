use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::kv;
use async_nats::jetstream::stream;
use ployz_types::error::{Error, Result};

use crate::subjects::{self, NatsScope};

const LEASE_DELETE_MARKER_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatsAssetScope {
    AuthorityLocal,
    InstallationRoot,
}

impl NatsAssetScope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthorityLocal => "authority_local",
            Self::InstallationRoot => "installation_root",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsAssetSpec {
    pub name: String,
    pub kind: &'static str,
    pub scope: NatsAssetScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NatsAssetNames {
    pub(crate) deploy_commits_stream: String,
    pub(crate) routing_events_stream: String,
    pub(crate) cert_jobs_stream: String,
    pub(crate) machines_bucket: String,
    pub(crate) invites_bucket: String,
    pub(crate) deploy_status_bucket: String,
    pub(crate) instances_bucket: String,
    pub(crate) acme_accounts_bucket: String,
    pub(crate) certificates_bucket: String,
    pub(crate) acme_challenges_bucket: String,
    pub(crate) acme_challenge_readiness_bucket: String,
    pub(crate) locks_bucket: String,
}

impl NatsAssetNames {
    #[must_use]
    pub(crate) fn new(scope: &NatsScope) -> Self {
        let installation = subjects::subject_token(scope.installation().as_str());
        let authority = subjects::subject_token(scope.authority().as_str());
        Self {
            deploy_commits_stream: format!("cp_deploy_commits_{authority}"),
            routing_events_stream: format!("routing_events_{authority}"),
            cert_jobs_stream: format!("work_cert_{authority}"),
            machines_bucket: format!("machines_{installation}"),
            invites_bucket: format!("cp_invites_{authority}"),
            deploy_status_bucket: format!("cp_deploy_status_{authority}"),
            instances_bucket: format!("cp_instances_{authority}"),
            acme_accounts_bucket: format!("cp_acme_accounts_{authority}"),
            certificates_bucket: format!("cp_certificates_{authority}"),
            acme_challenges_bucket: format!("cp_acme_challenges_{authority}"),
            acme_challenge_readiness_bucket: format!("cp_acme_challenge_readiness_{authority}"),
            locks_bucket: format!("cp_locks_{authority}"),
        }
    }

    #[must_use]
    pub(crate) fn stream_assets(&self) -> Vec<NatsAssetSpec> {
        vec![
            NatsAssetSpec {
                name: self.deploy_commits_stream.clone(),
                kind: "stream",
                scope: NatsAssetScope::AuthorityLocal,
            },
            NatsAssetSpec {
                name: self.routing_events_stream.clone(),
                kind: "stream",
                scope: NatsAssetScope::AuthorityLocal,
            },
            NatsAssetSpec {
                name: self.cert_jobs_stream.clone(),
                kind: "stream",
                scope: NatsAssetScope::AuthorityLocal,
            },
        ]
    }

    #[must_use]
    pub(crate) fn kv_assets(&self) -> Vec<NatsAssetSpec> {
        vec![
            NatsAssetSpec {
                name: self.machines_bucket.clone(),
                kind: "kv",
                scope: NatsAssetScope::InstallationRoot,
            },
            NatsAssetSpec {
                name: self.invites_bucket.clone(),
                kind: "kv",
                scope: NatsAssetScope::AuthorityLocal,
            },
            NatsAssetSpec {
                name: self.deploy_status_bucket.clone(),
                kind: "kv",
                scope: NatsAssetScope::AuthorityLocal,
            },
            NatsAssetSpec {
                name: self.instances_bucket.clone(),
                kind: "kv",
                scope: NatsAssetScope::AuthorityLocal,
            },
            NatsAssetSpec {
                name: self.acme_accounts_bucket.clone(),
                kind: "kv",
                scope: NatsAssetScope::AuthorityLocal,
            },
            NatsAssetSpec {
                name: self.certificates_bucket.clone(),
                kind: "kv",
                scope: NatsAssetScope::AuthorityLocal,
            },
            NatsAssetSpec {
                name: self.acme_challenges_bucket.clone(),
                kind: "kv",
                scope: NatsAssetScope::AuthorityLocal,
            },
            NatsAssetSpec {
                name: self.acme_challenge_readiness_bucket.clone(),
                kind: "kv",
                scope: NatsAssetScope::AuthorityLocal,
            },
            NatsAssetSpec {
                name: self.locks_bucket.clone(),
                kind: "kv",
                scope: NatsAssetScope::AuthorityLocal,
            },
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AssetPolicy {
    pub(crate) replicas: usize,
}

impl AssetPolicy {
    #[must_use]
    pub(crate) fn replicas(self) -> usize {
        self.replicas
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AssetConfigs {
    deploy_commits: stream::Config,
    routing_events: stream::Config,
    cert_jobs: stream::Config,
    authority_durable_kv: Vec<kv::Config>,
    root_durable_kv: Vec<kv::Config>,
    authority_lease_kv: Vec<kv::Config>,
}

#[must_use]
pub(crate) fn asset_configs_in(scope: &NatsScope, policy: AssetPolicy) -> AssetConfigs {
    let replicas = policy.replicas();
    let names = NatsAssetNames::new(scope);
    AssetConfigs {
        deploy_commits: deploy_commits_stream(&names, scope, replicas),
        routing_events: routing_events_stream(&names, scope, replicas),
        cert_jobs: cert_jobs_stream(&names, scope, replicas),
        authority_durable_kv: authority_durable_buckets(&names, replicas),
        root_durable_kv: root_durable_buckets(&names, replicas),
        authority_lease_kv: authority_lease_buckets(&names, replicas),
    }
}

pub(crate) async fn ensure_assets_in(
    js: &jetstream::Context,
    scope: &NatsScope,
    policy: AssetPolicy,
) -> Result<()> {
    let configs = asset_configs_in(scope, policy);
    ensure_stream(js, configs.deploy_commits).await?;
    ensure_stream(js, configs.routing_events).await?;
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

fn deploy_commits_stream(
    names: &NatsAssetNames,
    scope: &NatsScope,
    replicas: usize,
) -> stream::Config {
    stream::Config {
        name: names.deploy_commits_stream.clone(),
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

fn routing_events_stream(
    names: &NatsAssetNames,
    scope: &NatsScope,
    replicas: usize,
) -> stream::Config {
    stream::Config {
        name: names.routing_events_stream.clone(),
        subjects: vec![subjects::routing_event_filter_in(scope)],
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

fn cert_jobs_stream(names: &NatsAssetNames, scope: &NatsScope, replicas: usize) -> stream::Config {
    stream::Config {
        name: names.cert_jobs_stream.clone(),
        subjects: vec![
            subjects::cert_renewal_filter_in(scope),
            subjects::cert_renewal_schedule_filter_in(scope),
        ],
        retention: stream::RetentionPolicy::WorkQueue,
        storage: stream::StorageType::File,
        num_replicas: replicas,
        duplicate_window: Duration::from_secs(60 * 60),
        allow_message_schedules: true,
        ..Default::default()
    }
}

fn authority_durable_buckets(names: &NatsAssetNames, replicas: usize) -> Vec<kv::Config> {
    [
        names.invites_bucket.as_str(),
        names.deploy_status_bucket.as_str(),
        names.instances_bucket.as_str(),
        names.acme_accounts_bucket.as_str(),
        names.certificates_bucket.as_str(),
        names.acme_challenges_bucket.as_str(),
        names.acme_challenge_readiness_bucket.as_str(),
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

fn root_durable_buckets(names: &NatsAssetNames, replicas: usize) -> Vec<kv::Config> {
    [names.machines_bucket.as_str()]
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

fn authority_lease_buckets(names: &NatsAssetNames, replicas: usize) -> Vec<kv::Config> {
    [names.locks_bucket.as_str()]
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

    fn local_asset_configs(policy: AssetPolicy) -> AssetConfigs {
        asset_configs_in(&NatsScope::local_default(), policy)
    }

    #[test]
    fn deploy_commit_stream_is_unpruned_and_not_collapsed() {
        let config = local_asset_configs(AssetPolicy { replicas: 3 }).deploy_commits;
        assert_eq!(config.retention, stream::RetentionPolicy::Limits);
        assert_eq!(config.max_age, Duration::ZERO);
        assert_eq!(config.max_messages_per_subject, -1);
        assert_eq!(config.num_replicas, 3);
    }

    #[test]
    fn routing_events_stream_is_plain_event_stream() {
        let config = local_asset_configs(AssetPolicy { replicas: 3 }).routing_events;
        assert_eq!(
            config.name,
            NatsAssetNames::new(&NatsScope::local_default()).routing_events_stream
        );
        assert_eq!(
            config.subjects,
            vec!["ployz.v1.local.auth-default.routing.event.>".to_string()]
        );
        assert_eq!(config.retention, stream::RetentionPolicy::Limits);
        assert_eq!(config.num_replicas, 3);
        assert!(config.allow_direct);
        assert!(!config.allow_atomic_publish);
    }

    #[test]
    fn cert_jobs_stream_allows_scheduled_messages() {
        let config = local_asset_configs(AssetPolicy { replicas: 3 }).cert_jobs;
        assert_eq!(
            config.name,
            NatsAssetNames::new(&NatsScope::local_default()).cert_jobs_stream
        );
        assert_eq!(
            config.subjects,
            vec![
                "ployz.v1.local.auth-default.work.cert.renew.>".to_string(),
                "ployz.v1.local.auth-default.work.cert.schedule.>".to_string(),
            ]
        );
        assert_eq!(config.retention, stream::RetentionPolicy::WorkQueue);
        assert_eq!(config.num_replicas, 3);
        assert_eq!(config.duplicate_window, Duration::from_secs(60 * 60));
        assert!(config.allow_message_schedules);
    }

    #[test]
    fn durable_buckets_have_no_ttl() {
        let configs = local_asset_configs(AssetPolicy { replicas: 1 });
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
        let configs = local_asset_configs(AssetPolicy { replicas: 3 });

        for bucket in configs.authority_lease_kv {
            assert_eq!(bucket.history, 1);
            assert_eq!(bucket.num_replicas, 3);
            assert_eq!(bucket.limit_markers, Some(LEASE_DELETE_MARKER_TTL));
        }
    }

    #[test]
    fn asset_configs_keep_only_live_authority_and_installation_kv() {
        let configs = local_asset_configs(AssetPolicy { replicas: 3 });
        let names = NatsAssetNames::new(&NatsScope::local_default());

        assert!(
            configs
                .root_durable_kv
                .iter()
                .any(|bucket| bucket.bucket == names.machines_bucket)
        );
        assert!(
            configs
                .authority_durable_kv
                .iter()
                .any(|bucket| bucket.bucket == names.instances_bucket)
        );
        assert!(
            names
                .kv_assets()
                .iter()
                .any(|asset| asset.name == names.machines_bucket
                    && asset.scope == NatsAssetScope::InstallationRoot)
        );
        assert_eq!(configs.root_durable_kv.len(), 1);
    }

    #[test]
    fn asset_manifest_matches_ensured_streams_and_buckets() {
        let configs = local_asset_configs(AssetPolicy { replicas: 3 });
        let mut configured_streams = vec![
            configs.deploy_commits.name.as_str().to_string(),
            configs.routing_events.name.as_str().to_string(),
            configs.cert_jobs.name.as_str().to_string(),
        ];
        configured_streams.sort();
        let names = NatsAssetNames::new(&NatsScope::local_default());
        let mut manifest_streams = names
            .stream_assets()
            .into_iter()
            .map(|asset| {
                assert_eq!(asset.kind, "stream");
                assert_eq!(asset.scope, NatsAssetScope::AuthorityLocal);
                asset.name
            })
            .collect::<Vec<_>>();
        manifest_streams.sort();

        let mut configured_buckets = configs
            .authority_durable_kv
            .iter()
            .chain(&configs.root_durable_kv)
            .chain(&configs.authority_lease_kv)
            .map(|bucket| bucket.bucket.clone())
            .collect::<Vec<_>>();
        configured_buckets.sort();
        let mut manifest_buckets = names
            .kv_assets()
            .into_iter()
            .map(|asset| {
                assert_eq!(asset.kind, "kv");
                asset.name
            })
            .collect::<Vec<_>>();
        manifest_buckets.sort();

        assert_eq!(manifest_streams, configured_streams);
        assert_eq!(manifest_buckets, configured_buckets);
    }

    #[test]
    fn asset_configs_use_provided_scope_for_names_and_subject_filters() {
        let scope = NatsScope::new(
            ployz_types::model::InstallationId("inst-acme".into()),
            ployz_types::model::AuthorityId("auth-sin".into()),
        );
        let configs = asset_configs_in(&scope, AssetPolicy { replicas: 3 });

        assert_eq!(configs.deploy_commits.name, "cp_deploy_commits_auth-sin");
        assert_eq!(configs.routing_events.name, "routing_events_auth-sin");
        assert_eq!(configs.cert_jobs.name, "work_cert_auth-sin");
        assert!(
            configs
                .authority_durable_kv
                .iter()
                .any(|bucket| bucket.bucket == "cp_instances_auth-sin")
        );
        assert!(
            configs
                .root_durable_kv
                .iter()
                .any(|bucket| bucket.bucket == "machines_inst-acme")
        );

        assert_eq!(
            configs.deploy_commits.subjects,
            vec!["ployz.v1.inst-acme.auth-sin.cp.deploy.commit.>".to_string()]
        );
        assert_eq!(
            configs.routing_events.subjects,
            vec!["ployz.v1.inst-acme.auth-sin.routing.event.>".to_string()]
        );
        assert_eq!(
            configs.cert_jobs.subjects,
            vec![
                "ployz.v1.inst-acme.auth-sin.work.cert.renew.>".to_string(),
                "ployz.v1.inst-acme.auth-sin.work.cert.schedule.>".to_string(),
            ]
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
