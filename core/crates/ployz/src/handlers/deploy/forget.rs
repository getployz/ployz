//! The forget fan-out of `ployz deploy --forget-older`. Once the Store admits a Deploy whose
//! versions are marked to forget, every Member is asked to raise its floors, not only the
//! deploy's targets: a Server that runs nothing of this Deploy must still refuse an older
//! version Cloud later sends it.

use ployz_core::{
    DescribeContractRequest, FORGET_VERSIONS_CAPABILITY, ForgetOutcome, ForgetVersionsRequest,
    MachineId, MachineTarget, QualifiedService, RpcError,
    authority::{
        ServiceVersion, Signed,
        version::{MAX_CHECKED, MAX_PRESENTED},
    },
    op,
};
use serde::Serialize;

use super::super::device::Reach;
use super::super::{Error, route};
use crate::cluster::{Call, Deadline};
use crate::connect::{Client, TARGET_RPC_TIMEOUT};
use crate::ui::{self, Gaps};

/// What each Member did with the forgets. A Member that didn't answer, refused, or runs a
/// ployz that keeps no floors still admits older versions, so it makes the deploy's result
/// partial.
#[derive(Debug)]
pub(in crate::handlers) enum Forgot {
    Asked {
        machines: Vec<Answer>,
        gaps: Gaps,
    },
    /// No Member could be asked at all.
    Unreached(RpcError),
}

#[derive(Debug, Serialize)]
pub(in crate::handlers) struct Answer {
    machine_id: MachineId,
    server: String,
    #[serde(flatten)]
    forgets: Forgets,
}

/// What one Member answered, batch by batch.
#[derive(Debug, Serialize)]
struct Forgets {
    outcomes: Vec<ForgetOutcome>,
    /// Set when a batch after the first failed: the Services of that batch and every later
    /// one, which this Member was never asked about or didn't answer for.
    #[serde(skip_serializing_if = "Option::is_none")]
    missed: Option<Missed>,
}

#[derive(Debug, Serialize)]
struct Missed {
    services: Vec<QualifiedService>,
    error: RpcError,
}

/// The JSON of [`Forgot`]: each gap carries its Server's name beside its id.
#[derive(Serialize)]
#[serde(untagged)]
enum View<'a> {
    Asked {
        machines: &'a [Answer],
        failures: Vec<Failed<'a>>,
        omitted: Vec<Named<'a>>,
    },
    Unreached {
        unreached: &'a RpcError,
    },
}

#[derive(Serialize)]
struct Failed<'a> {
    machine_id: MachineId,
    server: String,
    error: &'a RpcError,
}

#[derive(Serialize)]
struct Named<'a> {
    machine_id: &'a MachineId,
    server: String,
}

impl Serialize for Forgot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Asked { machines, gaps } => View::Asked {
                machines,
                failures: gaps
                    .failures
                    .iter()
                    .map(|failure| Failed {
                        machine_id: failure.machine_id,
                        server: gaps.name(&failure.machine_id),
                        error: &failure.error,
                    })
                    .collect(),
                omitted: gaps
                    .omitted
                    .iter()
                    .map(|machine_id| Named {
                        machine_id,
                        server: gaps.name(machine_id),
                    })
                    .collect(),
            },
            Self::Unreached(unreached) => View::Unreached { unreached },
        }
        .serialize(serializer)
    }
}

impl Forgot {
    /// Whether every Member forgot.
    pub(in crate::handlers) fn is_complete(&self) -> bool {
        match self {
            Self::Asked { machines, gaps } => {
                gaps.is_complete()
                    && machines
                        .iter()
                        .all(|answer| answer.forgets.missed.is_none())
                    && machines
                        .iter()
                        .flat_map(|answer| &answer.forgets.outcomes)
                        .all(|outcome| !matches!(outcome, ForgetOutcome::Refused { .. }))
            }
            Self::Unreached(_) => false,
        }
    }

    /// One line per Service each Member forgot, and a warning for each it refused or
    /// missed and each Member that didn't answer.
    fn say(&self) {
        let (machines, gaps) = match self {
            Self::Asked { machines, gaps } => (machines, gaps),
            Self::Unreached(unreached) => {
                ui::warn(format!(
                    "Couldn't reach your servers, so none forgot the older versions: {}",
                    unreached.message
                ));
                return;
            }
        };
        for answer in machines {
            for outcome in &answer.forgets.outcomes {
                match outcome {
                    ForgetOutcome::Raised { service, .. } | ForgetOutcome::Held { service, .. } => {
                        ui::note(format_args!(
                            "{} forgot the older versions of {service}.",
                            answer.server
                        ));
                    }
                    ForgetOutcome::Refused { reason } => ui::warn(format!(
                        "{} refused to forget the older versions, so it can still start them: {}",
                        answer.server, reason
                    )),
                }
            }
            if let Some(missed) = &answer.forgets.missed {
                let services: Vec<String> =
                    missed.services.iter().map(ToString::to_string).collect();
                ui::warn(format!(
                    "{} didn't forget the older versions of {}, so it can still start them: {}",
                    answer.server,
                    services.join(", "),
                    missed.error.message
                ));
            }
        }
        gaps.warn_that("it can still start the older versions");
    }
}

