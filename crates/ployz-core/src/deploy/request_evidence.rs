use super::{DeployImageReplacementError, DeployRequest, EnvName, EnvValue, ImageReference};
use crate::ids::ServiceId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const FINGERPRINT_PREFIX: &str = "v1:sha256:";
const FINGERPRINT_DOMAIN: &str = "ployz.deploy-env-evidence.v1";

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"EnvValueFingerprint\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct EnvValueFingerprint(String);

impl EnvValueFingerprint {
    #[must_use]
    pub fn for_value(service_id: &ServiceId, name: &EnvName, value: &EnvValue) -> Self {
        let mut hasher = Sha256::new();
        for part in [
            FINGERPRINT_DOMAIN.as_bytes(),
            service_id.as_str().as_bytes(),
            name.as_str().as_bytes(),
            value.as_str().as_bytes(),
        ] {
            hasher.update((part.len() as u64).to_be_bytes());
            hasher.update(part);
        }
        Self(format!("{FINGERPRINT_PREFIX}{:x}", hasher.finalize()))
    }

    pub fn try_new(value: impl Into<String>) -> Result<Self, EnvValueFingerprintError> {
        let value = value.into();
        let Some(digest) = value.strip_prefix(FINGERPRINT_PREFIX) else {
            return Err(EnvValueFingerprintError);
        };
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(EnvValueFingerprintError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EnvValueFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("EnvValueFingerprint")
            .field(&self.0)
            .finish()
    }
}

impl TryFrom<String> for EnvValueFingerprint {
    type Error = EnvValueFingerprintError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<EnvValueFingerprint> for String {
    fn from(value: EnvValueFingerprint) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("environment value fingerprint must be v1:sha256 followed by 64 lowercase hex digits")]
pub struct EnvValueFingerprintError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceEnvironmentEvidence {
    service_id: ServiceId,
    fingerprints: BTreeMap<EnvName, EnvValueFingerprint>,
}

impl ServiceEnvironmentEvidence {
    #[must_use]
    pub fn service_id(&self) -> &ServiceId {
        &self.service_id
    }

    #[must_use]
    pub fn fingerprints(&self) -> &BTreeMap<EnvName, EnvValueFingerprint> {
        &self.fingerprints
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fingerprints.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRequestEvidence {
    request: DeployRequest,
    environments: Vec<ServiceEnvironmentEvidence>,
}

impl DeployRequestEvidence {
    #[must_use]
    pub fn from_request(request: &DeployRequest) -> Self {
        let mut request = request.clone();
        let environments = request
            .services
            .iter_mut()
            .map(|service| {
                let environment = std::mem::replace(
                    &mut service.runtime.environment,
                    super::ServiceEnvironment::empty(),
                );
                let fingerprints = environment
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            EnvValueFingerprint::for_value(&service.service_id, name, value),
                        )
                    })
                    .collect();
                ServiceEnvironmentEvidence {
                    service_id: service.service_id.clone(),
                    fingerprints,
                }
            })
            .collect();
        Self {
            request,
            environments,
        }
    }

    #[must_use]
    pub fn request(&self) -> &DeployRequest {
        &self.request
    }

    #[must_use]
    pub fn environments(&self) -> &[ServiceEnvironmentEvidence] {
        &self.environments
    }

    #[must_use]
    pub fn status_service_id(&self) -> ServiceId {
        self.request.status_service_id()
    }

    pub fn replace_service_image(
        &mut self,
        service_id: &ServiceId,
        image: ImageReference,
    ) -> Result<(), DeployImageReplacementError> {
        self.request.replace_service_image(service_id, image)
    }

