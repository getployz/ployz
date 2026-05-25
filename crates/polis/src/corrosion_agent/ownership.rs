use std::{
    fs,
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;

use super::config::CorrosionAgentConfig;
use super::process::LocalCorrosionAgent;
use super::{
    CorrosionAgentError, setup_error, setup_message, store_probe_statement, store_probe_timeout,
};
use crate::{CorrosionStore, StoreStatement, StoreTimeout};

#[derive(Debug)]
pub enum CorrosionAdoption {
    Owned(LocalCorrosionAgent),
    NotListening { api_addr: SocketAddr },
    Foreign { source: CorrosionAgentError },
}

pub(crate) async fn adopt_existing(config: CorrosionAgentConfig) -> CorrosionAdoption {
    match api_listener_ready(config.api_addr).await {
        Ok(true) => {}
        Ok(false) => {
            return CorrosionAdoption::NotListening {
                api_addr: config.api_addr,
            };
        }
        Err(source) => return CorrosionAdoption::Foreign { source },
    }

    match LocalCorrosionAgent::connect_existing(config).await {
        Ok(agent) => CorrosionAdoption::Owned(agent),
        Err(source) => CorrosionAdoption::Foreign { source },
    }
}

pub(crate) async fn record_owner_marker(
    config: &CorrosionAgentConfig,
) -> Result<(), CorrosionAgentError> {
    verify_corrosion_database_path(config).await?;
    let marker = toml::to_string(&owner_marker(config))
        .map_err(|error| setup_message(format!("serialize corrosion owner marker: {error}")))?;
    fs::write(owner_marker_path(config), marker).map_err(setup_error)?;
    Ok(())
}

pub(crate) async fn verify_owner_marker(
    config: &CorrosionAgentConfig,
) -> Result<(), CorrosionAgentError> {
    verify_corrosion_database_path(config).await?;
    let marker = fs::read_to_string(owner_marker_path(config)).map_err(|error| {
        CorrosionAgentError::Ownership {
            message: format!("read owner marker: {error}"),
        }
    })?;
    let actual = toml::from_str::<CorrosionOwnerMarker>(&marker).map_err(|error| {
        CorrosionAgentError::Ownership {
            message: format!("parse owner marker: {error}"),
        }
    })?;
    let expected = owner_marker(config);
    if actual != expected {
        return Err(CorrosionAgentError::Ownership {
            message: "owner marker does not match configured corrosion state".to_string(),
        });
    }

    Ok(())
}

pub(crate) async fn wait_store_ready(
    api_addr: SocketAddr,
    readiness_timeout: Duration,
) -> Result<(), CorrosionAgentError> {
    let deadline = Instant::now() + readiness_timeout;
    let probe_timeout = store_probe_timeout()?;
    let probe = store_probe_statement()?;

    loop {
        if Instant::now() >= deadline {
            return Err(CorrosionAgentError::ReadinessTimeout { api_addr });
        }

        if let Ok(store) = CorrosionStore::new(api_addr)
            && store.query(&probe, probe_timeout).await.is_ok()
        {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn api_listener_ready(api_addr: SocketAddr) -> Result<bool, CorrosionAgentError> {
    match tokio::time::timeout(Duration::from_millis(100), TcpStream::connect(api_addr)).await {
        Ok(Ok(_stream)) => Ok(true),
        Ok(Err(error))
            if matches!(
                error.kind(),
                ErrorKind::ConnectionRefused
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::TimedOut
                    | ErrorKind::AddrNotAvailable
                    | ErrorKind::NotConnected
            ) =>
        {
            Ok(false)
        }
        Ok(Err(error)) => Err(setup_error(error)),
        Err(_) => Ok(false),
    }
}

async fn verify_corrosion_database_path(
    config: &CorrosionAgentConfig,
) -> Result<(), CorrosionAgentError> {
    let store = CorrosionStore::new(config.api_addr)
        .map_err(|source| CorrosionAgentError::Store { source })?;
    let query = StoreStatement::new("PRAGMA database_list")
        .map_err(|source| CorrosionAgentError::Store { source })?;
    let rows = store
        .query(&query, StoreTimeout::CONTROL_PLANE_DEFAULT)
        .await
        .map_err(|source| CorrosionAgentError::Store { source })?;
    let expected = config.root_dir.join("state.db");
    let Some(actual) = rows
        .rows()
        .iter()
        .find(|row| row.text("name").ok() == Some("main"))
        .and_then(|row| row.text("file").ok())
    else {
        return Err(CorrosionAgentError::Ownership {
            message: "corrosion database path is missing".to_string(),
        });
    };

    if !paths_match(&expected, Path::new(actual)) {
        return Err(CorrosionAgentError::Ownership {
            message: format!(
                "corrosion database path mismatch: expected {}, got {actual}",
                expected.display()
            ),
        });
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CorrosionOwnerMarker {
    owner_id: String,
    root_dir: String,
    api_addr: String,
    gossip_addr: String,
    prometheus_addr: String,
    db_path: String,
    admin_path: String,
}

fn owner_marker(config: &CorrosionAgentConfig) -> CorrosionOwnerMarker {
    CorrosionOwnerMarker {
        owner_id: "ployzd".to_string(),
        root_dir: config.root_dir.display().to_string(),
        api_addr: config.api_addr.to_string(),
        gossip_addr: config.gossip_addr.to_string(),
        prometheus_addr: config.prometheus_addr.to_string(),
        db_path: config.root_dir.join("state.db").display().to_string(),
        admin_path: config.root_dir.join("admin.sock").display().to_string(),
    }
}

fn owner_marker_path(config: &CorrosionAgentConfig) -> PathBuf {
    config.root_dir.join("polis-owner.toml")
}

fn paths_match(expected: &Path, actual: &Path) -> bool {
    if expected == actual {
        return true;
    }
    match (fs::canonicalize(expected), fs::canonicalize(actual)) {
        (Ok(expected), Ok(actual)) => expected == actual,
        _ => false,
    }
}
