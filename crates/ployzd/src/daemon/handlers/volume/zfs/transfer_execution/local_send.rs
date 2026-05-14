use std::time::Duration;

use crate::daemon::handlers::volume::transfer_listener::{ZfsTransferOpen, ZfsTransferReceived};
use ployz_model::{MachineId, MachineMembership, VolumeRecord};
use ployz_volume_zfs::{SendResult, TokioShellRunner, ZfsDriver};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const ACK_READ_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ACK_BYTES: usize = 16 * 1024;

#[allow(clippy::too_many_arguments)]
pub(in crate::daemon::handlers::volume::zfs) async fn send_zfs_stream_from_local(
    record: &VolumeRecord,
    target: &MachineMembership,
    driver: &ZfsDriver<TokioShellRunner>,
    transfer_port: u16,
    local_machine_id: &MachineId,
    snapshot: &str,
    expected_guid: u64,
    from_snapshot: Option<&str>,
    from_snapshot_guid: Option<u64>,
) -> Result<SendResult, String> {
    let dataset = super::super::volume_dataset(
        driver.root_dataset(),
        &record.namespace,
        &record.volume_name,
    );
    let actual_guid = driver
        .snapshot_guid(&dataset, snapshot)
        .await
        .map_err(|err| err.to_string())?;
    if actual_guid != expected_guid {
        return Err(format!(
            "local snapshot '{dataset}@{snapshot}' guid {actual_guid} did not match expected {expected_guid}"
        ));
    }
    if let Some(from_snapshot) = from_snapshot
        && let Some(expected_from) = from_snapshot_guid
    {
        let actual_from = driver
            .snapshot_guid(&dataset, from_snapshot)
            .await
            .map_err(|err| err.to_string())?;
        if actual_from != expected_from {
            return Err(format!(
                "local base snapshot '{dataset}@{from_snapshot}' guid {actual_from} did not match expected {expected_from}"
            ));
        }
    }
    let address =
        std::net::SocketAddr::new(std::net::IpAddr::V6(target.overlay_ip.0), transfer_port);
    let stream = TcpStream::connect(address)
        .await
        .map_err(|err| format!("connect zfs transfer target {address}: {err}"))?;
    let (reader, mut writer) = stream.into_split();
    let open = ZfsTransferOpen {
        namespace: record.namespace.as_str().to_string(),
        volume: record.volume_name.clone(),
        snapshot: snapshot.to_string(),
        expected_guid,
        source_machine_id: Some(local_machine_id.clone()),
        from_snapshot: from_snapshot.map(str::to_string),
        from_snapshot_guid,
    };
    let mut header =
        serde_json::to_string(&open).map_err(|err| format!("encode transfer open: {err}"))?;
    header.push('\n');
    writer
        .write_all(header.as_bytes())
        .await
        .map_err(|err| format!("write transfer open: {err}"))?;

    let mut send = match from_snapshot {
        Some(from_snapshot) => driver
            .spawn_send_incremental(&dataset, from_snapshot, snapshot)
            .map_err(|err| err.to_string())?,
        None => driver
            .spawn_send_full(&dataset, snapshot)
            .map_err(|err| err.to_string())?,
    };
    let Some(mut stdout) = send.stdout.take() else {
        return Err("zfs send stdout was not piped".to_string());
    };
    let copy_result = tokio::io::copy(&mut stdout, &mut writer).await;
    drop(stdout);
    let bytes = match copy_result {
        Ok(bytes) => bytes,
        Err(err) => {
            let copy_error = format!("copy zfs send stream: {err}");
            // Always reap the child even if kill fails, so stream errors do not leave a zombie.
            if let Err(kill_err) = send.kill().await {
                tracing::warn!(error = %kill_err, "failed to kill zfs send after copy error");
            }
            if let Err(wait_err) = send.wait_with_output().await {
                return Err(format!("{copy_error}; failed to reap zfs send: {wait_err}"));
            }
            return Err(copy_error);
        }
    };
    writer
        .shutdown()
        .await
        .map_err(|err| format!("shutdown zfs transfer stream: {err}"))?;
    let output = send
        .wait_with_output()
        .await
        .map_err(|err| format!("wait for zfs send: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "zfs send failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let mut reader = BufReader::new(reader).take((MAX_ACK_BYTES + 1) as u64);
    let mut buf = Vec::new();
    let bytes_read = tokio::time::timeout(ACK_READ_TIMEOUT, reader.read_until(b'\n', &mut buf))
        .await
        .map_err(|_| "timed out waiting for zfs transfer response".to_string())?
        .map_err(|err| format!("read zfs transfer response: {err}"))?;
    if bytes_read == 0 {
        return Err("connection closed before zfs transfer response".to_string());
    }
    if buf.len() > MAX_ACK_BYTES {
        return Err(format!(
            "zfs transfer response exceeded {MAX_ACK_BYTES} bytes"
        ));
    }
    let response: ZfsTransferReceived = serde_json::from_slice(&buf)
        .map_err(|err| format!("decode zfs transfer response: {err}"))?;
    if !response.ok {
        return Err(response.message);
    }
    let Some(snapshot_guid) = response.snapshot_guid else {
        return Err("target did not report snapshot guid".to_string());
    };
    Ok(SendResult {
        bytes_transferred: bytes,
        snapshot_guid,
    })
}
