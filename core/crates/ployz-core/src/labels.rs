//! Docker labels on Ployz's containers: every key, what the labels of a container mean,
//! and the writer that stamps them.
//!
//! A key spelled here is on live containers and in replicated observations, so none is
//! ever renamed. Callers ask a question of a label map ([`Class::of`], [`ServiceLabels::parse`],
//! [`service_pair`]) rather than reading keys.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ContainerKind, DeploymentLogId, Gen, Namespace, QualifiedService, ServiceId, ServiceName,
    ValueError,
};

/// Written, empty, on every container a Machine creates for a Service or a pre-deploy
/// hook, and on the unregistry.
pub const MANAGED: &str = "ployz.managed";
/// The Namespace that owns a container or a Docker volume.
pub const NAMESPACE: &str = "ployz.namespace";
/// The Service's deployment identity.
pub const SERVICE_ID: &str = "ployz.service.id";
/// The Service's name.
pub const SERVICE_NAME: &str = "ployz.service.name";
/// Present on a pre-deploy hook container, with any value.
pub const HOOK: &str = "ployz.service.hook";
/// The value [`HOOK`] is written with.
pub const HOOK_PRE_DEPLOY: &str = "pre-deploy";
/// The Deploy run that created a container.
pub const DEPLOYMENT_ID: &str = "ployz.deployment.id";
/// The creation key a container was made under.
pub const CREATION_KEY: &str = "ployz.creation.key";
/// The rotation generation of a container's copy.
pub const GEN: &str = "ployz.gen";
/// `true` on a container whose spec publishes no host port, else `false`. Written
/// either way so a label the image carries can't decide it.
pub const BRIDGE_IPV6: &str = "ployz.network.ipv6";
/// The sealed record of the environment values a container was created with.
pub const SEALED_ENV: &str = "ployz.env.sealed";
/// Marks what the daemon runs for itself, empty: the Corrosion container and the ployz
/// bridge network.
pub const SYSTEM_MANAGED: &str = "ployzd.managed";
/// The digest of the create body and files a daemon-managed container was made from.
pub const SPEC: &str = "ployzd.spec";
/// The authored Service a container runs, for bookkeeping only: no Machine or Cloud reads
/// it back. It sits outside [`MANAGEMENT_PREFIX`], so a Config may author it.
pub const CLOUD_SERVICE_ID: &str = "cloud.ployz.service.id";
/// Keys under this prefix are reserved for Ployz; a Config may not author one.
pub const MANAGEMENT_PREFIX: &str = "ployz.";

/// A read-only map of Docker labels.
pub trait LabelMap {
    /// The value under `key`.
    fn label(&self, key: &str) -> Option<&str>;

    /// Whether `key` is present, with any value.
    fn has_label(&self, key: &str) -> bool {
        self.label(key).is_some()
    }
}

impl LabelMap for HashMap<String, String> {
    fn label(&self, key: &str) -> Option<&str> {
        self.get(key).map(String::as_str)
    }
}

impl LabelMap for BTreeMap<String, String> {
    fn label(&self, key: &str) -> Option<&str> {
        self.get(key).map(String::as_str)
    }
}

/// Whether the labels mark a container Ployz made, for a Service or for itself.
#[must_use]
pub fn is_ployz_owned(labels: &impl LabelMap) -> bool {
    labels.has_label(MANAGED) || labels.has_label(SYSTEM_MANAGED)
}

/// The labels of what the daemon runs for itself.
#[must_use]
pub fn system_labels() -> HashMap<String, String> {
    HashMap::from([(SYSTEM_MANAGED.to_owned(), String::new())])
}

/// What a container is, read leniently from its labels. Observe writes it to `meta.json`
/// and the sampler decides what to book from it.
///
/// It is not core's replicated [`ContainerKind`]: that one has no system variant, and a new
/// variant would not be additive.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Service,
    PreDeployHook,
    /// Ingress, the unregistry, Corrosion, and other containers Ployz runs
    /// for itself. They share the one cap with apps.
    System,
}

impl Class {
    /// `None` when the labels do not mark a Ployz container. A hook label wins over the
    /// system Namespace, which wins over a Service name; anything else managed is system.
    #[must_use]
    pub fn of(labels: &impl LabelMap) -> Option<Self> {
        if !is_ployz_owned(labels) {
            return None;
        }
        Some(if labels.has_label(HOOK) {
            Self::PreDeployHook
        } else if labels.label(NAMESPACE) == Some(Namespace::SYSTEM) {
            Self::System
        } else if labels.has_label(SERVICE_NAME) {
            Self::Service
        } else {
            Self::System
        })
    }
}

