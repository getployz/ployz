//! What a Service or a Server uses now, read live from each Machine's Docker. History
//! lives in Ployz Cloud.

use crate::cluster::Call;

use futures_util::future::join_all;
use ployz_core::{
    MachineFailure, MachineId, MachineObservation, MachineSuccess, MachineTarget, QualifiedService,
    RpcError,
    metrics::{MetricScope, MetricsLiveReply, MetricsLiveRequest},
    op,
};
use serde::Serialize;

use crate::connect::Client;

/// Whose numbers to read: one Machine's Service containers, or one Service's across
/// every Machine that runs it.
#[derive(Clone, Debug)]
pub enum MetricSubject {
    Machine { machine_id: MachineId },
    Service { service: QualifiedService },
}

/// Each Machine's answer whole. A Machine that did not answer is named in
/// `unanswered`, never read as an idle one.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LiveMetrics {
    pub machines: Vec<MachineSuccess<MetricsLiveReply>>,
    pub unanswered: Vec<MachineFailure<RpcError>>,
}

/// Read `subject` live from the Machines in `listed`, or the one Machine it names.
pub async fn metrics_live(
    client: &Client,
    subject: MetricSubject,
    listed: &[MachineObservation],
) -> LiveMetrics {
    let (scope, asked) = match subject {
        MetricSubject::Machine { machine_id } => (MetricScope::Machine, vec![machine_id]),
        MetricSubject::Service { service } => (
            MetricScope::Service { service },
            listed.iter().map(|machine| machine.machine.id).collect(),
        ),
    };
    let mut unanswered = Vec::new();
    let mut reachable = Vec::new();
    for machine_id in asked {
        let invites = listed
            .iter()
            .find(|machine| machine.machine.id == machine_id)
            .is_none_or(MachineObservation::invites_rpc);
        if invites {
            reachable.push(machine_id);
        } else {
            unanswered.push(MachineFailure {
                machine_id,
                error: RpcError::unavailable("the Server is not up, so it was not asked"),
            });
        }
    }
    let request = MetricsLiveRequest { scope };
    let answers = join_all(reachable.into_iter().map(|machine_id| {
        let mut client = client.clone();
        let request = request.clone();
        async move {
            let answer = client
                .call::<op::MetricsLive>(
                    request,
                    Call::target_read(&MachineTarget::from(&machine_id)),
                )
                .await
                .map_err(crate::connect::rpc_error);
            (machine_id, answer)
        }
    }))
    .await;
    let mut machines = Vec::new();
    for (machine_id, answer) in answers {
        match answer {
            Ok(value) => machines.push(MachineSuccess { machine_id, value }),
            Err(error) => unanswered.push(MachineFailure { machine_id, error }),
        }
    }
    LiveMetrics {
        machines,
        unanswered,
    }
}
