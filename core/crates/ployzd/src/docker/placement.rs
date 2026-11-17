//! In-place placement updates of a running Service Container.

use std::future::Future;

use ployz_core::labels::ServiceLabels;
use ployz_core::{
    ContainerId, ContainerKind, ContainerPlacementUpdated, Machine, MachineStorageObservation,
    PlacementRefusal, ResolvedServiceSpec, SpecChange, compare_specs,
};

use super::limits::is_running;
use super::{ContainerRuntime, Error, require_eligible};

impl ContainerRuntime {
    /// Record `spec` as the resolved spec of the Service Container `container_id`,
    /// whose placement `machine`, this Machine, still satisfies. Docker is only
    /// read, so the Container keeps running untouched.
    ///
    /// Only a difference [`compare_specs`] calls `NeedsPlacementUpdate` is
    /// admitted, and the new spec passes the same placement check a Container
    /// start does. A spec already stored answers a resent request as recorded.
    ///
    /// # Errors
    ///
    /// Refuses a Container without a stored spec, one that is not a Service
    /// Container, one that is not running, a spec that differs in more than
    /// placement, and a placement this Machine does not satisfy.
    pub async fn update_placement(
        &self,
        machine: &Machine,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
        storage: impl Future<Output = Option<MachineStorageObservation>>,
    ) -> Result<ContainerPlacementUpdated, Error> {
        let mut config_operation = self.specs.config_operation().await;
        let inspected = self.observation().ensure_managed(container_id).await?;
        let labels = inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .ok_or(Error::NotManaged)?;
        let labels = ServiceLabels::parse(labels)?;
        if labels.kind != ContainerKind::ServiceContainer {
            return Err(Error::NotAServiceContainer(*container_id));
        }
        let stored = self
            .specs
            .get(container_id)
            .await?
            .ok_or_else(|| Error::SpecNotFound(*container_id))?;
        let refused = |refusal| Error::PlacementRefused {
            container_id: *container_id,
            refusal,
        };
        let resent = match admit(&stored, spec) {
            Ok(Admitted::Recorded) => true,
            Ok(Admitted::Change) => false,
            Err(refusal) => return Err(refused(refusal)),
        };
        if !is_running(&inspected) {
            return Err(refused(PlacementRefusal::NotRunning));
        }
        let updated = ContainerPlacementUpdated {
            container_id: *container_id,
        };
        if resent {
            return Ok(updated);
        }
        let storage = if spec.volume_graph().has_mounted_provisioned_volume() {
            storage.await
        } else {
            None
        };
        require_eligible(spec.placement_eligibility_in_namespace(
            &labels.identity.namespace,
            machine,
            storage.as_ref(),
        ))?;
        config_operation.put(container_id, None, spec).await?;
        config_operation.republish();
        Ok(updated)
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Admitted {
    Change,
    Recorded,
}

fn admit(
    stored: &ResolvedServiceSpec,
    spec: &ResolvedServiceSpec,
) -> Result<Admitted, PlacementRefusal> {
    if stored.service_id != spec.service_id || stored.name != spec.name {
        return Err(PlacementRefusal::OtherService);
    }
    match compare_specs(stored, &spec.to_requested()) {
        SpecChange::NeedsPlacementUpdate => Ok(Admitted::Change),
        SpecChange::UpToDate => Ok(Admitted::Recorded),
        SpecChange::NeedsUpdate | SpecChange::NeedsRecreate => Err(PlacementRefusal::NeedsRecreate),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::test_support::{machine, runtime_with_running_container};
    use axum::http::Method;
    use ployz_core::{MachineLabelKey, MachineLabelValue};

    fn placed(spec: &ResolvedServiceSpec, region: &str) -> ResolvedServiceSpec {
        let mut placed = spec.clone();
        placed.placement = serde_json::from_value(serde_json::json!({
            "constraints": [format!("node.labels.region=={region}")]
        }))
        .unwrap();
        placed
    }

    fn machine_in(region: &str) -> Machine {
        let mut machine = machine();
        machine.labels.insert(
            MachineLabelKey::parse("region").unwrap(),
            MachineLabelValue::parse(region).unwrap(),
        );
        machine
    }

    fn docker_writes(fake: &crate::docker::test_support::FakeDocker, from: usize) -> Vec<String> {
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .skip(from)
            .filter(|(method, _)| *method != Method::GET)
            .map(|(method, path)| format!("{method} {path}"))
            .collect()
    }

    #[tokio::test]
    async fn a_placement_this_machine_satisfies_is_recorded_without_asking_docker_for_anything() {
        let (runtime, fake, id) = runtime_with_running_container().await;
        let stored = runtime.specs.get(&id).await.unwrap().unwrap();
        let wanted = placed(&stored, "fsn");
        let created = fake.requests.lock().unwrap().len();

        for _resend in 0..2 {
            let updated = runtime
                .update_placement(&machine_in("fsn"), &id, &wanted, std::future::ready(None))
                .await
                .unwrap();
            assert_eq!(updated.container_id, id);
            assert_eq!(runtime.specs.get(&id).await.unwrap().unwrap(), wanted);
        }

        assert_eq!(docker_writes(&fake, created), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_placement_this_machine_does_not_satisfy_is_refused_and_the_spec_stays() {
        let (runtime, _fake, id) = runtime_with_running_container().await;
        let stored = runtime.specs.get(&id).await.unwrap().unwrap();

        let error = runtime
            .update_placement(
                &machine_in("fsn"),
                &id,
                &placed(&stored, "syd"),
                std::future::ready(None),
            )
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::ServicePlacementMismatch),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "this Machine does not satisfy the resolved Service placement"
        );
        assert_eq!(
            ployz_core::RpcError::from(&error).code,
            ployz_core::RpcErrorCode::Conflict
        );
        assert_eq!(runtime.specs.get(&id).await.unwrap().unwrap(), stored);
    }

    #[tokio::test]
    async fn a_spec_that_changes_more_than_placement_is_refused_and_the_spec_stays() {
        let (runtime, _fake, id) = runtime_with_running_container().await;
        let stored = runtime.specs.get(&id).await.unwrap().unwrap();
        let mut wanted = placed(&stored, "fsn");
        wanted.container.image = "example.test/api:2".into();

        let error = runtime
            .update_placement(&machine_in("fsn"), &id, &wanted, std::future::ready(None))
            .await
            .unwrap_err();

        assert!(
            matches!(
                error,
                Error::PlacementRefused {
                    refusal: PlacementRefusal::NeedsRecreate,
                    ..
                }
            ),
            "{error:?}"
        );
        let answer = ployz_core::RpcError::from(&error);
        assert_eq!(answer.code, ployz_core::RpcErrorCode::Conflict);
        assert_eq!(
            answer.details.get("reason"),
            Some(&serde_json::json!("needs_recreate"))
        );
        assert_eq!(runtime.specs.get(&id).await.unwrap().unwrap(), stored);
    }

    #[test]
    fn only_a_placement_change_of_the_same_service_is_admitted() {
        let stored: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "a".repeat(32), "name": "api",
            "mode": {"mode": "replicated", "replicas": 1},
            "container": {"image": "api:1", "pull_policy": "missing"}
        }))
        .unwrap();
        let wanted = placed(&stored, "fsn");
        assert_eq!(admit(&stored, &wanted), Ok(Admitted::Change));
        assert_eq!(admit(&wanted, &wanted), Ok(Admitted::Recorded));
        assert_eq!(admit(&wanted, &stored), Ok(Admitted::Change));
        let mut limited = wanted.clone();
        limited.container.resources = serde_json::from_value(serde_json::json!({
            "memory_bytes": 64
        }))
        .unwrap();
        assert_eq!(
            admit(&stored, &limited),
            Err(PlacementRefusal::NeedsRecreate)
        );
        let mut renamed = wanted;
        renamed.name = "worker".parse().unwrap();
        assert_eq!(
            admit(&stored, &renamed),
            Err(PlacementRefusal::OtherService)
        );
    }
}
