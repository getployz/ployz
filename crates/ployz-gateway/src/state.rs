use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use corro_api_types::{SqliteValue, Statement, TypedQueryEvent};
use futures_util::StreamExt;
use ployz::adapters::corrosion::client::CorrClient;
use ployz::{CorrTransport, NetworkConfig, corrosion_config};
use ployz_routing::{GatewaySnapshot, RoutingState, project};
use ployz_sdk::model::{
    DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId,
    ServiceHeadRecord, ServiceRevisionRecord, ServiceSlotRecord, SlotId,
};
use ployz_sdk::spec::Namespace;
use thiserror::Error;
use tokio::runtime::Builder;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::SharedSnapshot;

const REFRESH_DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub data_dir: PathBuf,
    pub network: String,
}

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("failed to load network config: {0}")]
    Config(String),
    #[error("failed to reach corrosion API: {0}")]
    Corrosion(String),
    #[error("failed to decode corrosion row: {0}")]
    Decode(String),
    #[error("projection failed: {0}")]
    Projection(String),
    #[error("gateway sync runtime failed: {0}")]
    Runtime(String),
}

pub fn load_initial_snapshot(config: &GatewayConfig) -> Result<GatewaySnapshot, GatewayError> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| GatewayError::Runtime(err.to_string()))?;
    runtime.block_on(async {
        let client = connect_client(config).await?;
        load_projected_snapshot(&client).await
    })
}

pub fn spawn_sync_thread(
    config: GatewayConfig,
    snapshot: SharedSnapshot,
) -> Result<(), GatewayError> {
    std::thread::Builder::new()
        .name("ployz-gateway-sync".into())
        .spawn(move || {
            let runtime = match Builder::new_multi_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(err) => {
                    warn!(?err, "failed to create gateway sync runtime");
                    return;
                }
            };
            runtime.block_on(async move {
                if let Err(err) = run_sync_loop(config, snapshot).await {
                    warn!(?err, "gateway sync loop exited");
                }
            });
        })
        .map_err(|err| GatewayError::Runtime(err.to_string()))?;
    Ok(())
}

async fn run_sync_loop(config: GatewayConfig, snapshot: SharedSnapshot) -> Result<(), GatewayError> {
    let client = connect_client(&config).await?;
    let (refresh_tx, mut refresh_rx) = mpsc::unbounded_channel::<()>();
    for (label, statement) in subscription_statements() {
        tokio::spawn(run_invalidator(
            label,
            client.clone(),
            statement,
            refresh_tx.clone(),
        ));
    }
    drop(refresh_tx);

    while refresh_rx.recv().await.is_some() {
        tokio::time::sleep(REFRESH_DEBOUNCE).await;
        while refresh_rx.try_recv().is_ok() {}
        match load_projected_snapshot(&client).await {
            Ok(next_snapshot) => {
                let http_routes = next_snapshot.http_routes.len();
                let tcp_routes = next_snapshot.tcp_routes.len();
                snapshot.replace(next_snapshot);
                info!(http_routes, tcp_routes, "gateway snapshot refreshed");
            }
            Err(err) => warn!(?err, "failed to refresh gateway snapshot; keeping previous state"),
        }
    }

    Ok(())
}

async fn run_invalidator(
    label: &'static str,
    client: CorrClient,
    statement: Statement,
    refresh_tx: mpsc::UnboundedSender<()>,
) {
    let mut stream = match client.subscribe(&statement, true, None).await {
        Ok(stream) => stream,
        Err(err) => {
            warn!(%label, ?err, "failed to subscribe gateway invalidator");
            return;
        }
    };

    while let Some(event) = stream.next().await {
        match event {
            Ok(TypedQueryEvent::Columns(_) | TypedQueryEvent::EndOfQuery { .. }) => {}
            Ok(TypedQueryEvent::Row(_, _) | TypedQueryEvent::Change(_, _, _, _)) => {
                let _ = refresh_tx.send(());
            }
            Ok(TypedQueryEvent::Error(err)) => {
                warn!(%label, ?err, "corrosion subscription error");
            }
            Err(err) => {
                warn!(%label, ?err, "gateway invalidator stream error");
            }
        }
    }
}