    pub fn try_into_rollback_request(
        self,
    ) -> Result<DeployRequest, DeployRollbackEnvironmentError> {
        let affected = self
            .environments
            .into_iter()
            .filter(|environment| !environment.is_empty())
            .map(|environment| DeployRollbackEnvironment {
                service_id: environment.service_id,
                environment_names: environment.fingerprints.into_keys().collect(),
            })
            .collect::<Vec<_>>();
        if affected.is_empty() {
            Ok(self.request)
        } else {
            Err(DeployRollbackEnvironmentError { affected })
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeployRequestEvidenceWire {
    request: DeployRequest,
    environments: Vec<ServiceEnvironmentEvidence>,
}

impl<'de> Deserialize<'de> for DeployRequestEvidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = DeployRequestEvidenceWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<DeployRequestEvidenceWire> for DeployRequestEvidence {
    type Error = DeployRequestEvidenceError;

    fn try_from(wire: DeployRequestEvidenceWire) -> Result<Self, Self::Error> {
        for service in &wire.request.services {
            if !service.runtime.environment.is_empty() {
                return Err(DeployRequestEvidenceError::RequestContainsEnvironment {
                    service_id: service.service_id.clone(),
                });
            }
        }

        let known = wire
            .request
            .services
            .iter()
            .map(|service| service.service_id.clone())
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        for environment in &wire.environments {
            if !known.contains(&environment.service_id) {
                return Err(DeployRequestEvidenceError::UnknownService {
                    service_id: environment.service_id.clone(),
                });
            }
            if !seen.insert(environment.service_id.clone()) {
                return Err(DeployRequestEvidenceError::DuplicateService {
                    service_id: environment.service_id.clone(),
                });
            }
        }
        Ok(Self {
            request: wire.request,
            environments: wire.environments,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployRequestEvidenceError {
    #[error("sanitized deploy request still contains environment values for service {}", .service_id.as_str())]
    RequestContainsEnvironment { service_id: ServiceId },
    #[error("deploy environment evidence names unknown service {}", .service_id.as_str())]
    UnknownService { service_id: ServiceId },
    #[error("deploy environment evidence names service {} more than once", .service_id.as_str())]
    DuplicateService { service_id: ServiceId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployRollbackEnvironment {
    service_id: ServiceId,
    environment_names: Vec<EnvName>,
}

impl DeployRollbackEnvironment {
    #[must_use]
    pub fn service_id(&self) -> &ServiceId {
        &self.service_id
    }

    #[must_use]
    pub fn environment_names(&self) -> &[EnvName] {
        &self.environment_names
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("rollback cannot restore redacted deploy environment values")]
pub struct DeployRollbackEnvironmentError {
    affected: Vec<DeployRollbackEnvironment>,
}

impl DeployRollbackEnvironmentError {
    #[must_use]
    pub fn affected(&self) -> &[DeployRollbackEnvironment] {
        &self.affected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::{
        ContainerRuntimeSpec, DeployRequest, DeployServiceSpec, EnvName, EnvValue, ImageReference,
        ImageSource, ReplicaCount, ServiceEnvironment, ServiceMode,
    };
    use crate::ids::{NamespaceId, ServiceId};
    use std::collections::BTreeMap;

    fn request_with_environment(entries: &[(&str, &str)]) -> DeployRequest {
        let environment = entries
            .iter()
            .map(|(name, value)| {
                (
                    EnvName::try_new(*name).expect("valid environment name"),
                    EnvValue::try_new(*value).expect("valid environment value"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut runtime = ContainerRuntimeSpec::image_defaults();
        runtime.environment = ServiceEnvironment::from(environment);
        DeployRequest {
            namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
            origin: None,
            volumes: BTreeMap::new(),
            services: vec![DeployServiceSpec {
                service_id: ServiceId::try_new("api").expect("valid service id"),
                image: ImageReference::try_new("ghcr.io/acme/api:current").expect("valid image"),
                image_source: ImageSource::Registry,
                mode: ServiceMode::Replicated {
                    replicas: ReplicaCount::try_new(1).expect("valid replicas"),
                },
                keep: None,
                runtime,
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        }
    }

    #[test]
    fn deploy_request_evidence_removes_values_and_keeps_names() {
        let evidence = DeployRequestEvidence::from_request(&request_with_environment(&[
            ("TOKEN", "sentinel-secret"),
            ("MODE", "production"),
        ]));
        let wire = serde_json::to_string(&evidence).expect("evidence serializes");

        assert!(!wire.contains("sentinel-secret"));
        assert!(!wire.contains("production"));
        assert!(wire.contains("TOKEN"));
        assert!(wire.contains("MODE"));
        assert!(
            evidence.request().services[0]
                .runtime
                .environment
                .is_empty()
        );
    }

    #[test]
    fn environment_fingerprint_is_stable_and_bound_to_service_and_name() {
        let service_id = ServiceId::try_new("api").expect("valid service id");
        let other_service_id = ServiceId::try_new("worker").expect("valid service id");
        let name = EnvName::try_new("TOKEN").expect("valid environment name");
        let other_name = EnvName::try_new("OTHER_TOKEN").expect("valid environment name");
        let value = EnvValue::try_new("secret-value").expect("valid environment value");

        let fingerprint = EnvValueFingerprint::for_value(&service_id, &name, &value);
        assert_eq!(
            fingerprint.as_str(),
            "v1:sha256:ab1a63f0834c06f5bd1e9e1451464da4f614b18946132de588c944679f5c0f3e"
        );
        assert_eq!(
            fingerprint,
            EnvValueFingerprint::for_value(&service_id, &name, &value)
        );
        assert_ne!(
            fingerprint,
            EnvValueFingerprint::for_value(&other_service_id, &name, &value)
        );
        assert_ne!(
            fingerprint,
            EnvValueFingerprint::for_value(&service_id, &other_name, &value)
        );

        for invalid in [
            "sha256:ab1a63f0834c06f5bd1e9e1451464da4f614b18946132de588c944679f5c0f3e",
            "v1:sha256:AB1A63F0834C06F5BD1E9E1451464DA4F614B18946132DE588C944679F5C0F3E",
            "v1:sha256:abc",
        ] {
            assert!(EnvValueFingerprint::try_new(invalid).is_err());
        }
    }

    #[test]
    fn evidence_wire_rejects_secret_values_unknown_and_duplicate_services() {
        let evidence = DeployRequestEvidence::from_request(&request_with_environment(&[(
            "TOKEN",
            "secret-value",
        )]));
        let mut wire = serde_json::to_value(&evidence).expect("evidence serializes");

        wire["request"]["services"][0]["runtime"]["environment"] =
            serde_json::json!({"TOKEN": "leaked"});
        assert!(serde_json::from_value::<DeployRequestEvidence>(wire).is_err());

        let mut wire = serde_json::to_value(&evidence).expect("evidence serializes");
        wire["environments"][0]["service_id"] = serde_json::json!("unknown");
        assert!(serde_json::from_value::<DeployRequestEvidence>(wire).is_err());

        let mut wire = serde_json::to_value(&evidence).expect("evidence serializes");
        let duplicate = wire["environments"][0].clone();
        wire["environments"]
            .as_array_mut()
            .expect("environments is an array")
            .push(duplicate);
        assert!(serde_json::from_value::<DeployRequestEvidence>(wire).is_err());
    }

    #[test]
    fn rollback_conversion_reports_missing_environment_values() {
        let evidence = DeployRequestEvidence::from_request(&request_with_environment(&[
            ("TOKEN", "secret-value"),
            ("MODE", "production"),
        ]));

        let error = evidence
            .try_into_rollback_request()
            .expect_err("fingerprints cannot restore environment values");
        assert_eq!(error.affected().len(), 1);
        assert_eq!(error.affected()[0].service_id().as_str(), "api");
        assert_eq!(
            error.affected()[0]
                .environment_names()
                .iter()
                .map(EnvName::as_str)
                .collect::<Vec<_>>(),
            vec!["MODE", "TOKEN"]
        );
    }

    #[test]
    fn rollback_conversion_accepts_requests_without_environment_values() {
        let request = request_with_environment(&[]);
        let evidence = DeployRequestEvidence::from_request(&request);

        assert_eq!(
            evidence
                .try_into_rollback_request()
                .expect("no values need restoration"),
            request
        );
    }

    #[test]
    fn environment_values_and_errors_never_debug_the_candidate() {
        let value = EnvValue::try_new("sentinel-secret").expect("valid value");
        assert_eq!(format!("{value:?}"), "EnvValue([redacted])");

        let error = EnvValue::try_new("sentinel\0secret").expect_err("NUL is rejected");
        assert!(!format!("{error:?}").contains("sentinel"));
        assert!(!error.to_string().contains("sentinel"));
    }
}
