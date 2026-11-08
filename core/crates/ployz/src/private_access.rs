//! Private Access: each Machine's daemon places the Tailscale router that advertises the
//! Cluster network to a tailnet, through SetPrivateAccess. The CLI seals the auth key to
//! the Cluster and asks each Machine; nothing here builds a spec or names an image.

use futures_util::{StreamExt, stream};
use ployz_core::{
    Ciphertext, ContainerRuntimeObservation, EnvironmentValues, MachineFailure, MachineId,
    MachineName, MachineObservation, PartialResult, PlatformPurpose, PrivateAccessProblem,
    PrivateAccessSet, QualifiedService, RowStanding, RpcError, RpcErrorCode,
    SET_PRIVATE_ACCESS_CAPABILITY, Sealing, SealingPublicKey, ServiceObservation,
    SetPrivateAccessRequest, private_access_sealed_key, seal_platform,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::{
    connect::Client,
    deploy::{MachineOperations, RestartTolerant},
    failure::Failure,
    ingress::unsupported,
};

/// How many Machines a command sets at once. Each router starts on its own, so a slow
/// Machine must not hold back the others.
const AT_ONCE: usize = 8;

/// A Machine whose router a command sets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AccessMachine {
    pub(crate) id: MachineId,
    pub(crate) name: MachineName,
    /// Whether its daemon may serve SetPrivateAccess. One that advertises other
    /// capabilities but not this one runs no router and is named as needing a newer
    /// Ployz when Private Access turns on.
    pub(crate) supported: bool,
}

/// The Machines that run a router: every reachable one that isn't leaving.
pub(crate) fn access_machines(observed: &[MachineObservation]) -> Vec<AccessMachine> {
    observed
        .iter()
        .filter(|machine| {
            machine.invites_rpc()
                && match machine.machine.standing() {
                    RowStanding::Registered | RowStanding::Versioned => true,
                    RowStanding::Leaving { .. } => false,
                }
        })
        .map(|machine| {
            let capabilities = &machine.machine.runtime.capabilities;
            AccessMachine {
                id: machine.machine.id,
                name: machine.machine.name.clone(),
                // An empty list is a row that predates it, which claims nothing, so the
                // daemon is asked and answers for itself.
                supported: capabilities.is_empty()
                    || capabilities.contains(SET_PRIVATE_ACCESS_CAPABILITY),
            }
        })
        .collect()
}

/// The sealed auth key the newest observed router carries, so a joiner runs the same
/// router as the rest.
pub(crate) fn cluster_sealed_key(services: &[ServiceObservation]) -> Option<Ciphertext> {
    services
        .iter()
        .filter(|service| service.identity == QualifiedService::system_tailscale())
        .flat_map(|service| service.containers.iter().map(|copy| copy.as_observation()))
        .max_by_key(|copy| (copy.created_at_unix_nanos, copy.container_id))
        .and_then(|router| private_access_sealed_key(&router.resolved_spec.container.labels))
}

/// The Machines whose router runs now.
fn running(services: &[ServiceObservation]) -> std::collections::BTreeSet<MachineId> {
    services
        .iter()
        .filter(|service| service.identity == QualifiedService::system_tailscale())
        .flat_map(|service| service.containers.iter().map(|copy| copy.as_observation()))
        .filter(|copy| matches!(copy.runtime, ContainerRuntimeObservation::Running { .. }))
        .map(|copy| copy.machine_id)
        .collect()
}

/// The Cluster's Machines and what Private Access is on each, as observed.
pub(crate) struct Access {
    pub(crate) machines: Vec<AccessMachine>,
    pub(crate) running: std::collections::BTreeSet<MachineId>,
    /// The listing the rest derives from, with the Machines that didn't answer it.
    pub(crate) listed: PartialResult<Vec<ployz_core::ContainerObservation>, RpcError>,
    /// The Cluster's Sealing Key, from the first Machine that holds it.
    pub(crate) sealing: Option<SealingPublicKey>,
}

