use crate::{CliError, Result};
use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, DoctorPayload, DoctorPeer,
    MachineInviteListPayload, MachineListPayload, MachineOperationInfo,
    MachineOperationListPayload, MachineOperationPayload, MachineRttPayload, MeshListPayload,
    MeshReadyPayload, MeshSelfRecordPayload, MeshStatusPayload, StatusPayload,
};
use ployz_sdk::{Transport, UnixSocketTransport};
use std::io::{BufRead, BufReader, Read, Write};

pub(crate) async fn cmd_rpc_stdio(socket: &str) -> Result<i32> {
    let mut line = String::new();
    BufReader::new(std::io::stdin())
        .read_line(&mut line)
        .map_err(|error| {
            CliError::Usage(format!("failed to read daemon request from stdin: {error}"))
        })?;
    let request = serde_json::from_str::<DaemonRequest>(&line)
        .map_err(|error| CliError::Usage(format!("invalid daemon request from stdin: {error}")))?;

    let transport = UnixSocketTransport::new(socket.to_string());
    let response = request_daemon(&transport, socket, request).await?;
    let body = serde_json::to_string(&response).map_err(|error| {
        CliError::Serialize(format!("failed to encode daemon response: {error}"))
    })?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(body.as_bytes())
        .map_err(|error| CliError::Io(format!("failed to write daemon response: {error}")))?;
    stdout.write_all(b"\n").map_err(|error| {
        CliError::Io(format!("failed to write daemon response newline: {error}"))
    })?;
    stdout
        .flush()
        .map_err(|error| CliError::Io(format!("failed to flush daemon response: {error}")))?;
    Ok(0)
}

pub(crate) async fn request_daemon<T: Transport>(
    transport: &T,
    socket: &str,
    request: DaemonRequest,
) -> Result<DaemonResponse> {
    transport
        .request(request)
        .await
        .map_err(|error| CliError::Transport {
            socket: socket.to_string(),
            message: error.to_string(),
        })
}

pub(crate) fn render_response(
    json: bool,
    plain: bool,
    quiet: bool,
    response: &DaemonResponse,
) -> Result<()> {
    if json {
        let body = serde_json::to_string_pretty(response).map_err(|error| {
            CliError::Serialize(format!("failed to encode JSON output: {error}"))
        })?;
        println!("{body}");
        return Ok(());
    }

    if response.ok {
        if !quiet {
            if plain {
                println!("{}", render_plain_success(response));
            } else {
                println!("{}", response.message);
            }
        }
        return Ok(());
    }

    if plain {
        eprintln!("{}", response.message);
    } else {
        eprintln!("error [{}]: {}", response.code, response.message);
    }
    Ok(())
}

fn render_plain_success(response: &DaemonResponse) -> String {
    match response.payload.as_ref() {
        Some(DaemonPayload::MachineList(payload)) => render_plain_machine_list(payload),
        Some(DaemonPayload::MachineRtt(payload)) => render_plain_machine_rtt(payload),
        Some(DaemonPayload::Doctor(payload)) => render_plain_doctor(payload),
        Some(DaemonPayload::Status(payload)) => render_plain_status(payload),
        Some(DaemonPayload::MeshList(payload)) => render_plain_mesh_list(payload),
        Some(DaemonPayload::MeshStatus(payload)) => render_plain_mesh_status(payload),
        Some(DaemonPayload::MeshReady(payload)) => render_plain_mesh_ready(payload),
        Some(DaemonPayload::MeshSelfRecord(payload)) => render_plain_mesh_self_record(payload),
        Some(DaemonPayload::MachineInviteList(payload)) => render_plain_invite_list(payload),
        Some(DaemonPayload::MachineOperationList(payload)) => {
            render_plain_machine_operation_list(payload)
        }
        Some(DaemonPayload::MachineOperation(payload)) => render_plain_machine_operation(payload),
        _ => response.message.clone(),
    }
}

