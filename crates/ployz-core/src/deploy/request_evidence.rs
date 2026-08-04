use super::{DeployImageReplacementError, DeployRequest, EnvName, ImageReference};
use crate::ids::ServiceId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceEnvironmentNames {
    service_id: ServiceId,
    names: BTreeSet<EnvName>,
}

impl ServiceEnvironmentNames {
    #[must_use]
    pub fn service_id(&self) -> &ServiceId {
        &self.service_id
    }

    #[must_use]
    pub fn names(&self) -> &BTreeSet<EnvName> {
        &self.names
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRequestEvidence {
    request: DeployRequest,
    environment_names: Vec<ServiceEnvironmentNames>,
}

impl DeployRequestEvidence {
    #[must_use]
    pub fn from_request(request: &DeployRequest) -> Self {
        let mut request = request.clone();
        let environment_names = request
            .services
            .iter_mut()
            .map(|service| {
                let environment = std::mem::replace(
                    &mut service.runtime.environment,
                    super::ServiceEnvironment::empty(),
                );
                let names = environment
                    .iter()
                    .map(|(name, _value)| name.clone())
                    .collect();
                ServiceEnvironmentNames {
                    service_id: service.service_id.clone(),
                    names,
                }
            })
            .collect();
        Self {
            request,
            environment_names,
        }
    }

    #[must_use]
    pub fn request(&self) -> &DeployRequest {
        &self.request
    }

    #[must_use]
    pub fn environment_names(&self) -> &[ServiceEnvironmentNames] {
        &self.environment_names
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
            .environment_names
            .into_iter()
            .filter(|environment| !environment.is_empty())
            .map(|environment| DeployRollbackEnvironment {
                service_id: environment.service_id,
                environment_names: environment.names.into_iter().collect(),
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
    environment_names: Vec<ServiceEnvironmentNames>,
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
        let mut environments = BTreeMap::new();
        for environment in wire.environment_names {
            if !known.contains(&environment.service_id) {
                return Err(DeployRequestEvidenceError::UnknownService {
                    service_id: environment.service_id.clone(),
                });
            }
            let service_id = environment.service_id.clone();
            if environments
                .insert(service_id.clone(), environment)
                .is_some()
            {
                return Err(DeployRequestEvidenceError::DuplicateService { service_id });
            }
        }
        let mut environment_names = Vec::with_capacity(wire.request.services.len());
        for service in &wire.request.services {
            let Some(environment) = environments.remove(&service.service_id) else {
                return Err(DeployRequestEvidenceError::MissingService {
                    service_id: service.service_id.clone(),
                });
            };
            environment_names.push(environment);
        }
        Ok(Self {
            request: wire.request,
            environment_names,
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
    #[error("deploy environment evidence omits service {}", .service_id.as_str())]
    MissingService { service_id: ServiceId },
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

    fn environment_names_wire_mut(wire: &mut serde_json::Value) -> &mut Vec<serde_json::Value> {
        wire.get_mut("environment_names")
            .and_then(serde_json::Value::as_array_mut)
            .expect("environment_names is an array")
    }

    fn request_environment_wire_mut(wire: &mut serde_json::Value) -> &mut serde_json::Value {
        wire.get_mut("request")
            .and_then(|request| request.get_mut("services"))
            .and_then(serde_json::Value::as_array_mut)
            .and_then(|services| services.first_mut())
            .and_then(|service| service.get_mut("runtime"))
            .and_then(|runtime| runtime.get_mut("environment"))
            .expect("request service has an environment field")
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
        let [service] = evidence.request().services.as_slice() else {
            panic!("evidence request contains one service");
        };
        assert!(service.runtime.environment.is_empty());
    }

    #[test]
    fn evidence_wire_rejects_secret_values_unknown_duplicate_and_missing_services() {
        let evidence = DeployRequestEvidence::from_request(&request_with_environment(&[(
            "TOKEN",
            "secret-value",
        )]));
        let mut wire = serde_json::to_value(&evidence).expect("evidence serializes");

        *request_environment_wire_mut(&mut wire) = serde_json::json!({"TOKEN": "leaked"});
        assert!(serde_json::from_value::<DeployRequestEvidence>(wire).is_err());

        let mut wire = serde_json::to_value(&evidence).expect("evidence serializes");
        let [environment] = environment_names_wire_mut(&mut wire).as_mut_slice() else {
            panic!("one environment name set serializes");
        };
        *environment
            .get_mut("service_id")
            .expect("environment name set has a service id") = serde_json::json!("unknown");
        assert!(serde_json::from_value::<DeployRequestEvidence>(wire).is_err());

        let mut wire = serde_json::to_value(&evidence).expect("evidence serializes");
        let duplicate = {
            let [environment] = environment_names_wire_mut(&mut wire).as_slice() else {
                panic!("one environment name set serializes");
            };
            environment.clone()
        };
        environment_names_wire_mut(&mut wire).push(duplicate);
        assert!(serde_json::from_value::<DeployRequestEvidence>(wire).is_err());

        let mut wire = serde_json::to_value(&evidence).expect("evidence serializes");
        environment_names_wire_mut(&mut wire).clear();
        assert!(serde_json::from_value::<DeployRequestEvidence>(wire).is_err());
    }

    #[test]
    fn evidence_wire_normalizes_services_and_names_to_canonical_order() {
        let mut request = request_with_environment(&[("TOKEN", "secret-value")]);
        let [api] = request.services.as_slice() else {
            panic!("test request contains one service");
        };
        let mut worker = api.clone();
        worker.service_id = ServiceId::try_new("worker").expect("valid service id");
        worker.runtime.environment = ServiceEnvironment::from(BTreeMap::from([
            (
                EnvName::try_new("ZEBRA").expect("valid environment name"),
                EnvValue::try_new("z-value").expect("valid environment value"),
            ),
            (
                EnvName::try_new("ALPHA").expect("valid environment name"),
                EnvValue::try_new("a-value").expect("valid environment value"),
            ),
        ]));
        request.services.push(worker);
        let evidence = DeployRequestEvidence::from_request(&request);
        let mut wire = serde_json::to_value(&evidence).expect("evidence serializes");
        let environments = environment_names_wire_mut(&mut wire);
        environments.reverse();
        let Some(worker) = environments.first_mut() else {
            panic!("worker environment name set serializes");
        };
        *worker
            .get_mut("names")
            .expect("environment name set has names") =
            serde_json::json!(["ZEBRA", "ALPHA", "ZEBRA"]);

        let normalized = serde_json::from_value::<DeployRequestEvidence>(wire)
            .expect("reordered complete evidence is accepted");
        let [api, worker] = normalized.environment_names() else {
            panic!("both service environment name sets remain present");
        };
        assert_eq!(api.service_id().as_str(), "api");
        assert_eq!(worker.service_id().as_str(), "worker");
        assert_eq!(
            worker
                .names()
                .iter()
                .map(EnvName::as_str)
                .collect::<Vec<_>>(),
            vec!["ALPHA", "ZEBRA"]
        );
    }

    #[test]
    fn rollback_conversion_reports_missing_environment_values() {
        let evidence = DeployRequestEvidence::from_request(&request_with_environment(&[
            ("TOKEN", "secret-value"),
            ("MODE", "production"),
        ]));

        let error = evidence
            .try_into_rollback_request()
            .expect_err("names cannot restore environment values");
        let [affected] = error.affected() else {
            panic!("one service requires environment restoration");
        };
        assert_eq!(affected.service_id().as_str(), "api");
        assert_eq!(
            affected
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
