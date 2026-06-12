use crate::kv::KvBucketSpec;
use crate::objects::ObjectBucketSpec;
use crate::streams::StreamSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapResourceKind {
    KvBucket,
    Stream,
    ObjectBucket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapResourceRef {
    pub kind: BootstrapResourceKind,
    pub name: &'static str,
}

impl BootstrapResourceRef {
    const fn kv_bucket(name: &'static str) -> Self {
        Self {
            kind: BootstrapResourceKind::KvBucket,
            name,
        }
    }

    const fn stream(name: &'static str) -> Self {
        Self {
            kind: BootstrapResourceKind::Stream,
            name,
        }
    }

    const fn object_bucket(name: &'static str) -> Self {
        Self {
            kind: BootstrapResourceKind::ObjectBucket,
            name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapResourceDecision {
    pub resource: BootstrapResourceRef,
    pub action: BootstrapResourceAction,
}

impl BootstrapResourceDecision {
    fn new(resource: BootstrapResourceRef, action: BootstrapResourceAction) -> Self {
        Self { resource, action }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapResourceAction {
    Create,
    Adopt,
    Refuse { reason: BootstrapResourceRefusal },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapResourceRefusal {
    MissingResource,
    ConfigurationDrift {
        field: &'static str,
        expected: String,
        observed: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResourceComparison {
    Missing,
    Unchanged,
    ShapeDrift { reason: BootstrapResourceRefusal },
}

impl ResourceComparison {
    fn bootstrap_action(self) -> BootstrapResourceAction {
        match self {
            Self::Missing => BootstrapResourceAction::Create,
            Self::Unchanged => BootstrapResourceAction::Adopt,
            Self::ShapeDrift { reason } => BootstrapResourceAction::Refuse { reason },
        }
    }
}

pub(crate) enum PlannedResource<'a> {
    KvBucket(&'a KvBucketSpec),
    Stream(&'a StreamSpec),
    ObjectBucket(&'a ObjectBucketSpec),
}

impl PlannedResource<'_> {
    pub(crate) fn bootstrap_decision(
        &self,
        existing: &ExistingResourceView<'_>,
    ) -> BootstrapResourceDecision {
        let (resource, comparison) = self.compare(existing);
        BootstrapResourceDecision::new(resource, comparison.bootstrap_action())
    }

    fn compare(
        &self,
        existing: &ExistingResourceView<'_>,
    ) -> (BootstrapResourceRef, ResourceComparison) {
        match self {
            Self::KvBucket(expected) => (
                BootstrapResourceRef::kv_bucket(expected.name),
                compare_kv_bucket(
                    expected,
                    existing
                        .kv_buckets
                        .iter()
                        .find(|observed| observed.name == expected.name),
                ),
            ),
            Self::Stream(expected) => (
                BootstrapResourceRef::stream(expected.name),
                compare_stream(
                    expected,
                    existing
                        .streams
                        .iter()
                        .find(|observed| observed.name == expected.name),
                ),
            ),
            Self::ObjectBucket(expected) => (
                BootstrapResourceRef::object_bucket(expected.name),
                compare_object_bucket(
                    expected,
                    existing
                        .object_buckets
                        .iter()
                        .find(|observed| observed.name == expected.name),
                ),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExistingResourceView<'a> {
    pub kv_buckets: &'a [KvBucketSpec],
    pub streams: &'a [StreamSpec],
    pub object_buckets: &'a [ObjectBucketSpec],
}

fn compare_kv_bucket(
    expected: &KvBucketSpec,
    observed: Option<&KvBucketSpec>,
) -> ResourceComparison {
    let Some(observed) = observed else {
        return ResourceComparison::Missing;
    };
    let KvBucketSpec {
        name: expected_name,
        replicas: expected_replicas,
    } = expected;
    let KvBucketSpec {
        name: observed_name,
        replicas: observed_replicas,
    } = observed;

    if expected_name != observed_name {
        return shape_drift("name", expected_name, observed_name);
    }

    if expected_replicas != observed_replicas {
        return replica_drift(expected_replicas.as_usize(), observed_replicas.as_usize());
    }

    ResourceComparison::Unchanged
}

fn compare_object_bucket(
    expected: &ObjectBucketSpec,
    observed: Option<&ObjectBucketSpec>,
) -> ResourceComparison {
    let Some(observed) = observed else {
        return ResourceComparison::Missing;
    };
    let ObjectBucketSpec {
        name: expected_name,
        replicas: expected_replicas,
    } = expected;
    let ObjectBucketSpec {
        name: observed_name,
        replicas: observed_replicas,
    } = observed;

    if expected_name != observed_name {
        return shape_drift("name", expected_name, observed_name);
    }

    if expected_replicas != observed_replicas {
        return replica_drift(expected_replicas.as_usize(), observed_replicas.as_usize());
    }

    ResourceComparison::Unchanged
}

fn compare_stream(expected: &StreamSpec, observed: Option<&StreamSpec>) -> ResourceComparison {
    let Some(observed) = observed else {
        return ResourceComparison::Missing;
    };
    let StreamSpec {
        name: expected_name,
        subjects: expected_subjects,
        retention: expected_retention,
        storage: expected_storage,
        replicas: expected_replicas,
        discard: expected_discard,
        allow_message_schedules: expected_allow_message_schedules,
    } = expected;
    let StreamSpec {
        name: observed_name,
        subjects: observed_subjects,
        retention: observed_retention,
        storage: observed_storage,
        replicas: observed_replicas,
        discard: observed_discard,
        allow_message_schedules: observed_allow_message_schedules,
    } = observed;

    if expected_name != observed_name {
        return shape_drift("name", expected_name, observed_name);
    }

    if expected_subjects != observed_subjects {
        return shape_drift(
            "subjects",
            &format!("{expected_subjects:?}"),
            &format!("{observed_subjects:?}"),
        );
    }

    if expected_retention != observed_retention {
        return shape_drift(
            "retention",
            &format!("{expected_retention:?}"),
            &format!("{observed_retention:?}"),
        );
    }

    if expected_storage != observed_storage {
        return shape_drift(
            "storage",
            &format!("{expected_storage:?}"),
            &format!("{observed_storage:?}"),
        );
    }

    if expected_discard != observed_discard {
        return shape_drift(
            "discard",
            &format!("{expected_discard:?}"),
            &format!("{observed_discard:?}"),
        );
    }

    if expected_allow_message_schedules != observed_allow_message_schedules {
        return shape_drift(
            "allow_message_schedules",
            &expected_allow_message_schedules.to_string(),
            &observed_allow_message_schedules.to_string(),
        );
    }

    if expected_replicas != observed_replicas {
        return replica_drift(expected_replicas.as_usize(), observed_replicas.as_usize());
    }

    ResourceComparison::Unchanged
}

fn replica_drift(expected: usize, observed: usize) -> ResourceComparison {
    ResourceComparison::ShapeDrift {
        reason: BootstrapResourceRefusal::ConfigurationDrift {
            field: "replicas",
            expected: expected.to_string(),
            observed: observed.to_string(),
        },
    }
}

fn shape_drift(field: &'static str, expected: &str, observed: &str) -> ResourceComparison {
    ResourceComparison::ShapeDrift {
        reason: BootstrapResourceRefusal::ConfigurationDrift {
            field,
            expected: expected.to_owned(),
            observed: observed.to_owned(),
        },
    }
}
