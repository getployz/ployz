use ployz_api::{DoctorPayload, DoctorPeer};

pub(super) fn render_doctor_report(report: &DoctorPayload) -> String {
    let blocking_peers: Vec<&DoctorPeer> = report.peers.iter().filter(|row| row.blocking).collect();
    let all_peers: Vec<&DoctorPeer> = report.peers.iter().collect();

    let mut lines = Vec::new();
    lines.push(format!("lifecycle: {}", report.overall.lifecycle));
    if !blocking_peers.is_empty() {
        lines.push(String::new());
        lines.push(String::from("blocking peers:"));
        append_peer_section(&mut lines, blocking_peers.as_slice(), false);
    }
    if !all_peers.is_empty() {
        lines.push(String::new());
        lines.push(String::from("all peers:"));
        append_peer_section(&mut lines, all_peers.as_slice(), true);
    }
    lines.push(String::new());
    lines.push(format!(
        "local: machine={} network={} network_lifecycle={} machine_lifecycle={} storage={} storage_participation={} runtime_running={}",
        report.local.machine_id,
        report.local.network,
        report.local.network_lifecycle,
        report.local.machine_lifecycle,
        report.local.storage,
        report.local.storage_participation,
        report.local.runtime_running,
    ));
    if report.local.config_subnet != report.local.record_subnet {
        lines.push(format!(
            "local subnet mismatch: config={:?} record={:?}",
            report.local.config_subnet, report.local.record_subnet
        ));
    }
    if report.local.published_endpoints != report.local.detected_endpoints {
        lines.push(format!(
            "local endpoint drift: published={:?} detected={:?}",
            report.local.published_endpoints, report.local.detected_endpoints
        ));
    }
    if !report.local.endpoint_watch_supported {
        lines.push(String::from(
            "local endpoint watch: unsupported, relying on periodic local endpoint audit",
        ));
    }

    lines.join("\n")
}

fn append_peer_section(lines: &mut Vec<String>, rows: &[&DoctorPeer], include_cause: bool) {
    let w_id = rows
        .iter()
        .map(|row| row.machine_id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    let w_store = rows
        .iter()
        .map(|row| store_status_column(row).len())
        .max()
        .unwrap_or("store=active subnet=none".len())
        .max("store=active subnet=none".len());
    let w_wg = rows
        .iter()
        .map(|row| wg_status_column(row).len())
        .max()
        .unwrap_or("wg=fresh".len())
        .max("wg=fresh".len());
    let w_rtt = rows
        .iter()
        .map(|row| rtt_status_column(row).len())
        .max()
        .unwrap_or("rtt=none".len())
        .max("rtt=none".len());
    for row in rows {
        let base = format!(
            "  {:<w_id$}  {:<w_store$}  {:<w_wg$}  {:<w_rtt$}",
            row.machine_id,
            store_status_column(row),
            wg_status_column(row),
            rtt_status_column(row),
        );
        if include_cause {
            lines.push(base);
        } else {
            lines.push(format!("{base}  cause={}", row.cause_message));
        }
    }
}

fn store_status_column(row: &DoctorPeer) -> String {
    format!(
        "store={} storage={} storage_participation={} subnet={}",
        row.store_lifecycle,
        row.storage,
        row.storage_participation,
        row.subnet.as_deref().unwrap_or("none")
    )
}

fn wg_status_column(row: &DoctorPeer) -> String {
    format!("wg={}", row.wg_state)
}

fn rtt_status_column(row: &DoctorPeer) -> String {
    match (row.rtt_median_ms, row.rtt_stddev_ms) {
        (Some(median), Some(stddev)) => {
            format!(
                "rtt={}±{}",
                format_ms(median),
                format_ms_one_decimal(stddev)
            )
        }
        (Some(median), None) => format!("rtt={}", format_ms(median)),
        (None, Some(_)) | (None, None) => String::from("rtt=none"),
    }
}

fn format_ms(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}ms")
    } else {
        format!("{value:.1}ms")
    }
}

fn format_ms_one_decimal(value: f64) -> String {
    format!("{value:.1}ms")
}
