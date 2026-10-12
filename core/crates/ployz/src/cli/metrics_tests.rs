use std::collections::BTreeMap;

use ployz_core::{
    ContainerId, MachineId, MachineSuccess, QualifiedService, ServiceId,
    metrics::{
        DiskBreakdown, HostMemory, HostPoint, InterfaceRate, MarkerKind, MetricGap,
        MetricGapReason, MetricHistoryReply, MetricMarker, MetricPoints, ServicePoint,
    },
};

use super::{Window, report};
use crate::sdk::MetricHistory;

const STEP: i64 = 10_000_000_000;
const UNTIL: i64 = 1_760_000_000_000_000_000;

fn machine() -> MachineId {
    MachineId::parse("a".repeat(32)).unwrap()
}

fn history(
    points: MetricPoints,
    gaps: Vec<MetricGap>,
    markers: Vec<MetricMarker>,
) -> MetricHistory {
    MetricHistory {
        step_nanos: Some(STEP),
        points: points.clone(),
        machines: vec![MachineSuccess {
            machine_id: machine(),
            value: MetricHistoryReply {
                points,
                step_nanos: STEP,
                gaps,
                markers,
            },
        }],
        unanswered: Vec::new(),
    }
}

fn marker(kind: MarkerKind) -> MetricMarker {
    MetricMarker {
        machine_id: machine(),
        container_id: ContainerId::parse("b".repeat(64)).unwrap(),
        service_id: ServiceId::parse("c".repeat(32)).unwrap(),
        service: QualifiedService::parse("prod/web").unwrap(),
        deployment_id: None,
        replica: "web-1".into(),
        ts: UNTIL - STEP,
        kind,
    }
}

/// The human output as a terminal shows it: heading and table on stdout,
/// then the notes.
fn shown(history: &MetricHistory, subject: &str) -> String {
    let report = report(
        history,
        subject,
        Window {
            since: "1h",
            until_nanos: UNTIL,
        },
    );
    let mut out = format!("{}\n", report.heading).into_bytes();
    report.table.write(&mut out, true).unwrap();
    let mut out = String::from_utf8(out)
        .unwrap()
        .replace("\u{1b}[1m", "")
        .replace("\u{1b}[0m", "");
    for note in report.notes {
        out.push_str(&note);
        out.push('\n');
    }
    out
}

fn service_point(ago: i64, cpu: f64, mem: u64) -> ServicePoint {
    ServicePoint {
        ts: UNTIL - ago * STEP,
        cpu_usec: Some(cpu),
        mem_bytes: Some(mem),
        mem_limit: Some(512_000_000),
        rx: Some(1_200.0),
        tx: Some(300.0),
        io_r: Some(0.0),
        io_w: Some(40_000.0),
    }
}

#[test]
fn a_service_shows_its_newest_values_and_the_range() {
    let history = history(
        MetricPoints::Service(vec![
            service_point(3, 20_000.0, 80_000_000),
            service_point(1, 60_000.0, 96_000_000),
        ]),
        Vec::new(),
        vec![marker(MarkerKind::Oom), marker(MarkerKind::Restart)],
    );
    assert_eq!(
        shown(&history, "web"),
        "web · last 1h\n\
         METRIC       NOW              AVG         PEAK\n\
         CPU          0.06 vCPU        0.04 vCPU   0.06 vCPU\n\
         Memory       96 MB / 512 MB   88 MB       96 MB\n\
         Net in       1.2 KB/s         1.2 KB/s    1.2 KB/s\n\
         Net out      300 B/s          300 B/s     300 B/s\n\
         Disk read    0 B/s            0 B/s       0 B/s\n\
         Disk write   40 KB/s          40 KB/s     40 KB/s\n\
         1 OOM kill, 1 restart in the range.\n"
    );
}

