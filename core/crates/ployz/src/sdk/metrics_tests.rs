use ployz_core::{
    MachineFailure, MachineId, MachineSuccess, QualifiedService, RpcErrorCode,
    metrics::{
        MetricGap, MetricGapReason, MetricHistoryReply, MetricHistoryRequest, MetricPoints,
        MetricRange, MetricScope, ServicePoint,
    },
};

use super::{MetricHistory, merge, unavailable};

const SINCE: i64 = 1_000_000_000_000;
const MIN: i64 = 60_000_000_000;

fn request() -> MetricHistoryRequest {
    MetricHistoryRequest {
        scope: MetricScope::Service {
            service: QualifiedService::parse("prod/web").unwrap(),
        },
        range: MetricRange {
            since_nanos: SINCE,
            until_nanos: SINCE + 10 * MIN,
        },
        step_nanos: None,
    }
}

fn point(minute: i64, cpu: f64, mem: u64, limit: Option<u64>) -> ServicePoint {
    ServicePoint {
        ts: SINCE + minute * MIN,
        cpu_usec: Some(cpu),
        mem_bytes: Some(mem),
        mem_limit: limit,
        rx: None,
        tx: None,
        io_r: None,
        io_w: None,
    }
}

fn answered(step: i64, points: Vec<ServicePoint>) -> MachineSuccess<MetricHistoryReply> {
    MachineSuccess {
        machine_id: MachineId::random(),
        value: MetricHistoryReply {
            points: MetricPoints::Service(points),
            step_nanos: step,
            gaps: Vec::new(),
            markers: Vec::new(),
        },
    }
}

fn service_points(history: &MetricHistory) -> &[ServicePoint] {
    match &history.points {
        MetricPoints::Service(points) => points,
        MetricPoints::Machine(_) => panic!("a Service query sums Service points"),
    }
}

#[test]
fn a_machine_that_times_out_is_named_and_the_rest_still_sum() {
    let slow = MachineId::random();
    let fast = answered(MIN, vec![point(0, 2.0, 100, Some(1_000))]);
    let timeout = MachineFailure {
        machine_id: slow,
        error: unavailable("deadline exceeded"),
    };

    let history = merge(&request(), vec![fast.clone()], vec![timeout]);

    assert_eq!(service_points(&history), [point(0, 2.0, 100, Some(1_000))]);
    assert_eq!(history.machines, [fast]);
    let [named] = history.unanswered.as_slice() else {
        panic!("one Machine is named: {:?}", history.unanswered);
    };
    assert_eq!(named.machine_id, slow);
    assert_eq!(named.error.code, RpcErrorCode::Unavailable);
}

#[test]
fn a_store_that_timed_out_keeps_its_gap_and_adds_nothing() {
    let mut store_timeout = answered(MIN, Vec::new());
    store_timeout.value.gaps = vec![MetricGap {
        from: SINCE,
        to: SINCE + 10 * MIN,
        reason: MetricGapReason::Timeout,
    }];
    let other = answered(MIN, vec![point(1, 3.0, 50, None)]);

    let history = merge(&request(), vec![store_timeout.clone(), other], Vec::new());

    assert_eq!(service_points(&history), [point(1, 3.0, 50, None)]);
    assert_eq!(history.machines.first(), Some(&store_timeout));
    assert!(history.unanswered.is_empty());
}

#[test]
fn machines_on_different_steps_sum_on_the_widest() {
    let narrow = answered(
        MIN,
        vec![
            point(0, 2.0, 100, Some(1_000)),
            point(1, 4.0, 300, Some(1_000)),
            point(5, 6.0, 500, Some(1_000)),
        ],
    );
    let wide = answered(5 * MIN, vec![point(0, 1.0, 10, Some(500))]);

    let history = merge(&request(), vec![narrow, wide], Vec::new());

    assert_eq!(history.step_nanos, Some(5 * MIN));
    assert_eq!(
        service_points(&history),
        [
            point(0, 4.0, 210, Some(1_500)),
            point(5, 6.0, 500, Some(1_000)),
        ]
    );
}

#[test]
fn the_limit_is_absent_once_one_machine_has_none() {
    let limited = answered(MIN, vec![point(0, 1.0, 10, Some(100))]);
    let unlimited = answered(MIN, vec![point(0, 1.0, 20, None)]);

    let history = merge(&request(), vec![limited, unlimited], Vec::new());

    assert_eq!(service_points(&history), [point(0, 2.0, 30, None)]);
}

#[test]
fn a_rate_no_machine_measured_stays_absent() {
    let mut first = point(0, 1.0, 10, None);
    first.io_r = Some(5.0);
    let second = point(0, 1.0, 10, None);

    let history = merge(
        &request(),
        vec![answered(MIN, vec![first]), answered(MIN, vec![second])],
        Vec::new(),
    );

    let [summed] = service_points(&history) else {
        panic!("one bucket");
    };
    assert_eq!(summed.io_r, Some(5.0));
    assert_eq!(summed.io_w, None);
}

#[test]
fn no_answer_has_no_step() {
    let history = merge(&request(), Vec::new(), Vec::new());

    assert_eq!(history.step_nanos, None);
    assert!(service_points(&history).is_empty());
}
