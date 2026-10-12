//! Metric history across a Cluster's Machines.
//!
//! A Machine reads only its own Observe Store, so a Service's history is the
//! sum of every Machine's answer. Each Machine answers on its own bucket grid,
//! and two grids can differ near the 23 hour raw boundary, so the sum lands on
//! the widest step any Machine used.

use std::collections::BTreeMap;

use futures_util::future::join_all;
use ployz_core::{
    ListMachinesRequest, MachineFailure, MachineId, MachineSuccess, MachineTarget,
    QualifiedService, RpcError, RpcErrorCode,
    metrics::{
        MetricHistoryReply, MetricHistoryRequest, MetricPoints, MetricRange, MetricScope,
        ServicePoint,
    },
    op,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::{Session, invalid_argument};
use crate::connect::Client;

/// Whose history to read: one Machine's host totals, or one Service summed
/// across every Machine that ran it.
#[derive(Clone, Debug, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MetricSubject {
    Machine { machine_id: MachineId },
    Service { service: QualifiedService },
}

/// Nanosecond bounds are decimal strings so JavaScript keeps their
/// precision; `since` is inclusive and `until` exclusive.
#[derive(Clone, Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MetricHistoryInput {
    pub subject: MetricSubject,
    pub since_nanos: String,
    pub until_nanos: String,
    #[serde(default)]
    #[ts(optional)]
    pub step_nanos: Option<String>,
}

/// One subject's history. `points` sums the Machines that answered; each
/// answer is also kept whole in `machines`. A Machine that did not answer is
/// named in `unanswered`, never read as an idle one.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
pub struct MetricHistory {
    /// Absent when no Machine answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub step_nanos: Option<i64>,
    pub points: MetricPoints,
    pub machines: Vec<MachineSuccess<MetricHistoryReply>>,
    pub unanswered: Vec<MachineFailure<RpcError>>,
}

impl Session {
    /// Read one Machine's or one Service's metric history.
    ///
    /// # Errors
    /// Returns invalid input, a closed session, or a failure to list Machines.
    /// A Machine that fails is named in the result instead.
    pub async fn metric_history(
        &self,
        input: MetricHistoryInput,
    ) -> Result<MetricHistory, RpcError> {
        let range = MetricRange {
            since_nanos: nanos(&input.since_nanos, "since_nanos")?,
            until_nanos: nanos(&input.until_nanos, "until_nanos")?,
        };
        let step_nanos = input
            .step_nanos
            .as_deref()
            .map(|step| nanos(step, "step_nanos"))
            .transpose()?;
        let client = self.client()?;
        self.until_closed(metric_history(&client, input.subject, range, step_nanos))
            .await
    }
}

/// Read `subject`'s metric history over `range`, fanning a Service out to
/// every Machine that invites an RPC.
///
/// # Errors
/// Returns invalid input or a failure to list Machines.
pub async fn metric_history(
    client: &Client,
    subject: MetricSubject,
    range: MetricRange,
    step_nanos: Option<i64>,
) -> Result<MetricHistory, RpcError> {
    let (scope, machine) = match subject {
        MetricSubject::Machine { machine_id } => (MetricScope::Machine, Some(machine_id)),
        MetricSubject::Service { service } => (MetricScope::Service { service }, None),
    };
    let request = MetricHistoryRequest {
        scope,
        range,
        step_nanos,
    };
    request.validate().map_err(invalid_argument)?;
    let machines = match machine {
        Some(machine_id) => Asked::one(machine_id),
        None => Asked::cluster(client).await?,
    };
    let answers = join_all(machines.reachable.into_iter().map(|machine_id| {
        let mut client = client.clone();
        let request = request.clone();
        async move {
            let answer = client
                .read::<op::MetricHistory>(request, &MachineTarget::from(&machine_id))
                .await;
            (machine_id, answer)
        }
    }))
    .await;
    let mut replies = Vec::new();
    let mut unanswered = machines.unreachable;
    for (machine_id, answer) in answers {
        match answer {
            Ok(value) => replies.push(MachineSuccess { machine_id, value }),
            Err(error) => unanswered.push(MachineFailure { machine_id, error }),
        }
    }
    Ok(merge(&request, replies, unanswered))
}

struct Asked {
    reachable: Vec<MachineId>,
    unreachable: Vec<MachineFailure<RpcError>>,
}

impl Asked {
    fn one(machine_id: MachineId) -> Self {
        Self {
            reachable: vec![machine_id],
            unreachable: Vec::new(),
        }
    }

    async fn cluster(client: &Client) -> Result<Self, RpcError> {
        let listed = client
            .clone()
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map_err(RpcError::from)?;
        let mut asked = Self {
            reachable: Vec::new(),
            unreachable: Vec::new(),
        };
        for machine in listed.machines {
            if machine.invites_rpc() {
                asked.reachable.push(machine.machine.id);
            } else {
                asked.unreachable.push(MachineFailure {
                    machine_id: machine.machine.id,
                    error: unavailable("the Server is not up, so it was not asked"),
                });
            }
        }
        Ok(asked)
    }
}