async fn load_projected_snapshot(client: &CorrClient) -> Result<GatewaySnapshot, GatewayError> {
    project(load_routing_state(client).await?).map_err(|err| GatewayError::Projection(err.to_string()))
}

async fn load_routing_state(client: &CorrClient) -> Result<RoutingState, GatewayError> {
    Ok(RoutingState {
        revisions: list_service_revisions(client).await?,
        heads: list_service_heads(client).await?,
        slots: list_service_slots(client).await?,
        instances: list_instance_status(client).await?,
    })
}

async fn list_service_revisions(client: &CorrClient) -> Result<Vec<ServiceRevisionRecord>, GatewayError> {
    let statement = Statement::Simple(
        "SELECT namespace, service, revision_hash, spec_json, created_by, created_at FROM service_revisions ORDER BY namespace, service, revision_hash".to_string(),
    );
    query_rows(client, &statement, "list_service_revisions")
        .await?
        .iter()
        .map(|row| parse_service_revision(row))
        .collect()
}

async fn list_service_heads(client: &CorrClient) -> Result<Vec<ServiceHeadRecord>, GatewayError> {
    let statement = Statement::Simple(
        "SELECT namespace, service, current_revision_hash, updated_by_deploy_id, updated_at FROM service_heads ORDER BY namespace, service".to_string(),
    );
    query_rows(client, &statement, "list_service_heads")
        .await?
        .iter()
        .map(|row| parse_service_head(row))
        .collect()
}

async fn list_service_slots(client: &CorrClient) -> Result<Vec<ServiceSlotRecord>, GatewayError> {
    let statement = Statement::Simple(
        "SELECT namespace, service, slot_id, machine_id, active_instance_id, revision_hash, updated_by_deploy_id, updated_at FROM service_slots ORDER BY namespace, service, slot_id".to_string(),
    );
    query_rows(client, &statement, "list_service_slots")
        .await?
        .iter()
        .map(|row| parse_service_slot(row))
        .collect()
}

async fn list_instance_status(client: &CorrClient) -> Result<Vec<InstanceStatusRecord>, GatewayError> {
    let statement = Statement::Simple(
        "SELECT instance_id, namespace, service, slot_id, machine_id, revision_hash, deploy_id, docker_container_id, overlay_ip, backend_ports_json, phase, ready, drain_state, error, started_at, updated_at FROM instance_status ORDER BY namespace, service, slot_id, instance_id".to_string(),
    );
    query_rows(client, &statement, "list_instance_status")
        .await?
        .iter()
        .map(|row| parse_instance_status(row))
        .collect()
}

fn subscription_statements() -> Vec<(&'static str, Statement)> {
    vec![
        (
            "service_revisions",
            Statement::Simple(
                "SELECT namespace, service, revision_hash, spec_json, created_by, created_at FROM service_revisions ORDER BY namespace, service, revision_hash".to_string(),
            ),
        ),
        (
            "service_heads",
            Statement::Simple(
                "SELECT namespace, service, current_revision_hash, updated_by_deploy_id, updated_at FROM service_heads ORDER BY namespace, service".to_string(),
            ),
        ),
        (
            "service_slots",
            Statement::Simple(
                "SELECT namespace, service, slot_id, machine_id, active_instance_id, revision_hash, updated_by_deploy_id, updated_at FROM service_slots ORDER BY namespace, service, slot_id".to_string(),
            ),
        ),
        (
            "instance_status",
            Statement::Simple(
                "SELECT instance_id, namespace, service, slot_id, machine_id, revision_hash, deploy_id, docker_container_id, overlay_ip, backend_ports_json, phase, ready, drain_state, error, started_at, updated_at FROM instance_status ORDER BY namespace, service, slot_id, instance_id".to_string(),
            ),
        ),
    ]
}

