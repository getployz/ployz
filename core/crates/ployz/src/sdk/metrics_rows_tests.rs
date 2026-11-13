use std::collections::BTreeMap;

use ployz_core::{
    MachineId, MachineSuccess,
    metrics::{HostPoint, MetricHistoryReply, MetricPoints, ServicePoint},
};

use super::behind_notes;
use crate::sdk::MetricHistory;

const STEP: i64 = 10_000_000_000;
const UNTIL: i64 = 1_760_000_000_000_000_000;

fn machine() -> MachineId {
    MachineId::parse("a".repeat(32)).unwrap()
}

fn service_point(ago: i64) -> ServicePoint {
    ServicePoint {
        ts: UNTIL - ago * STEP,
        cpu_usec: Some(20_000.0),
        mem_bytes: Some(80_000_000),
        mem_limit: Some(512_000_000),
        rx: Some(1_200.0),
        tx: Some(300.0),
        io_r: Some(0.0),
        io_w: Some(40_000.0),
    }
}

fn answered(machine_id: MachineId, points: MetricPoints) -> MachineSuccess<MetricHistoryReply> {
    MachineSuccess {
        machine_id,
        value: MetricHistoryReply {
            points,
            step_nanos: STEP,
            gaps: Vec::new(),
            markers: Vec::new(),
        },
    }
}

#[test]
fn a_server_that_answers_with_only_old_readings_is_named_with_their_age() {
    let point = |ago: i64| HostPoint {
        ts: UNTIL - ago * STEP,
        cpu_busy: Some(0.5),
        memory: None,
        interfaces: BTreeMap::new(),
        disk: None,
    };
    let current = MachineId::parse("d".repeat(32)).unwrap();
    let silent = MachineId::parse("e".repeat(32)).unwrap();
    let history = MetricHistory {
        step_nanos: Some(STEP),
        points: MetricPoints::Machine(vec![point(1)]),
        machines: vec![
            answered(
                machine(),
                MetricPoints::Machine(vec![point(400), point(360)]),
            ),
            answered(current, MetricPoints::Machine(vec![point(2), point(1)])),
            answered(silent, MetricPoints::Machine(Vec::new())),
        ],
        unanswered: Vec::new(),
    };
    let names = BTreeMap::from([
        (machine(), "machine-3"),
        (current, "machine-1"),
        (silent, "machine-2"),
    ]);
    assert_eq!(
        behind_notes(&history, UNTIL, |id| names.get(&id).unwrap().to_string()),
        [
            "machine-3 answered, but its newest reading is about 59m old. \
             Its clock may be behind, or it stopped taking readings."
        ]
    );
}

// A Server a Service moved off answers with the readings from when it ran a
// replica, so its old newest reading says nothing about its clock.
#[test]
fn a_service_whose_replica_left_a_server_names_no_server() {
    let point = service_point;
    let current = MachineId::parse("d".repeat(32)).unwrap();
    let history = MetricHistory {
        step_nanos: Some(STEP),
        points: MetricPoints::Service(vec![point(360), point(1)]),
        machines: vec![
            answered(
                machine(),
                MetricPoints::Service(vec![point(400), point(360)]),
            ),
            answered(current, MetricPoints::Service(vec![point(2), point(1)])),
        ],
        unanswered: Vec::new(),
    };
    assert!(behind_notes(&history, UNTIL, |id| id.to_string()).is_empty());
}