/// Ask every Member to forget the versions before each of `versions`, and say what each
/// did. Reaching no Member at all is a gap too: the Deploy is already admitted.
pub(in crate::handlers) fn everywhere(
    matches: &clap::ArgMatches,
    versions: Vec<Signed<ServiceVersion>>,
) -> Result<Forgot, Error> {
    let context = matches.get_one::<String>("context").map(String::as_str);
    let forgot = super::super::runtime()?.block_on(async {
        let mut client = route::connect(matches, context).await?;
        ask(&mut client, &versions).await
    });
    let forgot = forgot.unwrap_or_else(|error| Forgot::Unreached(error.report()));
    forgot.say();
    Ok(forgot)
}

/// The most versions one forget request carries. Each version this Device signs carries
/// its one signature and no bundle, so a Machine verifies it once, and a batch this size
/// stays within what one request may cost and carry.
const BATCH: usize = if MAX_CHECKED < MAX_PRESENTED {
    MAX_CHECKED
} else {
    MAX_PRESENTED
};

pub(super) async fn ask(
    client: &mut Client,
    versions: &[Signed<ServiceVersion>],
) -> Result<Forgot, Error> {
    let batches: Vec<ForgetVersionsRequest> = versions
        .chunks(BATCH)
        .map(|batch| ForgetVersionsRequest {
            versions: batch.to_vec(),
        })
        .collect();
    let mut reach = Reach::of(client).await?;
    let client: &Client = client;
    // A Server that joined while this ran was sealed the floors its admitting Server held
    // then, so it is asked too.
    let (answers, gaps) = reach
        .fan_out_through_joins(client, |machine_id| {
            on(client.clone(), machine_id, &batches)
        })
        .await;
    Ok(Forgot::Asked {
        machines: answers
            .into_iter()
            .map(|(machine_id, forgets)| Answer {
                server: reach.name(&machine_id),
                machine_id,
                forgets,
            })
            .collect(),
        gaps: reach.named(gaps),
    })
}

/// What one Member did with `batches`, sent in order; `None` when its ployz keeps no
/// floors. Each call has its own deadline, so a Member that holds the connection open
/// and never answers is a gap, not a stall. A failed first batch fails the Member; a later
/// one ends its answer with what it missed.
async fn on(
    mut client: Client,
    machine_id: MachineId,
    batches: &[ForgetVersionsRequest],
) -> Result<Option<Forgets>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    let contract = client
        .call_shared::<op::DescribeContract>(
            DescribeContractRequest {},
            Call::once()
                .to(&target)
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)?;
    if !contract.supports(FORGET_VERSIONS_CAPABILITY) {
        return Ok(None);
    }
    let mut outcomes = Vec::new();
    for (sent, batch) in batches.iter().enumerate() {
        let forgot = client
            // A forget only raises floors, so a resend after a lost reply is harmless.
            .call::<op::ForgetVersions>(
                batch.clone(),
                Call::keyed().to(&target).budget(TARGET_RPC_TIMEOUT),
            )
            .await
            .map_err(crate::connect::rpc_error);
        match forgot {
            Ok(forgot) => outcomes.extend(forgot.outcomes),
            Err(error) if sent == 0 => return Err(error),
            Err(error) => {
                return Ok(Some(Forgets {
                    outcomes,
                    missed: Some(Missed {
                        services: services(batches.get(sent..).unwrap_or_default()),
                        error,
                    }),
                }));
            }
        }
    }
    Ok(Some(Forgets {
        outcomes,
        missed: None,
    }))
}

/// The Services `batches` name, read from the versions this Device signed.
fn services(batches: &[ForgetVersionsRequest]) -> Vec<QualifiedService> {
    batches
        .iter()
        .flat_map(|batch| &batch.versions)
        .filter_map(|version| version.open().ok())
        .map(|body| QualifiedService::new(body.namespace, body.service))
        .collect()
}

#[cfg(test)]
#[path = "forget_tests.rs"]
mod tests;
