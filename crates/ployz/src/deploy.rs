//! First-deploy submission and progress attachment.

use std::io::Write as _;

use hyper::Method;
use ployz_core::deploy::ContainerRuntimeSpec;
use ployz_core::placement::PlacementRefusal;
use ployz_core::{DEPLOY_ROUTE, DeployAccepted, DeployRefusal, DeployRequest};

use crate::commands::{DeployCommand, OpsWatchCommand};
use crate::mesh::http::JsonReply;
use crate::ops::{OpsExecutionError, elimination_reason, watch_to};
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: DeployCommand) -> Result<String, DeployExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let reply = remote
        .request_json_with_refusal::<_, DeployAccepted, DeployRefusal>(
            Method::POST,
            DEPLOY_ROUTE,
            Some(&deploy_request(&command)),
        )
        .await?;
    let accepted = match reply {
        JsonReply::Success(accepted) => accepted,
        JsonReply::Refused(refusal) => return Err(refusal.into()),
    };

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(
        stdout,
        "accepted operation {} on driver {}",
        accepted.operation_id, accepted.driver_machine_id
    )
    .map_err(DeployExecutionError::Output)?;
    stdout.flush().map_err(DeployExecutionError::Output)?;
    watch_to(
        &OpsWatchCommand {
            operation_id: accepted.operation_id,
            target: command.target,
        },
        &mut stdout,
    )
    .await?;
    Ok(String::new())
}

fn deploy_request(command: &DeployCommand) -> DeployRequest {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.environment = command.environment.clone();
    DeployRequest {
        namespace_name: command.namespace.clone(),
        service_name: command.service.clone(),
        image: command.image.clone(),
        runtime,
        health_gate: command.health_gate,
        placement: command.placement.clone(),
        machines: command.machines.clone(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DeployExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error(transparent)]
    Watch(#[from] OpsExecutionError),
    #[error("cannot write deploy progress: {0}")]
    Output(std::io::Error),
    #[error("namespace {namespace_name} does not exist; run `{create_command}`")]
    NamespaceNotFound {
        namespace_name: String,
        create_command: String,
    },
    #[error(
        "namespace {namespace_name} is ambiguous; remove duplicate namespace rows using one of these ids: {namespace_ids}"
    )]
    NamespaceAmbiguous {
        namespace_name: String,
        namespace_ids: String,
    },
    #[error(
        "namespace {namespace_id} is held by service {incumbent_service_name}; deploy that service or run `ployz service remove {incumbent_service_name}` first"
    )]
    DifferentService {
        namespace_id: String,
        incumbent_service_name: String,
    },
    #[error(
        "namespace {namespace_id} holds multiple services ({service_ids}); run `ployz service remove <service>` on the extras before deploying"
    )]
    MultipleServices {
        namespace_id: String,
        service_ids: String,
    },
    #[error(
        "namespace {namespace_id} has route bindings but no service; run `ployz route rm <hostname>` on each route before deploying"
    )]
    RoutesWithoutServices { namespace_id: String },
    #[error(
        "no machine can host this deploy: {eliminations}; run `ployz status` to inspect the machines"
    )]
    NoEligibleMachines { eliminations: String },
    #[error(
        "volume {volume} exists on machines {holders}; two copies of a data volume is a fork — pick one holder with `ployz deploy --machine <machine>`"
    )]
    VolumeHolderConflict { volume: String, holders: String },
    #[error(
        "machine(s) {machines} may hold this service's volumes but did not answer; recover them, or fence a lost machine with `ployz machine remove <machine>` before redeploying"
    )]
    DarkVolumeHolder { machines: String },
    #[error("volume services run exactly one replica; drop --replicas (requested {requested})")]
    VolumeReplicaLimit { requested: u16 },
    #[error(
        "--replicas alone cannot change a global service; convert it with `ployz deploy --mode replicated --replicas <n>`"
    )]
    ReplicasOnGlobalService,
    #[error("machine {machine_name} is not in the roster; run `ployz status` to see the machines")]
    UnknownPinnedMachine { machine_name: String },
    #[error("the required ployz container bridge is unavailable on the driver")]
    BridgeUnavailable,
}

