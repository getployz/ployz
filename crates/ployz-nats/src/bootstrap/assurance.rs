use super::resources::PlannedResource;
use super::{
    BootstrapPlan, BootstrapResourceAction, BootstrapResourceKind, BootstrapResourceRef,
    BootstrapResourceRefusal, ExistingResources, ResourceReplicas,
};
use crate::kv::{KvBucketSpec, NatsIoTimeout, with_io_timeout};
use crate::objects::ObjectBucketSpec;
use crate::streams::{DiscardPolicy, RetentionPolicy, StorageBackend, StreamSpec};
use async_nats::jetstream;
use async_nats::jetstream::ErrorCode;
use async_nats::jetstream::context::{GetStreamError, GetStreamErrorKind};
use std::fmt;
use std::future::Future;

pub async fn assure_nats_resources(
    jetstream: &jetstream::Context,
    plan: &BootstrapPlan,
) -> Result<(), BootstrapAssuranceError> {
    for bucket in &plan.kv_buckets {
        assure_kv_bucket(jetstream, bucket).await?;
    }
    for stream in &plan.streams {
        assure_stream(jetstream, stream).await?;
    }
    for bucket in &plan.object_buckets {
        assure_object_bucket(jetstream, bucket).await?;
    }

    Ok(())
}

async fn assure_kv_bucket(
    jetstream: &jetstream::Context,
    expected: &KvBucketSpec,
) -> Result<(), BootstrapAssuranceError> {
    let existing = load_existing_kv_bucket(jetstream, expected).await?;
    match kv_bucket_action(expected, existing.as_ref()).action {
        BootstrapResourceAction::Adopt => Ok(()),
        BootstrapResourceAction::Refuse { reason } => {
            Err(BootstrapAssuranceError::RefuseResource {
                resource: ResourceLookup::KvBucket(expected.name).resource_ref(),
                reason,
            })
        }
        BootstrapResourceAction::Create => match create_kv_bucket(jetstream, expected).await {
            Ok(()) => Ok(()),
            Err(error) => adopt_after_create_error(
                error,
                load_existing_kv_bucket(jetstream, expected).await?,
                |observed| kv_bucket_action(expected, observed),
            ),
        },
    }
}

async fn assure_stream(
    jetstream: &jetstream::Context,
    expected: &StreamSpec,
) -> Result<(), BootstrapAssuranceError> {
    let existing = load_existing_stream(jetstream, expected).await?;
    match stream_action(expected, existing.as_ref()).action {
        BootstrapResourceAction::Adopt => Ok(()),
        BootstrapResourceAction::Refuse { reason } => {
            Err(BootstrapAssuranceError::RefuseResource {
                resource: ResourceLookup::Stream(expected.name).resource_ref(),
                reason,
            })
        }
        BootstrapResourceAction::Create => match create_stream(jetstream, expected).await {
            Ok(()) => Ok(()),
            Err(error) => adopt_after_create_error(
                error,
                load_existing_stream(jetstream, expected).await?,
                |observed| stream_action(expected, observed),
            ),
        },
    }
}

async fn assure_object_bucket(
    jetstream: &jetstream::Context,
    expected: &ObjectBucketSpec,
) -> Result<(), BootstrapAssuranceError> {
    let existing = load_existing_object_bucket(jetstream, expected).await?;
    match object_bucket_action(expected, existing.as_ref()).action {
        BootstrapResourceAction::Adopt => Ok(()),
        BootstrapResourceAction::Refuse { reason } => {
            Err(BootstrapAssuranceError::RefuseResource {
                resource: ResourceLookup::ObjectBucket(expected.name).resource_ref(),
                reason,
            })
        }
        BootstrapResourceAction::Create => match create_object_bucket(jetstream, expected).await {
            Ok(()) => Ok(()),
            Err(error) => adopt_after_create_error(
                error,
                load_existing_object_bucket(jetstream, expected).await?,
                |observed| object_bucket_action(expected, observed),
            ),
        },
    }
}

fn adopt_after_create_error<T>(
    create_error: BootstrapAssuranceError,
    observed: Option<T>,
    decide: impl FnOnce(Option<&T>) -> super::BootstrapResourceDecision,
) -> Result<(), BootstrapAssuranceError> {
    match decide(observed.as_ref()).action {
        BootstrapResourceAction::Adopt => Ok(()),
        BootstrapResourceAction::Create | BootstrapResourceAction::Refuse { .. } => {
            Err(create_error)
        }
    }
}