#[test]
fn a_server_shows_host_totals_its_own_interfaces_and_its_disk() {
    let point = |ago: i64, busy: f64| HostPoint {
        ts: UNTIL - ago * STEP,
        cpu_busy: Some(busy),
        memory: Some(HostMemory {
            total: 4_000_000_000,
            available: 2_500_000_000,
        }),
        interfaces: BTreeMap::from([
            (
                "eth0".into(),
                InterfaceRate {
                    rx: 5_000.0,
                    tx: 2_000.0,
                },
            ),
            ("lo".into(), InterfaceRate { rx: 9.0, tx: 9.0 }),
            ("veth1a2b".into(), InterfaceRate { rx: 9.0, tx: 9.0 }),
        ]),
        disk: Some(DiskBreakdown {
            total: 40_000_000_000,
            free: 30_000_000_000,
            volumes: Some(4_000_000_000),
            mirror_slots: Some(0),
            observe_store: Some(500_000_000),
            other: Some(5_500_000_000),
        }),
    };
    let history = history(
        MetricPoints::Machine(vec![point(2, 0.1), point(1, 0.3)]),
        Vec::new(),
        vec![marker(MarkerKind::Exit { exit_code: Some(1) })],
    );
    assert_eq!(
        shown(&history, "web-1"),
        "web-1 · last 1h\n\
         METRIC             NOW               AVG        PEAK\n\
         CPU                30%               20%        30%\n\
         Memory             1.5 GB / 4.0 GB   1.5 GB     1.5 GB\n\
         Net eth0 in        5.0 KB/s          5.0 KB/s   5.0 KB/s\n\
         Net eth0 out       2.0 KB/s          2.0 KB/s   2.0 KB/s\n\
         Disk               10 GB / 40 GB     10 GB      10 GB\n\
         Volumes            4.0 GB            4.0 GB     4.0 GB\n\
         Mirrors            0 B               0 B        0 B\n\
         Logs and metrics   500 MB            500 MB     500 MB\n\
         Other              5.5 GB            5.5 GB     5.5 GB\n\
         prod/web: 1 exit in the range.\n"
    );
}

#[test]
fn a_missing_reading_prints_as_not_measured_never_as_zero() {
    let point = |ago: i64| HostPoint {
        ts: UNTIL - ago * STEP,
        cpu_busy: Some(0.5),
        memory: None,
        interfaces: BTreeMap::new(),
        disk: Some(DiskBreakdown {
            total: 40_000_000_000,
            free: 30_000_000_000,
            volumes: None,
            mirror_slots: None,
            observe_store: Some(500_000_000),
            other: None,
        }),
    };
    // The newest reading is 30 minutes old, so nothing is current.
    let history = history(
        MetricPoints::Machine(vec![point(180)]),
        vec![MetricGap {
            from: UNTIL - 180 * STEP,
            to: UNTIL,
            reason: MetricGapReason::NotSampled,
        }],
        Vec::new(),
    );
    assert_eq!(
        shown(&history, "web-1"),
        "web-1 · last 1h\n\
         METRIC             NOW            AVG      PEAK\n\
         CPU                -              50%      50%\n\
         Memory             not measured   -        -\n\
         Disk               -              10 GB    10 GB\n\
         Volumes            not measured   -        -\n\
         Mirrors            not measured   -        -\n\
         Logs and metrics   -              500 MB   500 MB\n\
         Other              not measured   -        -\n\
         No readings for 30m of the range.\n"
    );

    let service = report(
        &history_with_service_gap(),
        "web",
        Window {
            since: "1h",
            until_nanos: UNTIL,
        },
    );
    let mut out = Vec::new();
    service.table.write(&mut out, false).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "METRIC\tNOW\tAVG\tPEAK\n\
         CPU\tnot measured\t-\t-\n\
         Memory\t50 MB\t50 MB\t50 MB\n\
         Net in\tnot measured\t-\t-\n\
         Net out\tnot measured\t-\t-\n\
         Disk read\tnot measured\t-\t-\n\
         Disk write\tnot measured\t-\t-\n"
    );
}

fn history_with_service_gap() -> MetricHistory {
    history(
        MetricPoints::Service(vec![ServicePoint {
            ts: UNTIL - STEP,
            cpu_usec: None,
            mem_bytes: Some(50_000_000),
            mem_limit: None,
            rx: None,
            tx: None,
            io_r: None,
            io_w: None,
        }]),
        Vec::new(),
        Vec::new(),
    )
}

#[test]
fn a_range_with_no_readings_says_so_instead_of_a_table() {
    let report = report(
        &history(MetricPoints::Service(Vec::new()), Vec::new(), Vec::new()),
        "wbe",
        Window {
            since: "1h",
            until_nanos: UNTIL,
        },
    );
    assert!(report.table.is_empty());
    assert_eq!(
        report.table.empty_sentence(),
        "No readings for wbe in the last 1h."
    );
    assert!(report.notes.is_empty());
}
