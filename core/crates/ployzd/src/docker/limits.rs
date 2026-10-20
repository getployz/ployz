//! In-place CPU and memory limit updates of a running Service Container.

use bollard::models::{ContainerInspectResponse, ContainerUpdateBody, HostConfig};
use ployz_core::{
    ByteQuantity, ContainerId, ContainerKind, ContainerLimits, ContainerLimitsUpdated, CpuNanos,
    LimitsRefusal, ResolvedServiceSpec, SpecChange, compare_specs,
};

use super::{ContainerRuntime, Error, ManagedLabels, docker_error};

/// The limits Docker reports on a Container after an update asked for others.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnappliedLimits {
    pub requested: ContainerLimits,
    pub observed: ContainerLimits,
}

impl std::fmt::Display for UnappliedLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "requested {:?}, observed {:?}",
            self.requested, self.observed
        )
    }
}

impl ContainerRuntime {
    /// Apply `spec`'s CPU and memory limits to the Service Container `container_id`
    /// with `docker update`, then record `spec` as its resolved spec. The Container
    /// keeps its identity and Docker applies the limits without a restart.
    ///
    /// Only a difference [`compare_specs`] calls `NeedsUpdate` is admitted, and
    /// only a memory limit at or above the one Docker runs the Container with. A
    /// spec already stored that Docker runs with answers a resent request as
    /// applied. The
    /// spec is written only after Docker reports the requested limits, so a crash
    /// between the two leaves the old spec and the next Deploy plans the update again.
    ///
    /// # Errors
    ///
    /// Refuses a Container without a stored spec, one that is not a Service
    /// Container, one that is not running, a spec that differs in more than
    /// limits, a memory limit below the live one, and limits Docker does not
    /// report back after the update.
    pub async fn update_limits(
        &self,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerLimitsUpdated, Error> {
        let mut config_operation = self.specs.config_operation().await;
        let stored = self
            .specs
            .get(container_id)
            .await?
            .ok_or_else(|| Error::SpecNotFound(*container_id))?;
        // A resent request whose first attempt applied the limits finds them stored.
        let resent = match refusal(&stored, spec) {
            None => false,
            Some(LimitsRefusal::UpToDate) => true,
            Some(refusal) => {
                return Err(Error::LimitsRefused {
                    container_id: *container_id,
                    refusal,
                });
            }
        };
        let inspected = self.ensure_managed(container_id).await?;
        let labels = inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .ok_or(Error::NotManaged)?;
        if ManagedLabels::parse(labels)?.kind != ContainerKind::ServiceContainer {
            return Err(Error::NotAServiceContainer(*container_id));
        }
        // A stopped Container would take the limits but serve nothing; the Deploy
        // promised a serving copy, so the caller must replace it instead.
        refuse_unless_running(container_id, &inspected)?;
        let requested = spec.container.resources.limits();
        if resent {
            return match live_limits(&inspected)? {
                limits if limits == requested => Ok(ContainerLimitsUpdated {
                    container_id: *container_id,
                    limits,
                }),
                _ => Err(Error::LimitsRefused {
                    container_id: *container_id,
                    refusal: LimitsRefusal::UpToDate,
                }),
            };
        }
        refuse_lowering_memory(container_id, requested, &inspected)?;
        self.docker
            .client
            .update_container(container_id.as_str(), update_body(requested))
            .await
            .map_err(|error| docker_error(container_id, error))?;
        let inspected = self
            .docker
            .client
            .inspect_container(container_id.as_str(), None)
            .await
            .map_err(|error| docker_error(container_id, error))?;
        let observed = confirm(container_id, requested, &inspected)?;
        config_operation.put(container_id, spec).await?;
        // Docker's `update` event can rescan before the spec is written.
        config_operation.republish();
        Ok(ContainerLimitsUpdated {
            container_id: *container_id,
            limits: observed,
        })
    }
}

fn refusal(stored: &ResolvedServiceSpec, spec: &ResolvedServiceSpec) -> Option<LimitsRefusal> {
    if stored.service_id != spec.service_id || stored.name != spec.name {
        return Some(LimitsRefusal::OtherService);
    }
    match compare_specs(stored, &spec.to_requested()) {
        SpecChange::NeedsUpdate => None,
        SpecChange::UpToDate => Some(LimitsRefusal::UpToDate),
        SpecChange::NeedsRecreate => Some(LimitsRefusal::NeedsRecreate),
    }
}

/// The stored spec admits only a raise, but Docker runs the Container with the
/// limits of its last update, which a crash or a restored spec can leave above
/// the spec. A raise against the spec that lowers the live limit could kill the
/// Container, so the live limit decides.
fn refuse_lowering_memory(
    container_id: &ContainerId,
    requested: ContainerLimits,
    inspected: &ContainerInspectResponse,
) -> Result<(), Error> {
    let Some(requested) = requested.memory_bytes else {
        return Ok(());
    };
    let refusal = match live_limits(inspected)?.memory_bytes {
        None => LimitsRefusal::MemoryUnlimited,
        Some(live) if requested < live => LimitsRefusal::WouldLowerMemory,
        Some(_) => return Ok(()),
    };
    Err(Error::LimitsRefused {
        container_id: *container_id,
        refusal,
    })
}

fn live_limits(inspected: &ContainerInspectResponse) -> Result<ContainerLimits, Error> {
    inspected
        .host_config
        .as_ref()
        .map(observed_limits)
        .transpose()
        .map(Option::unwrap_or_default)
}

/// Read back an updated Container. Docker has already taken the update here, so
/// a Container found stopped is not a refusal: its limits may be the new ones.
fn confirm(
    container_id: &ContainerId,
    requested: ContainerLimits,
    inspected: &ContainerInspectResponse,
) -> Result<ContainerLimits, Error> {
    if !is_running(inspected) {
        return Err(Error::StoppedAfterLimitsUpdate(*container_id));
    }
    let observed = live_limits(inspected)?;
    if observed != requested {
        return Err(Error::LimitsUnapplied {
            container_id: *container_id,
            limits: Box::new(UnappliedLimits {
                requested,
                observed,
            }),
        });
    }
    Ok(observed)
}

fn refuse_unless_running(
    container_id: &ContainerId,
    inspected: &ContainerInspectResponse,
) -> Result<(), Error> {
    if is_running(inspected) {
        Ok(())
    } else {
        Err(Error::LimitsRefused {
            container_id: *container_id,
            refusal: LimitsRefusal::NotRunning,
        })
    }
}

fn is_running(inspected: &ContainerInspectResponse) -> bool {
    inspected
        .state
        .as_ref()
        .and_then(|state| state.running)
        .unwrap_or(false)
}

fn update_body(limits: ContainerLimits) -> ContainerUpdateBody {
    ContainerUpdateBody {
        nano_cpus: limits.cpu_nanos.map(i64::from),
        memory: limits.memory_bytes.map(i64::from),
        // Docker refuses a memory limit above the swap limit, and a create that sets
        // only memory gets swap of twice that. Keep the same ratio on update.
        memory_swap: limits
            .memory_bytes
            .map(|memory| i64::from(memory).saturating_mul(2)),
        memory_reservation: limits.memory_reservation_bytes.map(i64::from),
        ..ContainerUpdateBody::default()
    }
}

/// Docker reports an unset limit as zero.
fn observed_limits(host_config: &HostConfig) -> Result<ContainerLimits, Error> {
    let set = |value: Option<i64>| value.filter(|value| *value > 0);
    Ok(ContainerLimits {
        cpu_nanos: set(host_config.nano_cpus)
            .map(CpuNanos::try_from)
            .transpose()
            .map_err(|source| Error::InvalidValue {
                field: "NanoCpus",
                source,
            })?,
        memory_bytes: set(host_config.memory)
            .map(ByteQuantity::try_from)
            .transpose()
            .map_err(|source| Error::InvalidValue {
                field: "Memory",
                source,
            })?,
        memory_reservation_bytes: set(host_config.memory_reservation)
            .map(ByteQuantity::try_from)
            .transpose()
            .map_err(|source| Error::InvalidValue {
                field: "MemoryReservation",
                source,
            })?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(memory: Option<i64>) -> ResolvedServiceSpec {
        serde_json::from_value(json!({
            "service_id": "a".repeat(32), "name": "api",
            "mode": {"mode": "replicated", "replicas": 1},
            "container": {
                "image": "api:1", "pull_policy": "missing",
                "resources": {"memory_bytes": memory}
            }
        }))
        .unwrap()
    }

    #[test]
    fn only_a_limits_change_of_the_same_service_is_admitted() {
        let stored = spec(Some(64));
        assert_eq!(refusal(&stored, &spec(Some(128))), None);
        assert_eq!(
            refusal(&stored, &spec(Some(64))),
            Some(LimitsRefusal::UpToDate)
        );
        assert_eq!(
            refusal(&stored, &spec(None)),
            Some(LimitsRefusal::NeedsRecreate)
        );
        let mut other_image = spec(Some(128));
        other_image.container.image = "api:2".into();
        assert_eq!(
            refusal(&stored, &other_image),
            Some(LimitsRefusal::NeedsRecreate)
        );
        let mut renamed = spec(Some(128));
        renamed.name = "worker".parse().unwrap();
        assert_eq!(
            refusal(&stored, &renamed),
            Some(LimitsRefusal::OtherService)
        );
    }

    #[test]
    fn an_update_sends_only_set_limits_and_twice_the_memory_as_swap() {
        let body = update_body(spec(Some(64)).container.resources.limits());
        assert_eq!(body.memory, Some(64));
        assert_eq!(body.memory_swap, Some(128));
        assert_eq!(body.nano_cpus, None);
        assert_eq!(body.memory_reservation, None);
    }

    #[test]
    fn a_stopped_container_is_refused() {
        let container_id = ContainerId::parse("c".repeat(64)).unwrap();
        let running = |running: Option<bool>| ContainerInspectResponse {
            state: Some(bollard::models::ContainerState {
                running,
                ..bollard::models::ContainerState::default()
            }),
            ..ContainerInspectResponse::default()
        };
        assert!(refuse_unless_running(&container_id, &running(Some(true))).is_ok());
        for stopped in [
            running(Some(false)),
            running(None),
            ContainerInspectResponse::default(),
        ] {
            assert!(matches!(
                refuse_unless_running(&container_id, &stopped),
                Err(Error::LimitsRefused {
                    container_id: id,
                    refusal: LimitsRefusal::NotRunning,
                }) if id == container_id
            ));
        }
    }

    #[test]
    fn a_container_stopped_after_the_update_is_unconfirmed_not_refused() {
        let container_id = ContainerId::parse("c".repeat(64)).unwrap();
        let requested = spec(Some(128)).container.resources.limits();
        let inspected = |running: bool| ContainerInspectResponse {
            state: Some(bollard::models::ContainerState {
                running: Some(running),
                ..bollard::models::ContainerState::default()
            }),
            host_config: Some(HostConfig {
                memory: Some(128),
                ..HostConfig::default()
            }),
            ..ContainerInspectResponse::default()
        };
        assert_eq!(
            confirm(&container_id, requested, &inspected(true)).unwrap(),
            requested
        );
        let stopped = confirm(&container_id, requested, &inspected(false)).unwrap_err();
        assert!(
            matches!(stopped, Error::StoppedAfterLimitsUpdate(id) if id == container_id),
            "{stopped}"
        );
        let rpc = ployz_core::RpcError::from(&stopped);
        assert_eq!(rpc.code, ployz_core::RpcErrorCode::Internal);
        assert!(rpc.details.get("reason").is_none());
    }

    #[test]
    fn a_memory_limit_below_or_onto_the_live_one_is_refused() {
        let container_id = ContainerId::parse("c".repeat(64)).unwrap();
        let live = |memory: Option<i64>| ContainerInspectResponse {
            host_config: Some(HostConfig {
                memory,
                ..HostConfig::default()
            }),
            ..ContainerInspectResponse::default()
        };
        let requested = |memory: Option<i64>| spec(memory).container.resources.limits();
        let refusal = |result: Result<(), Error>| match result {
            Ok(()) => None,
            Err(Error::LimitsRefused { refusal, .. }) => Some(refusal),
            Err(other) => panic!("{other}"),
        };
        for (live_memory, memory, expected) in [
            (Some(64), Some(128), None),
            (Some(64), Some(64), None),
            (Some(128), Some(64), Some(LimitsRefusal::WouldLowerMemory)),
            (Some(0), Some(64), Some(LimitsRefusal::MemoryUnlimited)),
            (None, Some(64), Some(LimitsRefusal::MemoryUnlimited)),
            (Some(128), None, None),
        ] {
            assert_eq!(
                refusal(refuse_lowering_memory(
                    &container_id,
                    requested(memory),
                    &live(live_memory)
                )),
                expected,
                "live {live_memory:?}, requested {memory:?}"
            );
        }
        assert_eq!(
            refusal(refuse_lowering_memory(
                &container_id,
                requested(Some(64)),
                &ContainerInspectResponse::default()
            )),
            Some(LimitsRefusal::MemoryUnlimited)
        );
    }

    #[test]
    fn docker_reports_an_unset_limit_as_zero() {
        let observed = observed_limits(&HostConfig {
            memory: Some(64),
            nano_cpus: Some(0),
            memory_reservation: None,
            ..HostConfig::default()
        })
        .unwrap();
        assert_eq!(
            observed,
            ContainerLimits {
                cpu_nanos: None,
                memory_bytes: Some(ByteQuantity::try_from(64).unwrap()),
                memory_reservation_bytes: None,
            }
        );
    }
}