fn kv_bucket_action(
    expected: &KvBucketSpec,
    observed: Option<&KvBucketSpec>,
) -> super::BootstrapResourceDecision {
    let existing = ExistingResources {
        kv_buckets: observed.cloned().into_iter().collect(),
        ..ExistingResources::default()
    };
    PlannedResource::KvBucket(expected).bootstrap_decision(&existing.view())
}

fn stream_action(
    expected: &StreamSpec,
    observed: Option<&StreamSpec>,
) -> super::BootstrapResourceDecision {
    let existing = ExistingResources {
        streams: observed.cloned().into_iter().collect(),
        ..ExistingResources::default()
    };
    PlannedResource::Stream(expected).bootstrap_decision(&existing.view())
}

fn object_bucket_action(
    expected: &ObjectBucketSpec,
    observed: Option<&ObjectBucketSpec>,
) -> super::BootstrapResourceDecision {
    let existing = ExistingResources {
        object_buckets: observed.cloned().into_iter().collect(),
        ..ExistingResources::default()
    };
    PlannedResource::ObjectBucket(expected).bootstrap_decision(&existing.view())
}

async fn load_existing_kv_bucket(
    jetstream: &jetstream::Context,
    expected: &KvBucketSpec,
) -> Result<Option<KvBucketSpec>, BootstrapAssuranceError> {
    let Some(config) =
        load_existing_stream_config(jetstream, ResourceLookup::KvBucket(expected.name)).await?
    else {
        return Ok(None);
    };

    Ok(Some(observed_kv_bucket(
        expected.name,
        observed_replicas(config.num_replicas)?,
    )))
}

fn observed_kv_bucket(name: &'static str, replicas: ResourceReplicas) -> KvBucketSpec {
    KvBucketSpec::new(name).with_observed_replicas(replicas)
}

async fn load_existing_stream(
    jetstream: &jetstream::Context,
    expected: &StreamSpec,
) -> Result<Option<StreamSpec>, BootstrapAssuranceError> {
    let Some(config) =
        load_existing_stream_config(jetstream, ResourceLookup::Stream(expected.name)).await?
    else {
        return Ok(None);
    };

    Ok(Some(observed_stream(
        expected.name,
        config.subjects,
        retention_from_nats(config.retention)?,
        storage_from_nats(config.storage)?,
        observed_replicas(config.num_replicas)?,
        discard_from_nats(config.discard)?,
    )))
}

fn observed_stream(
    name: &'static str,
    subjects: Vec<String>,
    retention: RetentionPolicy,
    storage: StorageBackend,
    replicas: ResourceReplicas,
    discard: DiscardPolicy,
) -> StreamSpec {
    let mut spec = StreamSpec::new(name, subjects, retention, storage, discard);
    spec = spec.with_observed_replicas(replicas);
    spec
}

async fn load_existing_object_bucket(
    jetstream: &jetstream::Context,
    expected: &ObjectBucketSpec,
) -> Result<Option<ObjectBucketSpec>, BootstrapAssuranceError> {
    let Some(config) =
        load_existing_stream_config(jetstream, ResourceLookup::ObjectBucket(expected.name)).await?
    else {
        return Ok(None);
    };

    Ok(Some(observed_object_bucket(
        expected.name,
        observed_replicas(config.num_replicas)?,
    )))
}

fn observed_object_bucket(name: &'static str, replicas: ResourceReplicas) -> ObjectBucketSpec {
    ObjectBucketSpec::new(name).with_observed_replicas(replicas)
}

fn observed_replicas(value: usize) -> Result<ResourceReplicas, BootstrapAssuranceError> {
    ResourceReplicas::observed(value).map_err(|invalid| {
        BootstrapAssuranceError::UnsupportedObservedResourceShape {
            field: "replicas",
            value: invalid.value.to_string(),
        }
    })
}

async fn load_existing_stream_config(
    jetstream: &jetstream::Context,
    resource: ResourceLookup,
) -> Result<Option<jetstream::stream::Config>, BootstrapAssuranceError> {
    let stream_name = resource.stream_name();
    match lookup_stream_config(jetstream, &stream_name).await {
        Ok(stream) => Ok(Some(stream.cached_info().config.clone())),
        Err(error) if is_stream_not_found(&error) => Ok(None),
        Err(error) => Err(BootstrapAssuranceError::LookupResource {
            resource: resource.resource_ref(),
            stream_name,
            source: BootstrapIoError::from_stream_lookup(error),
        }),
    }
}