impl From<DeployRefusal> for DeployExecutionError {
    fn from(refusal: DeployRefusal) -> Self {
        match refusal {
            DeployRefusal::NamespaceNotFound {
                namespace_name,
                create_command,
            } => Self::NamespaceNotFound {
                namespace_name: namespace_name.as_str().to_owned(),
                create_command,
            },
            DeployRefusal::NamespaceAmbiguous {
                namespace_name,
                namespace_ids,
            } => Self::NamespaceAmbiguous {
                namespace_name: namespace_name.as_str().to_owned(),
                namespace_ids: namespace_ids
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            },
            DeployRefusal::DifferentService {
                namespace_id,
                incumbent_service_name,
            } => Self::DifferentService {
                namespace_id: namespace_id.to_string(),
                incumbent_service_name: incumbent_service_name.as_str().to_owned(),
            },
            DeployRefusal::MultipleServices {
                namespace_id,
                service_ids,
            } => Self::MultipleServices {
                namespace_id: namespace_id.to_string(),
                service_ids: service_ids
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            },
            DeployRefusal::RoutesWithoutServices { namespace_id } => Self::RoutesWithoutServices {
                namespace_id: namespace_id.to_string(),
            },
            DeployRefusal::Placement { refusal } => Self::from(refusal),
            DeployRefusal::ReplicasOnGlobalService => Self::ReplicasOnGlobalService,
            DeployRefusal::UnknownPinnedMachine { machine_name } => Self::UnknownPinnedMachine {
                machine_name: machine_name.as_str().to_owned(),
            },
            DeployRefusal::BridgeUnavailable => Self::BridgeUnavailable,
        }
    }
}