/// A Service's `(ServiceId, QualifiedService)` as the labels name it, read leniently. A row
/// names its Service both ways or neither, so a container missing or mis-spelling any of
/// the three labels is none.
#[must_use]
pub fn service_pair(labels: &impl LabelMap) -> Option<(ServiceId, QualifiedService)> {
    let id = ServiceId::parse(labels.label(SERVICE_ID)?).ok()?;
    let namespace = Namespace::parse(labels.label(NAMESPACE)?).ok()?;
    let name = ServiceName::parse(labels.label(SERVICE_NAME)?).ok()?;
    Some((id, QualifiedService::new(namespace, name)))
}

/// A Service or pre-deploy hook container's identity and kind, read strictly from its labels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceLabels {
    pub identity: QualifiedService,
    pub kind: ContainerKind,
}

/// Why labels are not a Service container's.
#[derive(Debug, Error)]
pub enum LabelError {
    #[error("container is not managed by Ployz")]
    NotManaged,
    #[error("managed container is missing label {0}")]
    Missing(&'static str),
    #[error("managed container has invalid {key}")]
    Invalid {
        key: &'static str,
        #[source]
        source: ValueError,
    },
}

impl ServiceLabels {
    /// Read a managed container's labels. A hook label of any value makes a hook.
    ///
    /// # Errors
    ///
    /// [`LabelError::NotManaged`] without [`MANAGED`]; [`LabelError::Missing`] for the
    /// first of the Namespace, Service ID and Service name that is absent or empty;
    /// [`LabelError::Invalid`] for the first of the Service ID, Namespace and Service name
    /// that does not parse.
    pub fn parse(labels: &impl LabelMap) -> Result<Self, LabelError> {
        if !labels.has_label(MANAGED) {
            return Err(LabelError::NotManaged);
        }
        let namespace = required(labels, NAMESPACE)?;
        let service_id = required(labels, SERVICE_ID)?;
        let service_name = required(labels, SERVICE_NAME)?;
        ServiceId::parse(service_id).map_err(|source| LabelError::Invalid {
            key: SERVICE_ID,
            source,
        })?;
        Ok(Self {
            identity: QualifiedService::new(
                Namespace::parse(namespace).map_err(|source| LabelError::Invalid {
                    key: NAMESPACE,
                    source,
                })?,
                ServiceName::parse(service_name).map_err(|source| LabelError::Invalid {
                    key: SERVICE_NAME,
                    source,
                })?,
            ),
            kind: if labels.has_label(HOOK) {
                ContainerKind::PreDeployHook
            } else {
                ContainerKind::ServiceContainer
            },
        })
    }
}

fn required<'a>(labels: &'a impl LabelMap, key: &'static str) -> Result<&'a str, LabelError> {
    labels
        .label(key)
        .filter(|value| !value.is_empty())
        .ok_or(LabelError::Missing(key))
}

/// What a Machine writes on a container it creates for a Service or a hook: the dual of
/// [`ServiceLabels::parse`].
#[derive(Clone, Copy, Debug)]
pub struct Stamp<'a> {
    pub namespace: &'a Namespace,
    pub service_id: &'a ServiceId,
    pub service_name: &'a ServiceName,
    pub kind: ContainerKind,
    /// Whether the container's spec publishes no host port, so it takes a bridge IPv6 address.
    pub bridge_ipv6: bool,
}

impl Stamp<'_> {
    /// Write the labels into `labels`, replacing any of the same key.
    pub fn write(&self, labels: &mut HashMap<String, String>) {
        labels.extend([
            (MANAGED.to_owned(), String::new()),
            (NAMESPACE.to_owned(), self.namespace.to_string()),
            (SERVICE_ID.to_owned(), self.service_id.to_string()),
            (SERVICE_NAME.to_owned(), self.service_name.to_string()),
            (BRIDGE_IPV6.to_owned(), self.bridge_ipv6.to_string()),
        ]);
        if self.kind == ContainerKind::PreDeployHook {
            labels.insert(HOOK.to_owned(), HOOK_PRE_DEPLOY.to_owned());
        }
    }
}