async fn connect_client(config: &GatewayConfig) -> Result<CorrClient, GatewayError> {
    let network_path = NetworkConfig::path(&config.data_dir, &config.network);
    let network_config = NetworkConfig::load(&network_path)
        .map_err(|err| GatewayError::Config(err.to_string()))?;
    let api_addr = SocketAddr::new(
        IpAddr::V6(network_config.overlay_ip.0),
        corrosion_config::DEFAULT_API_PORT,
    );
    let bridge_addr = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        corrosion_config::DEFAULT_API_PORT,
    );

    let bridge = CorrClient::new(api_addr, CorrTransport::Bridge { local_addr: bridge_addr });
    if bridge.health().await.is_ok() {
        info!(%api_addr, %bridge_addr, "gateway using local bridge transport for corrosion");
        return Ok(bridge);
    }

    let direct = CorrClient::new(api_addr, CorrTransport::Direct);
    if direct.health().await.is_ok() {
        info!(%api_addr, "gateway using direct overlay transport for corrosion");
        return Ok(direct);
    }

    Err(GatewayError::Corrosion(format!(
        "failed to reach corrosion via bridge {bridge_addr} or direct {api_addr}"
    )))
}

async fn query_rows(
    client: &CorrClient,
    statement: &Statement,
    op: &'static str,
) -> Result<Vec<Vec<SqliteValue>>, GatewayError> {
    let mut stream = client
        .query(statement, None)
        .await
        .map_err(|err| GatewayError::Corrosion(format!("{op}: {err}")))?;
    let mut rows = Vec::new();
    while let Some(event) = stream.next().await {
        match event.map_err(|err| GatewayError::Corrosion(format!("{op}: {err}")))? {
            TypedQueryEvent::Columns(_) => {}
            TypedQueryEvent::Row(_, cells) => rows.push(cells),
            TypedQueryEvent::EndOfQuery { .. } => break,
            TypedQueryEvent::Error(err) => {
                return Err(GatewayError::Corrosion(format!("{op}: {err}")));
            }
            TypedQueryEvent::Change(_, _, _, _) => {}
        }
    }
    Ok(rows)
}

fn parse_service_revision(row: &[SqliteValue]) -> Result<ServiceRevisionRecord, GatewayError> {
    let [namespace, service, revision_hash, spec_json, created_by, created_at] = row else {
        return Err(GatewayError::Decode(format!(
            "expected 6 revision columns, got {}",
            row.len()
        )));
    };
    Ok(ServiceRevisionRecord {
        namespace: Namespace(text(namespace, "namespace")?),
        service: text(service, "service")?,
        revision_hash: text(revision_hash, "revision_hash")?,
        spec_json: text(spec_json, "spec_json")?,
        created_by: MachineId(text(created_by, "created_by")?),
        created_at: integer(created_at, "created_at")? as u64,
    })
}

fn parse_service_head(row: &[SqliteValue]) -> Result<ServiceHeadRecord, GatewayError> {
    let [namespace, service, revision_hash, deploy_id, updated_at] = row else {
        return Err(GatewayError::Decode(format!(
            "expected 5 head columns, got {}",
            row.len()
        )));
    };
    Ok(ServiceHeadRecord {
        namespace: Namespace(text(namespace, "namespace")?),
        service: text(service, "service")?,
        current_revision_hash: text(revision_hash, "current_revision_hash")?,
        updated_by_deploy_id: DeployId(text(deploy_id, "updated_by_deploy_id")?),
        updated_at: integer(updated_at, "updated_at")? as u64,
    })
}