pub(crate) async fn observe_access(client: &mut Client) -> Result<Access, Failure> {
    let machines = client.machines().await?;
    let live = client
        .live_services_from(&machines, EnvironmentValues::Redacted)
        .await?;
    let services = live.services();
    let sealing = machines
        .iter()
        .find_map(|machine| match &machine.machine.runtime.sealing {
            Some(Sealing::Held { public_key }) => Some(*public_key),
            Some(Sealing::NeedsAccess | Sealing::AwaitsRecovery { .. }) | None => None,
        });
    Ok(Access {
        machines: access_machines(&machines),
        running: running(&services),
        listed: live.containers,
        sealing,
    })
}

/// One Machine's router after a command set or read it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineAccess {
    pub machine: MachineId,
    pub server: String,
    pub router: bool,
}

/// What [`turn_on`] or [`turn_off`] left on every Machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct AccessChanged {
    pub machines: Vec<MachineAccess>,
    /// Each Machine whose router isn't as asked after, with why.
    pub failures: Vec<MachineFailure<RpcError>>,
}

/// Seal `key` to the Cluster.
///
/// # Errors
///
/// Fails when no Machine holds the Sealing Key to seal it for.
pub(crate) fn seal_key(observed: &Access, key: &str) -> Result<Ciphertext, Failure> {
    let Some(sealing) = &observed.sealing else {
        return Err(Failure::coded(
            RpcErrorCode::Unavailable,
            "No online Server holds the Cluster's Sealing Key, so the auth key can't be sealed.",
        ));
    };
    seal_platform(sealing, PlatformPurpose::PrivateAccess, key.as_bytes()).map_err(|error| {
        Failure::caused(
            RpcErrorCode::Internal,
            "Could not seal the auth key for this Cluster",
            error,
        )
    })
}

/// What one Machine's daemon answered, or why it wasn't asked to the end.
enum Answer {
    Set(Box<PrivateAccessSet>),
    Refused(RpcError),
    Cancelled,
}

async fn set_all<C: MachineOperations>(
    client: &C,
    machines: &[AccessMachine],
    request: &SetPrivateAccessRequest,
    cancel: &CancellationToken,
) -> Vec<Answer> {
    let client = &RestartTolerant::new(client, cancel);
    stream::iter(machines.to_vec())
        .map(|machine| async move {
            tokio::select! {
                answer = client.set_private_access(&machine.id, request.clone()) => match answer {
                    Ok(set) => Answer::Set(Box::new(set)),
                    Err(error) => Answer::Refused(error),
                },
                () = cancel.cancelled() => Answer::Cancelled,
            }
        })
        .buffered(AT_ONCE)
        .collect()
        .await
}

/// Place the router on every Machine, with the auth key `sealed`, `AT_ONCE` at a time.
/// A router that was placed and did not serve is the Machine's failure, and stays for a
/// corrected key to replace.
///
/// # Errors
///
/// Never; a Machine's refusal or failed router is one of the result's failures.
pub(crate) async fn turn_on<C: MachineOperations>(
    client: &C,
    machines: Vec<AccessMachine>,
    sealed: Ciphertext,
    cancel: &CancellationToken,
) -> AccessChanged {
    let mut changed = AccessChanged::default();
    let (asked, unable): (Vec<_>, Vec<_>) =
        machines.into_iter().partition(|machine| machine.supported);
    for machine in unable {
        changed.failures.push(MachineFailure {
            machine_id: machine.id,
            error: unsupported(&machine.name, SET_PRIVATE_ACCESS_CAPABILITY),
        });
    }
    let request = SetPrivateAccessRequest {
        enabled: true,
        sealed_auth_key: Some(sealed),
    };
    let answers = set_all(client, &asked, &request, cancel).await;
    for (machine, answer) in asked.into_iter().zip(answers) {
        match answer {
            Answer::Set(set) => match set.failure {
                None => changed.machines.push(MachineAccess {
                    machine: machine.id,
                    server: machine.name.to_string(),
                    router: set.router.is_some(),
                }),
                Some(failure) => changed.failures.push(MachineFailure {
                    machine_id: machine.id,
                    error: not_serving(&machine.name, failure.problem, failure.detail),
                }),
            },
            Answer::Refused(error) => changed.failures.push(MachineFailure {
                machine_id: machine.id,
                error,
            }),
            Answer::Cancelled => changed.failures.push(MachineFailure {
                machine_id: machine.id,
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: format!(
                        "{} was still starting its router when the command was cancelled",
                        machine.name
                    ),
                    details: serde_json::Value::Null,
                    cause: Vec::new(),
                },
            }),
        }
    }
    changed
}