fn merge(
    request: &MetricHistoryRequest,
    replies: Vec<MachineSuccess<MetricHistoryReply>>,
    mut unanswered: Vec<MachineFailure<RpcError>>,
) -> MetricHistory {
    let step_nanos = replies.iter().map(|reply| reply.value.step_nanos).max();
    let points = match request.scope {
        MetricScope::Machine => replies.first().map_or_else(
            || MetricPoints::Machine(Vec::new()),
            |reply| reply.value.points.clone(),
        ),
        MetricScope::Service { .. } => {
            let mut sum = ServiceSum::default();
            for reply in &replies {
                let MetricPoints::Service(points) = &reply.value.points else {
                    unanswered.push(MachineFailure {
                        machine_id: reply.machine_id,
                        error: unavailable("the Server answered with host totals, not a Service"),
                    });
                    continue;
                };
                let step = step_nanos.unwrap_or(reply.value.step_nanos);
                sum.add(request.range.since_nanos, step, points);
            }
            MetricPoints::Service(sum.points())
        }
    };
    MetricHistory {
        step_nanos,
        points,
        machines: replies,
        unanswered,
    }
}

/// Every Machine's points re-bucketed onto one grid and summed per bucket.
#[derive(Default)]
struct ServiceSum {
    buckets: BTreeMap<i64, Total>,
}

impl ServiceSum {
    /// Averages one Machine's points into `step` buckets starting at `since`,
    /// then adds each bucket to the total.
    fn add(&mut self, since: i64, step: i64, points: &[ServicePoint]) {
        let mut means: BTreeMap<i64, Average> = BTreeMap::new();
        for point in points {
            let start = since + (point.ts - since).div_euclid(step) * step;
            means.entry(start).or_default().add(point);
        }
        for (start, mean) in means {
            self.buckets.entry(start).or_default().add(mean);
        }
    }

    fn points(self) -> Vec<ServicePoint> {
        self.buckets
            .into_iter()
            .map(|(ts, total)| total.point(ts))
            .collect()
    }
}

const RATES: usize = 5;

fn rates(point: &ServicePoint) -> [Option<f64>; RATES] {
    [point.cpu_usec, point.rx, point.tx, point.io_r, point.io_w]
}

/// One Machine's mean over the narrower buckets that fall in a wide one.
struct Average {
    rates: [(f64, u32); RATES],
    mem: (u128, u32),
    /// `None` once a point had no limit.
    limit: Option<(u128, u32)>,
}

impl Default for Average {
    fn default() -> Self {
        Self {
            rates: [(0.0, 0); RATES],
            mem: (0, 0),
            limit: Some((0, 0)),
        }
    }
}

impl Average {
    fn add(&mut self, point: &ServicePoint) {
        for ((sum, count), value) in self.rates.iter_mut().zip(rates(point)) {
            if let Some(value) = value {
                *sum += value;
                *count += 1;
            }
        }
        if let Some(mem) = point.mem_bytes {
            self.mem.0 += u128::from(mem);
            self.mem.1 += 1;
        }
        self.limit = self
            .limit
            .zip(point.mem_limit)
            .map(|((sum, count), limit)| (sum + u128::from(limit), count + 1));
    }
}

fn mean_bytes((sum, count): (u128, u32)) -> Option<u64> {
    (count > 0).then(|| u64::try_from(sum / u128::from(count)).unwrap_or(u64::MAX))
}

/// The sum across Machines of one bucket, following the Machine's own sum
/// across Deployments: a field is the sum of the values present, absent only
/// when every Machine lacks it, and the limit is absent once one lacks it.
struct Total {
    rates: [Option<f64>; RATES],
    mem: Option<u64>,
    limit: Option<u64>,
}

impl Default for Total {
    fn default() -> Self {
        Self {
            rates: [None; RATES],
            mem: None,
            limit: Some(0),
        }
    }
}

impl Total {
    fn add(&mut self, mean: Average) {
        for (total, (sum, count)) in self.rates.iter_mut().zip(mean.rates) {
            if count > 0 {
                *total = Some(total.unwrap_or(0.0) + sum / f64::from(count));
            }
        }
        if let Some(mem) = mean_bytes(mean.mem) {
            self.mem = Some(self.mem.unwrap_or(0).saturating_add(mem));
        }
        self.limit = self
            .limit
            .zip(mean.limit.and_then(mean_bytes))
            .map(|(total, limit)| total.saturating_add(limit));
    }

    fn point(self, ts: i64) -> ServicePoint {
        let [cpu_usec, rx, tx, io_r, io_w] = self.rates;
        ServicePoint {
            ts,
            cpu_usec,
            mem_bytes: self.mem,
            mem_limit: self.limit,
            rx,
            tx,
            io_r,
            io_w,
        }
    }
}

fn nanos(value: &str, field: &str) -> Result<i64, RpcError> {
    value
        .parse()
        .map_err(|_| invalid_argument(format!("{field} must be a decimal integer")))
}

fn unavailable(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.into(),
        details: Value::Null,
        cause: Vec::new(),
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