fn parse_service_slot(row: &[SqliteValue]) -> Result<ServiceSlotRecord, GatewayError> {
    let [namespace, service, slot_id, machine_id, instance_id, revision_hash, deploy_id, updated_at] =
        row
    else {
        return Err(GatewayError::Decode(format!(
            "expected 8 slot columns, got {}",
            row.len()
        )));
    };
    Ok(ServiceSlotRecord {
        namespace: Namespace(text(namespace, "namespace")?),
        service: text(service, "service")?,
        slot_id: SlotId(text(slot_id, "slot_id")?),
        machine_id: MachineId(text(machine_id, "machine_id")?),
        active_instance_id: InstanceId(text(instance_id, "active_instance_id")?),
        revision_hash: text(revision_hash, "revision_hash")?,
        updated_by_deploy_id: DeployId(text(deploy_id, "updated_by_deploy_id")?),
        updated_at: integer(updated_at, "updated_at")? as u64,
    })
}

fn parse_instance_status(row: &[SqliteValue]) -> Result<InstanceStatusRecord, GatewayError> {
    let [
        instance_id,
        namespace,
        service,
        slot_id,
        machine_id,
        revision_hash,
        deploy_id,
        docker_container_id,
        overlay_ip,
        backend_ports,
        phase,
        ready,
        drain_state,
        error,
        started_at,
        updated_at,
    ] = row
    else {
        return Err(GatewayError::Decode(format!(
            "expected 16 instance columns, got {}",
            row.len()
        )));
    };

    let overlay_ip = text(overlay_ip, "overlay_ip")?;
    let overlay_ip = if overlay_ip.is_empty() {
        None
    } else {
        Some(
            overlay_ip
                .parse()
                .map_err(|err| GatewayError::Decode(format!("invalid overlay ip: {err}")))?,
        )
    };
    let backend_ports = serde_json::from_str::<HashMap<String, u16>>(&text(
        backend_ports,
        "backend_ports_json",
    )?)
    .map_err(|err| GatewayError::Decode(format!("invalid backend ports json: {err}")))?;
    let phase: InstancePhase = text(phase, "phase")?
        .parse()
        .map_err(GatewayError::Decode)?;
    let drain_state: DrainState = text(drain_state, "drain_state")?
        .parse()
        .map_err(GatewayError::Decode)?;
    let error = text(error, "error")?;

    Ok(InstanceStatusRecord {
        instance_id: InstanceId(text(instance_id, "instance_id")?),
        namespace: Namespace(text(namespace, "namespace")?),
        service: text(service, "service")?,
        slot_id: SlotId(text(slot_id, "slot_id")?),
        machine_id: MachineId(text(machine_id, "machine_id")?),
        revision_hash: text(revision_hash, "revision_hash")?,
        deploy_id: DeployId(text(deploy_id, "deploy_id")?),
        docker_container_id: text(docker_container_id, "docker_container_id")?,
        overlay_ip,
        backend_ports: backend_ports.into_iter().collect(),
        phase,
        ready: integer(ready, "ready")? != 0,
        drain_state,
        error: if error.is_empty() { None } else { Some(error) },
        started_at: integer(started_at, "started_at")? as u64,
        updated_at: integer(updated_at, "updated_at")? as u64,
    })
}

fn integer(value: &SqliteValue, field: &'static str) -> Result<i64, GatewayError> {
    if let Some(&integer) = value.as_integer() {
        return Ok(integer);
    }
    if let Some(text) = value.as_text() {
        if text.is_empty() {
            return Ok(0);
        }
        return text
            .parse::<i64>()
            .map_err(|err| GatewayError::Decode(format!("invalid integer for '{field}': {err}")));
    }
    Err(GatewayError::Decode(format!(
        "expected integer for '{field}', got {value:?}"
    )))
}

fn text(value: &SqliteValue, field: &'static str) -> Result<String, GatewayError> {
    value
        .as_text()
        .map(ToOwned::to_owned)
        .ok_or_else(|| GatewayError::Decode(format!("expected text for '{field}'")))
}