/// Remove the router and its auth key from every Machine. A Machine whose daemon
/// predates Private Access runs no router, so it has nothing to remove.
pub(crate) async fn turn_off<C: MachineOperations>(
    client: &C,
    machines: Vec<AccessMachine>,
    cancel: &CancellationToken,
) -> AccessChanged {
    let mut changed = AccessChanged::default();
    let (asked, unable): (Vec<_>, Vec<_>) =
        machines.into_iter().partition(|machine| machine.supported);
    changed
        .machines
        .extend(unable.into_iter().map(|machine| MachineAccess {
            machine: machine.id,
            server: machine.name.to_string(),
            router: false,
        }));
    let request = SetPrivateAccessRequest {
        enabled: false,
        sealed_auth_key: None,
    };
    let answers = set_all(client, &asked, &request, cancel).await;
    for (machine, answer) in asked.into_iter().zip(answers) {
        match answer {
            Answer::Set(_) => changed.machines.push(MachineAccess {
                machine: machine.id,
                server: machine.name.to_string(),
                router: false,
            }),
            Answer::Refused(error) => changed.failures.push(MachineFailure {
                machine_id: machine.id,
                error,
            }),
            Answer::Cancelled => changed.failures.push(MachineFailure {
                machine_id: machine.id,
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: format!(
                        "{} was still removing its router when the command was cancelled",
                        machine.name
                    ),
                    details: serde_json::Value::Null,
                    cause: Vec::new(),
                },
            }),
        }
    }
    changed
}

/// Why a router isn't serving, as a refusal naming `server`, with the router's last
/// word as its cause.
fn not_serving(server: &MachineName, problem: PrivateAccessProblem, detail: String) -> RpcError {
    let message = match problem {
        PrivateAccessProblem::KeyRefused => {
            format!("Tailscale refused the auth key on {server}. It may be revoked or expired.")
        }
        PrivateAccessProblem::NotRunning | PrivateAccessProblem::Unrecognized => {
            format!("The Tailscale router on {server} isn't running.")
        }
    };
    RpcError {
        code: RpcErrorCode::Unavailable,
        message,
        details: serde_json::json!({ "problem": problem }),
        cause: if detail.is_empty() {
            Vec::new()
        } else {
            vec![detail]
        },
    }
}

/// Whether `error` is a router that Tailscale refused the auth key for.
pub(crate) fn key_refused(error: &RpcError) -> bool {
    error.details.get("problem") == Some(&serde_json::json!(PrivateAccessProblem::KeyRefused))
}

/// Why the router isn't serving after `set`, on one line, or `None` when it serves.
pub(crate) fn failure_of(set: &PrivateAccessSet) -> Option<String> {
    let failure = set.failure.as_ref()?;
    let said = match failure.problem {
        PrivateAccessProblem::KeyRefused => {
            "Tailscale refused the auth key. It may be revoked or expired."
        }
        PrivateAccessProblem::NotRunning | PrivateAccessProblem::Unrecognized => {
            "The Tailscale router isn't running."
        }
    };
    Some(if failure.detail.is_empty() {
        said.to_owned()
    } else {
        format!("{said} ({})", failure.detail)
    })
}

/// Where each Machine's router stands, from what each one lists.
pub(crate) fn shown(observed: &Access) -> Vec<MachineAccess> {
    observed
        .machines
        .iter()
        .map(|machine| MachineAccess {
            machine: machine.id,
            server: machine.name.to_string(),
            router: observed.running.contains(&machine.id),
        })
        .collect()
}

#[cfg(test)]
#[path = "private_access_tests.rs"]
mod tests;
