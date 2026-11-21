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

/// What each Member did with the forgets. A Member that didn't answer, or runs a ployz
/// that keeps no floors, is a gap, and makes the deploy's result partial.
#[derive(Debug, Serialize)]
pub(in crate::handlers) struct Forgot {
    machines: Vec<Answer>,
    #[serde(flatten)]
    gaps: Gaps,
    /// Why no Member could be asked at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    unreached: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct Answer {
    machine_id: MachineId,
    server: String,
    outcomes: Vec<ForgetOutcome>,
}

impl Forgot {
    /// Whether every Member answered.
    pub(in crate::handlers) fn is_complete(&self) -> bool {
        self.unreached.is_none() && self.gaps.is_complete()
    }

    /// One line per Service each Member forgot, a warning for each it refused, and one
    /// per Member that didn't answer.
    fn say(&self) {
        for answer in &self.machines {
            for outcome in &answer.outcomes {
                match outcome {
                    ForgetOutcome::Raised { service, .. } | ForgetOutcome::Held { service, .. } => {
                        ui::note(format_args!(
                            "{} forgot the older versions of {service}.",
                            answer.server
                        ));
                    }
                    ForgetOutcome::Refused { reason } => ui::warn(format!(
                        "{} refused to forget older versions: {}",
                        answer.server, reason
                    )),
                }
            }
        }
        self.gaps.warn();
        if let Some(unreached) = &self.unreached {
            ui::warn(format!(
                "Couldn't reach your servers, so none forgot the older versions: {}",
                unreached.message
            ));
        }
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
    let forgot = forgot.unwrap_or_else(|error| Forgot {
        machines: Vec::new(),
        gaps: Gaps::default(),
        unreached: Some(error.report()),
    });
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
    Ok(Forgot {
        machines: answers
            .into_iter()
            .map(|(machine_id, outcomes)| Answer {
                server: reach.name(&machine_id),
                machine_id,
                outcomes,
            })
            .collect(),
        gaps: reach.named(gaps),
        unreached: None,
    })
}

/// What one Member did with `request`; `None` when its ployz keeps no floors.
async fn on(
    client: Client,
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
        .call_shared::<op::ForgetVersions>(request, Call::once().to(&target))
        .await
        .map_err(crate::connect::rpc_error)
        .map(|forgot| Some(forgot.outcomes))
}

#[cfg(test)]
#[path = "forget_tests.rs"]
mod tests;
