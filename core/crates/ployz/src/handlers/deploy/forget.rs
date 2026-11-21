//! The forget fan-out of `ployz deploy --forget-older`. Once the Store admits a Deploy whose
//! versions are marked to forget, every Member is asked to raise its floors, not only the
//! deploy's targets: a Server that runs nothing of this Deploy must still refuse an older
//! version Cloud later sends it.

use ployz_core::{
    FORGET_VERSIONS_CAPABILITY, ForgetOutcome, ForgetVersionsRequest, MachineId, MachineTarget,
    RpcError,
    authority::{ServiceVersion, Signed},
    op,
};
use serde::Serialize;

use super::super::device::Reach;
use super::super::{Error, route};
use crate::cluster::Call;
use crate::connect::Client;
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
    outcomes: Vec<ForgetOutcome>,
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
                        .flat_map(|answer| &answer.outcomes)
                        .all(|outcome| !matches!(outcome, ForgetOutcome::Refused { .. }))
            }
            Self::Unreached(_) => false,
        }
    }

    /// One line per Service each Member forgot, and a warning for each it refused and
    /// each Member that didn't answer.
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
            for outcome in &answer.outcomes {
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
        ask(&mut client, ForgetVersionsRequest { versions }).await
    });
    let forgot = forgot.unwrap_or_else(|error| Forgot::Unreached(error.report()));
    forgot.say();
    Ok(forgot)
}

pub(super) async fn ask(
    client: &mut Client,
    request: ForgetVersionsRequest,
) -> Result<Forgot, Error> {
    let reach = Reach::of(client).await?;
    let (answers, gaps) = reach
        .fan_out(|machine_id| on(client.clone(), machine_id, request.clone()))
        .await;
    Ok(Forgot::Asked {
        machines: answers
            .into_iter()
            .map(|(machine_id, outcomes)| Answer {
                server: reach.name(&machine_id),
                machine_id,
                outcomes,
            })
            .collect(),
        gaps: reach.named(gaps),
    })
}

/// What one Member did with `request`; `None` when its ployz keeps no floors.
async fn on(
    mut client: Client,
    machine_id: MachineId,
    request: ForgetVersionsRequest,
) -> Result<Option<Vec<ForgetOutcome>>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    if !client
        .supports(Some(&target), FORGET_VERSIONS_CAPABILITY)
        .await
        .map_err(crate::connect::rpc_error)?
    {
        return Ok(None);
    }
    client
        // A forget only raises floors, so a resend after a lost reply is harmless.
        .call::<op::ForgetVersions>(request, Call::keyed().to(&target))
        .await
        .map_err(crate::connect::rpc_error)
        .map(|forgot| Some(forgot.outcomes))
}

#[cfg(test)]
#[path = "forget_tests.rs"]
mod tests;