impl From<PlacementRefusal> for DeployExecutionError {
    fn from(refusal: PlacementRefusal) -> Self {
        match refusal {
            PlacementRefusal::NoEligibleMachines { eliminations } => Self::NoEligibleMachines {
                eliminations: eliminations
                    .into_iter()
                    .map(|elimination| {
                        format!(
                            "{} ({})",
                            elimination.machine_name.as_str(),
                            elimination_reason(&elimination.reason)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            },
            PlacementRefusal::VolumeHolderConflict { volume, holders } => {
                Self::VolumeHolderConflict {
                    volume: volume.as_str().to_owned(),
                    holders: holders
                        .into_iter()
                        .map(|holder| holder.machine_name.as_str().to_owned())
                        .collect::<Vec<_>>()
                        .join(", "),
                }
            }
            PlacementRefusal::DarkVolumeHolder { machines } => Self::DarkVolumeHolder {
                machines: machines
                    .into_iter()
                    .map(|machine| machine.machine_name.as_str().to_owned())
                    .collect::<Vec<_>>()
                    .join(", "),
            },
            PlacementRefusal::VolumeReplicaLimit { requested } => Self::VolumeReplicaLimit {
                requested: requested.get(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ployz_core::HealthGatePolicy;
    use ployz_core::corrosion::{CorrosionNamespaceName, CorrosionServiceName};
    use ployz_core::deploy::{ImageReference, ServiceEnvironment};

    use super::*;

    fn command(health_gate: HealthGatePolicy) -> DeployCommand {
        DeployCommand {
            namespace: CorrosionNamespaceName::try_new("production").expect("namespace name"),
            service: CorrosionServiceName::try_new("web").expect("service name"),
            image: ImageReference::try_new("registry.example/web:latest").expect("image"),
            environment: ServiceEnvironment::from(BTreeMap::new()),
            health_gate,
            placement: None,
            machines: None,
            target: None,
        }
    }

    #[test]
    fn deploy_request_carries_the_commanded_health_gate_policy() {
        for (policy, expected) in [
            (HealthGatePolicy::Enforce, "enforce"),
            (HealthGatePolicy::Skip, "skip"),
        ] {
            let body = serde_json::to_value(deploy_request(&command(policy)))
                .expect("deploy request serializes");
            assert_eq!(body.get("health_gate"), Some(&serde_json::json!(expected)));
            assert_eq!(
                body.get("namespace_name"),
                Some(&serde_json::json!("production"))
            );
            assert_eq!(body.get("service_name"), Some(&serde_json::json!("web")));
        }
    }

    #[test]
    fn deploy_request_forwards_placement_and_pins_only_when_commanded() {
        let inherit = serde_json::to_value(deploy_request(&command(HealthGatePolicy::Enforce)))
            .expect("deploy request serializes");
        assert_eq!(inherit.get("placement"), None);
        assert_eq!(inherit.get("machines"), None);

        let mut placed = command(HealthGatePolicy::Enforce);
        placed.placement = Some(ployz_core::RequestedPlacement::Replicated {
            replicas: Some(
                ployz_core::corrosion::ServiceReplicaCount::try_new(3).expect("replica count"),
            ),
        });
        placed.machines = Some(ployz_core::RequestedPins::Any);
        let body =
            serde_json::to_value(deploy_request(&placed)).expect("deploy request serializes");
        assert_eq!(
            body.get("placement"),
            Some(&serde_json::json!({ "mode": "replicated", "replicas": 3 }))
        );
        assert_eq!(
            body.get("machines"),
            Some(&serde_json::json!({ "kind": "any" }))
        );
    }

    #[test]
    fn placement_refusal_copy_names_each_resolver() {
        let machine = ployz_core::placement::PlacementMachine {
            machine_id: ployz_core::ids::MachineRowId::generate(),
            machine_name: ployz_core::machine::MachineName::try_new("edge-a")
                .expect("machine name"),
        };
        let cases: Vec<(DeployRefusal, &[&str])> = vec![
            (
                DeployRefusal::Placement {
                    refusal: PlacementRefusal::NoEligibleMachines {
                        eliminations: vec![ployz_core::placement::PlacementElimination {
                            machine_id: machine.machine_id.clone(),
                            machine_name: machine.machine_name.clone(),
                            reason:
                                ployz_core::placement::PlacementEliminationReason::OutsidePinSet,
                        }],
                    },
                },
                &[
                    "no machine can host this deploy",
                    "edge-a",
                    "outside the pin set",
                    "ployz status",
                ],
            ),
            (
                DeployRefusal::Placement {
                    refusal: PlacementRefusal::VolumeHolderConflict {
                        volume: ployz_core::deploy::VolumeName::try_new("data").expect("volume"),
                        holders: vec![machine.clone()],
                    },
                },
                &["fork", "edge-a", "--machine <machine>"],
            ),
            (
                DeployRefusal::Placement {
                    refusal: PlacementRefusal::DarkVolumeHolder {
                        machines: vec![machine.clone()],
                    },
                },
                &["did not answer", "edge-a", "ployz machine remove"],
            ),
            (
                DeployRefusal::Placement {
                    refusal: PlacementRefusal::VolumeReplicaLimit {
                        requested: ployz_core::corrosion::ServiceReplicaCount::try_new(3)
                            .expect("replica count"),
                    },
                },
                &["exactly one replica", "drop --replicas"],
            ),
            (
                DeployRefusal::ReplicasOnGlobalService,
                &["--replicas alone", "--mode replicated"],
            ),
            (
                DeployRefusal::UnknownPinnedMachine {
                    machine_name: ployz_core::machine::MachineName::try_new("edge-z")
                        .expect("machine name"),
                },
                &["edge-z", "not in the roster", "ployz status"],
            ),
        ];
        for (refusal, expected) in cases {
            let copy = DeployExecutionError::from(refusal).to_string();
            for needle in expected {
                assert!(copy.contains(needle), "{copy:?} must contain {needle:?}");
            }
        }
    }
}