/// Write which Deploy run, creation key and generation a container was made under. A
/// missing run or key leaves its label off.
pub fn stamp_run(
    labels: &mut HashMap<String, String>,
    deployment: Option<&DeploymentLogId>,
    creation_key: Option<&str>,
    generation: Gen,
) {
    if let Some(deployment) = deployment {
        labels.insert(DEPLOYMENT_ID.to_owned(), deployment.to_string());
    }
    if let Some(key) = creation_key {
        labels.insert(CREATION_KEY.to_owned(), key.to_owned());
    }
    labels.insert(GEN.to_owned(), generation.to_string());
}

/// The generation a container's labels record; [`Gen::FIRST`] when absent or unreadable.
#[must_use]
pub fn generation(labels: &impl LabelMap) -> Gen {
    labels
        .label(GEN)
        .and_then(|label| label.parse().ok())
        .map_or(Gen::FIRST, Gen::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_key_keeps_its_live_spelling() {
        for (key, spelling) in [
            (MANAGED, "ployz.managed"),
            (NAMESPACE, "ployz.namespace"),
            (SERVICE_ID, "ployz.service.id"),
            (SERVICE_NAME, "ployz.service.name"),
            (HOOK, "ployz.service.hook"),
            (HOOK_PRE_DEPLOY, "pre-deploy"),
            (DEPLOYMENT_ID, "ployz.deployment.id"),
            (CREATION_KEY, "ployz.creation.key"),
            (GEN, "ployz.gen"),
            (BRIDGE_IPV6, "ployz.network.ipv6"),
            (SEALED_ENV, "ployz.env.sealed"),
            (SYSTEM_MANAGED, "ployzd.managed"),
            (SPEC, "ployzd.spec"),
            (CLOUD_SERVICE_ID, "cloud.ployz.service.id"),
            (MANAGEMENT_PREFIX, "ployz."),
        ] {
            assert_eq!(key, spelling);
        }
    }

    fn map(labels: &[(&str, &str)]) -> HashMap<String, String> {
        labels
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_stamp_reads_back_through_the_strict_parse() {
        let namespace = Namespace::parse("shop").unwrap();
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let service_name = ServiceName::parse("api").unwrap();
        for kind in [
            ContainerKind::ServiceContainer,
            ContainerKind::PreDeployHook,
        ] {
            let mut labels = HashMap::new();
            Stamp {
                namespace: &namespace,
                service_id: &service_id,
                service_name: &service_name,
                kind,
                bridge_ipv6: true,
            }
            .write(&mut labels);
            let parsed = ServiceLabels::parse(&labels).unwrap();
            assert_eq!(parsed.kind, kind);
            assert_eq!(
                parsed.identity,
                QualifiedService::new(namespace.clone(), service_name.clone())
            );
            assert_eq!(
                service_pair(&labels),
                Some((service_id, parsed.identity.clone()))
            );
            assert_eq!(labels.has_label(HOOK), kind == ContainerKind::PreDeployHook);
            assert_eq!(labels.label(BRIDGE_IPV6), Some("true"));
        }
    }

    #[test]
    fn a_stamp_writes_exactly_its_labels() {
        let namespace = Namespace::parse("shop").unwrap();
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let service_name = ServiceName::parse("api").unwrap();
        let mut labels = map(&[("example.kept", "yes"), (BRIDGE_IPV6, "stale")]);
        Stamp {
            namespace: &namespace,
            service_id: &service_id,
            service_name: &service_name,
            kind: ContainerKind::PreDeployHook,
            bridge_ipv6: false,
        }
        .write(&mut labels);
        let deployment = DeploymentLogId::parse("deploy-7").unwrap();
        stamp_run(&mut labels, Some(&deployment), Some("plan:0"), Gen::new(3));
        assert_eq!(
            labels,
            map(&[
                ("example.kept", "yes"),
                ("ployz.managed", ""),
                ("ployz.namespace", "shop"),
                ("ployz.service.id", &"a".repeat(32)),
                ("ployz.service.name", "api"),
                ("ployz.service.hook", "pre-deploy"),
                ("ployz.network.ipv6", "false"),
                ("ployz.deployment.id", "deploy-7"),
                ("ployz.creation.key", "plan:0"),
                ("ployz.gen", "3"),
            ])
        );
        assert_eq!(generation(&labels), Gen::new(3));
        let mut bare = HashMap::new();
        stamp_run(&mut bare, None, None, Gen::FIRST);
        assert_eq!(bare.len(), 1);
        assert_eq!(generation(&bare), Gen::FIRST);
    }

    #[test]
    fn generation_falls_back_to_the_first() {
        assert_eq!(generation(&HashMap::new()), Gen::FIRST);
        assert_eq!(generation(&map(&[(GEN, "three")])), Gen::FIRST);
        assert_eq!(generation(&map(&[(GEN, "3")])), Gen::new(3));
    }

    #[test]
    fn the_strict_parse_names_what_is_wrong_in_a_fixed_order() {
        let id = "a".repeat(32);
        let outcome = |labels: &[(&str, &str)]| match ServiceLabels::parse(&map(labels)) {
            Ok(parsed) => format!(
                "{:?} {}/{}",
                parsed.kind, parsed.identity.namespace, parsed.identity.name
            ),
            Err(error) => error.to_string(),
        };
        assert_eq!(outcome(&[]), "container is not managed by Ployz");
        assert_eq!(
            outcome(&[(SYSTEM_MANAGED, "")]),
            "container is not managed by Ployz"
        );
        assert_eq!(
            outcome(&[(MANAGED, "")]),
            "managed container is missing label ployz.namespace"
        );
        assert_eq!(
            outcome(&[(MANAGED, ""), (NAMESPACE, "app")]),
            "managed container is missing label ployz.service.id"
        );
        assert_eq!(
            outcome(&[(MANAGED, ""), (NAMESPACE, "app"), (SERVICE_ID, &id)]),
            "managed container is missing label ployz.service.name"
        );
        assert_eq!(
            outcome(&[
                (MANAGED, ""),
                (NAMESPACE, "Not_DNS"),
                (SERVICE_ID, "short"),
                (SERVICE_NAME, "Bad_Name")
            ]),
            "managed container has invalid ployz.service.id"
        );
        assert_eq!(
            outcome(&[
                (MANAGED, ""),
                (NAMESPACE, "app"),
                (SERVICE_ID, &id),
                (SERVICE_NAME, "api"),
                (HOOK, ""),
            ]),
            "PreDeployHook app/api"
        );
    }

    #[test]
    fn the_lenient_classes_follow_the_labels() {
        let class = |labels: &[(&str, &str)]| Class::of(&map(labels));
        assert_eq!(class(&[]), None);
        assert_eq!(class(&[(NAMESPACE, "app")]), None);
        assert_eq!(class(&[(HOOK, "pre-deploy")]), None);
        assert_eq!(class(&[(SYSTEM_MANAGED, "")]), Some(Class::System));
        assert_eq!(class(&[(MANAGED, "")]), Some(Class::System));
        assert_eq!(
            class(&[(MANAGED, ""), (SERVICE_NAME, "web")]),
            Some(Class::Service)
        );
        assert_eq!(
            class(&[
                (MANAGED, ""),
                (NAMESPACE, "ployz-system"),
                (SERVICE_NAME, "ingress")
            ]),
            Some(Class::System)
        );
        assert_eq!(
            class(&[
                (MANAGED, ""),
                (NAMESPACE, "ployz-system"),
                (SERVICE_NAME, "ingress"),
                (HOOK, "")
            ]),
            Some(Class::PreDeployHook)
        );
    }

    #[test]
    fn a_service_pair_needs_all_three_labels() {
        let id = "a".repeat(32);
        let all = [
            (SERVICE_ID, id.as_str()),
            (NAMESPACE, "app"),
            (SERVICE_NAME, "api"),
        ];
        assert!(service_pair(&map(&all)).is_some());
        for without in 0..all.len() {
            let mut labels = all.to_vec();
            labels.remove(without);
            assert_eq!(service_pair(&map(&labels)), None);
        }
        assert_eq!(
            service_pair(&map(&[
                (SERVICE_ID, "short"),
                (NAMESPACE, "app"),
                (SERVICE_NAME, "api")
            ])),
            None
        );
    }

    #[test]
    fn the_label_maps_read_alike() {
        let hash = map(&[(MANAGED, "")]);
        let tree: BTreeMap<String, String> = hash.clone().into_iter().collect();
        assert!(is_ployz_owned(&hash) && is_ployz_owned(&tree));
        assert_eq!(system_labels(), map(&[("ployzd.managed", "")]));
    }
}