async fn lookup_stream_config(
    jetstream: &jetstream::Context,
    stream_name: &str,
) -> Result<jetstream::stream::Stream, BootstrapStreamLookupError> {
    with_io_timeout("stream lookup", jetstream.get_stream(stream_name))
        .await?
        .map_err(BootstrapStreamLookupError::Nats)
}

fn is_stream_not_found(error: &BootstrapStreamLookupError) -> bool {
    matches!(
        error,
        BootstrapStreamLookupError::Nats(source)
            if matches!(
                source.kind(),
                GetStreamErrorKind::JetStream(error)
                    if error.kind() == ErrorCode::STREAM_NOT_FOUND
            )
    )
}

#[derive(Debug)]
enum BootstrapStreamLookupError {
    Timeout,
    Nats(GetStreamError),
}

impl From<NatsIoTimeout> for BootstrapStreamLookupError {
    fn from(NatsIoTimeout { operation: _ }: NatsIoTimeout) -> Self {
        Self::Timeout
    }
}

impl BootstrapIoError {
    fn from_stream_lookup(error: BootstrapStreamLookupError) -> Self {
        match error {
            BootstrapStreamLookupError::Timeout => Self::Timeout {
                operation: "stream lookup",
            },
            BootstrapStreamLookupError::Nats(source) => Self::Nats {
                source: source.to_string(),
            },
        }
    }
}

async fn create_kv_bucket(
    jetstream: &jetstream::Context,
    bucket: &KvBucketSpec,
) -> Result<(), BootstrapAssuranceError> {
    with_bootstrap_timeout(
        "kv bucket create",
        jetstream.create_key_value(jetstream::kv::Config {
            bucket: bucket.name.to_owned(),
            storage: jetstream::stream::StorageType::File,
            num_replicas: bucket.replicas().as_usize(),
            ..Default::default()
        }),
    )
    .await
    .map(|_| ())
    .map_err(|source| BootstrapAssuranceError::CreateKvBucket {
        bucket: bucket.name,
        source,
    })
}

async fn create_stream(
    jetstream: &jetstream::Context,
    stream: &StreamSpec,
) -> Result<(), BootstrapAssuranceError> {
    with_bootstrap_timeout(
        "stream create",
        jetstream.create_stream(jetstream::stream::Config {
            name: stream.name.to_owned(),
            subjects: stream.subjects.clone(),
            retention: nats_retention(stream.retention),
            storage: nats_storage(stream.storage),
            num_replicas: stream.replicas().as_usize(),
            discard: nats_discard(stream.discard),
            ..Default::default()
        }),
    )
    .await
    .map(|_| ())
    .map_err(|source| BootstrapAssuranceError::CreateStream {
        stream: stream.name,
        source,
    })
}

async fn create_object_bucket(
    jetstream: &jetstream::Context,
    bucket: &ObjectBucketSpec,
) -> Result<(), BootstrapAssuranceError> {
    with_bootstrap_timeout(
        "object bucket create",
        jetstream.create_object_store(jetstream::object_store::Config {
            bucket: bucket.name.to_owned(),
            storage: jetstream::stream::StorageType::File,
            num_replicas: bucket.replicas().as_usize(),
            ..Default::default()
        }),
    )
    .await
    .map(|_| ())
    .map_err(|source| BootstrapAssuranceError::CreateObjectBucket {
        bucket: bucket.name,
        source,
    })
}