fn render_plain_status(payload: &StatusPayload) -> String {
    format!(
        "machine={} network={} overlay={} network_lifecycle={} local_machine_lifecycle={} mesh_phase={}",
        payload.machine_id,
        payload.network.as_deref().unwrap_or("none"),
        payload.overlay_ip.as_deref().unwrap_or("—"),
        payload
            .network_lifecycle
            .map(|value| value.to_string())
            .unwrap_or_else(|| "—".into()),
        payload
            .local_machine_lifecycle
            .map(|value| value.to_string())
            .unwrap_or_else(|| "—".into()),
        payload.mesh_phase
    )
}

fn render_plain_machine_list(payload: &MachineListPayload) -> String {
    if payload.rows.is_empty() {
        return String::from("no machines");
    }

    payload
        .rows
        .iter()
        .map(|row| {
            format!(
                "id={} lifecycle={} region={} az={} overlay_ip={} subnet={} created_at={}",
                row.id,
                row.lifecycle,
                row.region,
                row.availability_zone.as_deref().unwrap_or("—"),
                row.overlay_ip,
                row.subnet.as_deref().unwrap_or("—"),
                row.created_at
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_plain_machine_rtt(payload: &MachineRttPayload) -> String {
    let mut lines = Vec::new();
    if payload.rows.is_empty() {
        lines.push(String::from("no rtt samples"));
    } else {
        lines.extend(payload.rows.iter().map(|row| {
            format!(
                "machine={} peer={} median={} stddev=±{}",
                row.machine,
                row.peer,
                format_ms(row.median_ms),
                format_ms_one_decimal(row.stddev_ms),
            )
        }));
    }
    if !payload.warnings.is_empty() {
        lines.extend(
            payload
                .warnings
                .iter()
                .map(|warning| format!("warning={warning}")),
        );
    }
    lines.join("\n")
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

fn render_plain_mesh_list(payload: &MeshListPayload) -> String {
    if payload.networks.is_empty() {
        return String::from("no networks");
    }
    payload
        .networks
        .iter()
        .map(|network| format!("network={} state={}", network.name, network.state))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_plain_mesh_status(payload: &MeshStatusPayload) -> String {
    format!(
        "network={} overlay={} state={}",
        payload.network, payload.overlay_ip, payload.lifecycle
    )
}

fn render_plain_mesh_ready(payload: &MeshReadyPayload) -> String {
    format!(
        "ready={} phase={} store_healthy={} sync_connected={} workload_subnet_present={}",
        payload.ready,
        payload.phase,
        payload.store_healthy,
        payload.sync_connected,
        payload.workload_subnet_present
    )
}

fn render_plain_mesh_self_record(payload: &MeshSelfRecordPayload) -> String {
    payload.encoded.clone()
}

fn render_plain_doctor(payload: &DoctorPayload) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "participation={} local_machine={} network={} local_participation={} local_status={} endpoint_watch_supported={} endpoint_drift={}",
        payload.overall.lifecycle,
        payload.local.machine_id,
        payload.local.network,
        payload.local.machine_lifecycle,
        payload.local.network_lifecycle,
        payload.local.endpoint_watch_supported,
        payload.local.published_endpoints != payload.local.detected_endpoints,
    ));
    lines.extend(payload.peers.iter().map(|peer| {
        format!(
            "peer={} role={} blocking={} store_lifecycle={} subnet={} corrosion_state={} wg_state={} rtt={} cause_code={}",
            peer.machine_id,
            peer.role,
            peer.blocking,
            peer.store_lifecycle,
            peer.subnet.as_deref().unwrap_or("—"),
            peer.corrosion_state,
            peer.wg_state,
            render_plain_peer_rtt(peer),
            peer.cause_code,
        )
    }));
    lines.join("\n")
}

fn render_plain_peer_rtt(peer: &DoctorPeer) -> String {
    match (peer.rtt_median_ms, peer.rtt_stddev_ms) {
        (Some(median), Some(stddev)) => {
            format!("{}±{}", format_ms(median), format_ms_one_decimal(stddev))
        }
        (Some(median), None) => format_ms(median),
        (None, Some(_)) | (None, None) => String::from("none"),
    }
}

fn render_plain_invite_list(payload: &MachineInviteListPayload) -> String {
    if payload.invites.is_empty() {
        return String::from("no invites");
    }
    payload
        .invites
        .iter()
        .map(|invite| {
            format!(
                "invite_id={} status={} expires_at={} consumed_by={}",
                invite.invite_id,
                invite.status,
                invite.expires_at,
                invite.consumed_by.as_deref().unwrap_or("—")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_plain_machine_operation_list(payload: &MachineOperationListPayload) -> String {
    if payload.operations.is_empty() {
        return String::from("no machine operations");
    }
    payload
        .operations
        .iter()
        .map(render_plain_machine_operation_info)
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_plain_machine_operation(payload: &MachineOperationPayload) -> String {
    render_plain_machine_operation_info(&payload.operation)
}

fn render_plain_machine_operation_info(operation: &MachineOperationInfo) -> String {
    format!(
        "id={} kind={} status={} stage={} network={} targets={} machine_id={} invite_id={} allocated_subnet={} last_error={}",
        operation.id,
        operation.kind,
        operation.status,
        operation.stage,
        operation.network_name.as_deref().unwrap_or("—"),
        if operation.targets.is_empty() {
            String::from("—")
        } else {
            operation.targets.join(",")
        },
        operation
            .machine_id
            .as_ref()
            .map_or("—", |machine_id| machine_id.0.as_str()),
        operation.invite_id.as_deref().unwrap_or("—"),
        operation.allocated_subnet.as_deref().unwrap_or("—"),
        operation.last_error.as_deref().unwrap_or("—")
    )
}

pub(crate) fn read_stdin_string(label: &str) -> Result<String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|error| CliError::Usage(format!("failed to read {label} from stdin: {error}")))?;

    String::from_utf8(bytes).map_err(|error| {
        CliError::Usage(format!("{label} from stdin was not valid utf-8: {error}"))
    })
}

pub(crate) fn read_text_source(label: &str, path: &str) -> Result<String> {
    match path {
        "-" => read_stdin_string(label),
        other => std::fs::read_to_string(other)
            .map_err(|error| CliError::Io(format!("failed to read {label} from {other}: {error}"))),
    }
}

pub(crate) fn read_optional_text_file(
    label: &str,
    path: Option<&std::path::Path>,
) -> Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let contents = std::fs::read_to_string(path).map_err(|error| {
        CliError::Io(format!(
            "failed to read {label} '{}': {error}",
            path.display()
        ))
    })?;
    if contents.trim().is_empty() {
        return Err(CliError::Usage(format!(
            "{label} '{}' is empty",
            path.display()
        )));
    }
    Ok(Some(contents))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_api::{
        DoctorLocal, DoctorOverall, DoctorPayload, DoctorPeer, MachineListPayload, MachineListRow,
        MachineRttPayload, MachineRttRow, MeshListEntry, MeshListPayload, StatusPayload,
    };

    #[test]
    fn plain_machine_list_renders_stable_lines() {
        let response = DaemonResponse {
            ok: true,
            code: String::from("OK"),
            message: String::from("table"),
            payload: Some(DaemonPayload::MachineList(MachineListPayload {
                rows: vec![MachineListRow {
                    id: String::from("peer"),
                    lifecycle: String::from("standby"),
                    region: String::from("us-east-1"),
                    availability_zone: None,
                    overlay_ip: String::from("fd00::2"),
                    subnet: None,
                    created_at: 123,
                }],
            })),
        };

        assert_eq!(
            render_plain_success(&response),
            "id=peer lifecycle=standby region=us-east-1 az=— overlay_ip=fd00::2 subnet=— created_at=123"
        );
    }

    #[test]
    fn plain_machine_rtt_renders_stable_lines() {
        let response = DaemonResponse {
            ok: true,
            code: String::from("OK"),
            message: String::from("table"),
            payload: Some(DaemonPayload::MachineRtt(MachineRttPayload {
                rows: vec![MachineRttRow {
                    machine: String::from("machine-1"),
                    peer: String::from("machine-2"),
                    median_ms: 140.0,
                    stddev_ms: 19.4,
                }],
                warnings: vec![String::from("machine 'machine-3' RTT snapshot unreachable")],
            })),
        };

        assert_eq!(
            render_plain_success(&response),
            "machine=machine-1 peer=machine-2 median=140ms stddev=±19.4ms\nwarning=machine 'machine-3' RTT snapshot unreachable"
        );
    }

    #[test]
    fn plain_doctor_renders_summary_and_peer_lines() {
        let response = DaemonResponse {
            ok: true,
            code: String::from("OK"),
            message: String::from("doctor"),
            payload: Some(DaemonPayload::Doctor(DoctorPayload {
                overall: DoctorOverall {
                    lifecycle: String::from("healthy"),
                },
                local: DoctorLocal {
                    machine_id: String::from("founder"),
                    network: String::from("alpha"),
                    network_lifecycle: String::from("running"),
                    machine_lifecycle: String::from("active"),
                    config_subnet: Some(String::from("10.210.0.0/24")),
                    record_subnet: Some(String::from("10.210.0.0/24")),
                    runtime_running: true,
                    published_endpoints: vec![String::from("10.0.0.1:51820")],
                    detected_endpoints: vec![String::from("10.0.0.1:51820")],
                    endpoint_watch_supported: true,
                },
                peers: vec![DoctorPeer {
                    machine_id: String::from("peer"),
                    role: String::from("blocking"),
                    blocking: false,
                    store_lifecycle: String::from("active"),
                    subnet: Some(String::from("10.210.1.0/24")),
                    wg_state: String::from("fresh"),
                    probe_state: String::from("not-used"),
                    corrosion_state: String::from("alive"),
                    corrosion_actor_id: Some(String::from("actor-1")),
                    corrosion_timestamp: Some(123),
                    rtt_median_ms: Some(40.0),
                    rtt_stddev_ms: Some(2.0),
                    cause_code: String::from("corrosion-alive"),
                    cause_message: String::from("corrosion reports peer alive"),
                }],
            })),
        };

        let rendered = render_plain_success(&response);
        assert!(rendered.contains("participation=healthy"));
        assert!(rendered.contains("local_machine=founder"));
        assert!(rendered.contains("peer=peer"));
        assert!(rendered.contains("store_lifecycle=active"));
        assert!(rendered.contains("corrosion_state=alive"));
        assert!(rendered.contains("rtt=40ms±2.0ms"));
        assert!(rendered.contains("cause_code=corrosion-alive"));
        assert!(!rendered.contains("probe_state="));
    }

    #[test]
    fn plain_status_renders_single_line_summary() {
        let response = DaemonResponse {
            ok: true,
            code: String::from("OK"),
            message: String::from("status"),
            payload: Some(DaemonPayload::Status(StatusPayload {
                machine_id: String::from("founder"),
                network: Some(String::from("alpha")),
                overlay_ip: Some(String::from("fd00::1")),
                network_lifecycle: Some(ployz_types::model::NetworkLifecycle::Running),
                local_machine_lifecycle: Some(ployz_types::model::MachineLifecycle::Active),
                mesh_phase: String::from("Running"),
            })),
        };

        assert_eq!(
            render_plain_success(&response),
            "machine=founder network=alpha overlay=fd00::1 network_lifecycle=running local_machine_lifecycle=active mesh_phase=Running"
        );
    }

    #[test]
    fn plain_mesh_list_renders_compact_network_lines() {
        let response = DaemonResponse {
            ok: true,
            code: String::from("OK"),
            message: String::from("mesh list"),
            payload: Some(DaemonPayload::MeshList(MeshListPayload {
                networks: vec![MeshListEntry {
                    name: String::from("alpha"),
                    state: String::from("running"),
                }],
            })),
        };

        assert_eq!(
            render_plain_success(&response),
            "network=alpha state=running"
        );
    }
}
