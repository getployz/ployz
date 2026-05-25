use std::{
    fs,
    io::ErrorKind,
    net::SocketAddr,
    path::Path,
    time::{Duration, Instant},
};

use tokio::net::TcpStream;

use super::config::{CorrosionAgentConfig, CorrosionOwnerFile, owner_file_path, paths_match};
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
    let marker = toml::to_string(&CorrosionOwnerFile::from_config(config))
        .map_err(|error| setup_message(format!("serialize corrosion owner marker: {error}")))?;
    fs::write(owner_file_path(config.root_dir()), marker).map_err(setup_error)?;
    Ok(())
}

pub(crate) async fn verify_owner_marker(
    config: &CorrosionAgentConfig,
) -> Result<(), CorrosionAgentError> {
    verify_corrosion_database_path(config).await?;
    let marker = fs::read_to_string(owner_file_path(config.root_dir())).map_err(|error| {
        CorrosionAgentError::Ownership {
            message: format!("read owner marker: {error}"),
        }
    })?;
    let actual = toml::from_str::<CorrosionOwnerFile>(&marker).map_err(|error| {
        CorrosionAgentError::Ownership {
            message: format!("parse owner marker: {error}"),
        }
    })?;
    if !actual.matches_config(config) {
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