async fn with_bootstrap_timeout<T, E: fmt::Display>(
    operation: &'static str,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, BootstrapIoError> {
    with_io_timeout(operation, future)
        .await?
        .map_err(|error| BootstrapIoError::Nats {
            source: error.to_string(),
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapIoError {
    Timeout { operation: &'static str },
    Nats { source: String },
}

impl From<NatsIoTimeout> for BootstrapIoError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

impl fmt::Display for BootstrapIoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
            Self::Nats { source } => formatter.write_str(source),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapAssuranceError {
    LookupResource {
        resource: BootstrapResourceRef,
        stream_name: String,
        source: BootstrapIoError,
    },
    RefuseResource {
        resource: BootstrapResourceRef,
        reason: BootstrapResourceRefusal,
    },
    UnsupportedObservedResourceShape {
        field: &'static str,
        value: String,
    },
    CreateKvBucket {
        bucket: &'static str,
        source: BootstrapIoError,
    },
    CreateStream {
        stream: &'static str,
        source: BootstrapIoError,
    },
    CreateObjectBucket {
        bucket: &'static str,
        source: BootstrapIoError,
    },
}

impl fmt::Display for BootstrapAssuranceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LookupResource {
                resource,
                stream_name,
                source,
            } => write!(
                formatter,
                "failed to inspect {:?} {} through stream {stream_name}: {source}",
                resource.kind, resource.name
            ),
            Self::RefuseResource { resource, reason } => {
                write!(
                    formatter,
                    "refusing to adopt {:?} {}: {reason:?}",
                    resource.kind, resource.name
                )
            }
            Self::UnsupportedObservedResourceShape { field, value } => {
                write!(
                    formatter,
                    "observed NATS resource has unsupported {field}: {value}"
                )
            }
            Self::CreateKvBucket { bucket, source } => {
                write!(formatter, "failed to assure KV bucket {bucket}: {source}")
            }
            Self::CreateStream { stream, source } => {
                write!(formatter, "failed to assure stream {stream}: {source}")
            }
            Self::CreateObjectBucket { bucket, source } => {
                write!(
                    formatter,
                    "failed to assure Object Store bucket {bucket}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for BootstrapAssuranceError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceLookup {
    KvBucket(&'static str),
    Stream(&'static str),
    ObjectBucket(&'static str),
}

impl ResourceLookup {
    fn stream_name(self) -> String {
        match self {
            Self::KvBucket(name) => format!("KV_{name}"),
            Self::Stream(name) => name.to_owned(),
            Self::ObjectBucket(name) => format!("OBJ_{name}"),
        }
    }

    const fn resource_ref(self) -> BootstrapResourceRef {
        match self {
            Self::KvBucket(name) => BootstrapResourceRef {
                kind: BootstrapResourceKind::KvBucket,
                name,
            },
            Self::Stream(name) => BootstrapResourceRef {
                kind: BootstrapResourceKind::Stream,
                name,
            },
            Self::ObjectBucket(name) => BootstrapResourceRef {
                kind: BootstrapResourceKind::ObjectBucket,
                name,
            },
        }
    }
}

fn nats_retention(retention: RetentionPolicy) -> jetstream::stream::RetentionPolicy {
    match retention {
        RetentionPolicy::Limits => jetstream::stream::RetentionPolicy::Limits,
    }
}

fn nats_storage(storage: StorageBackend) -> jetstream::stream::StorageType {
    match storage {
        StorageBackend::File => jetstream::stream::StorageType::File,
    }
}

fn nats_discard(discard: DiscardPolicy) -> jetstream::stream::DiscardPolicy {
    match discard {
        DiscardPolicy::Old => jetstream::stream::DiscardPolicy::Old,
        DiscardPolicy::New => jetstream::stream::DiscardPolicy::New,
    }
}

fn retention_from_nats(
    retention: jetstream::stream::RetentionPolicy,
) -> Result<RetentionPolicy, BootstrapAssuranceError> {
    match retention {
        jetstream::stream::RetentionPolicy::Limits => Ok(RetentionPolicy::Limits),
        jetstream::stream::RetentionPolicy::Interest
        | jetstream::stream::RetentionPolicy::WorkQueue => {
            Err(BootstrapAssuranceError::UnsupportedObservedResourceShape {
                field: "retention",
                value: format!("{retention:?}"),
            })
        }
    }
}

fn storage_from_nats(
    storage: jetstream::stream::StorageType,
) -> Result<StorageBackend, BootstrapAssuranceError> {
    match storage {
        jetstream::stream::StorageType::File => Ok(StorageBackend::File),
        jetstream::stream::StorageType::Memory => {
            Err(BootstrapAssuranceError::UnsupportedObservedResourceShape {
                field: "storage",
                value: format!("{storage:?}"),
            })
        }
    }
}

fn discard_from_nats(
    discard: jetstream::stream::DiscardPolicy,
) -> Result<DiscardPolicy, BootstrapAssuranceError> {
    match discard {
        jetstream::stream::DiscardPolicy::Old => Ok(DiscardPolicy::Old),
        jetstream::stream::DiscardPolicy::New => Ok(DiscardPolicy::New),
    }
}
