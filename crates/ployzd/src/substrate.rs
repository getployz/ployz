use std::fs;
use std::time::Duration;

use crate::config::{CorrosionStartMode, DaemonConfig};
use crate::daemon::DaemonError;
use crate::report::{CorrosionFailure, PeerFailure, SchemaFailure, StartupReport};

pub(crate) struct DaemonSubstrate {
    corrosion: polis::LocalCorrosionAgent,
    store: polis::CorrosionStore,
    peer: polis::PeerRuntime,
    report: StartupReport,
    peer_shutdown_timeout: Duration,
    corrosion_shutdown_timeout: Duration,
}

impl DaemonSubstrate {
    pub async fn start(config: DaemonConfig) -> Result<Self, DaemonError> {
        let configured_report = StartupReport::configured(&config);
        fs::create_dir_all(config.state_dir())
            .map_err(|error| setup_error(error, &configured_report))?;
        let config = match config.with_persisted_corrosion_addresses() {
            Ok(config) => config,
            Err(error) => {
                let failure_report =
                    configured_report.with_corrosion_failed(CorrosionFailure::Agent);
                return Err(DaemonError::corrosion(error, failure_report));
            }
        };
        let mut report = StartupReport::configured(&config);

        let peer_shutdown_timeout = config.peer_shutdown_timeout();
        let corrosion_shutdown_timeout = config.corrosion_shutdown_timeout();
        let corrosion = match start_corrosion(&config).await {
            Ok(corrosion) => {
                report.mark_corrosion_ready(&corrosion);
                corrosion
            }
            Err(error) => {
                let failure_report = report.with_corrosion_failed(CorrosionFailure::Agent);
                return Err(DaemonError::corrosion(error, failure_report));
            }
        };
        let store = match corrosion.store() {
            Ok(store) => store,
            Err(error) => {
                let failure_report = report.with_corrosion_failed(CorrosionFailure::Store);
                let error = DaemonError::corrosion(error, failure_report);
                return Err(rollback_corrosion(corrosion, corrosion_shutdown_timeout, error).await);
            }
        };

        let schema = match substrate_schema_statements() {
            Ok(schema) => schema,
            Err(source) => {
                let failure_report = report.with_schema_failed(SchemaFailure::Verify);
                let error = DaemonError::store(source, failure_report);
                return Err(rollback_corrosion(corrosion, corrosion_shutdown_timeout, error).await);
            }
        };
        if let Err(source) = store
            .execute_transaction(&schema, config.schema_timeout())
            .await
        {
            let failure_report = report.with_schema_failed(SchemaFailure::Verify);
            let error = DaemonError::store(source, failure_report);
            return Err(rollback_corrosion(corrosion, corrosion_shutdown_timeout, error).await);
        }
        if let Err(source) =
            polis::verify_membership_replication_schema(&store, config.schema_timeout()).await
        {
            let failure_report = report.with_schema_failed(SchemaFailure::Verify);
            let error = DaemonError::store(source, failure_report);
            return Err(rollback_corrosion(corrosion, corrosion_shutdown_timeout, error).await);
        }
        if let Err(source) =
            ployz::composition::verify_product_schema(&store, config.schema_timeout()).await
        {
            let failure_report = report.with_schema_failed(SchemaFailure::Verify);
            let error = DaemonError::store(map_product_schema_error(source), failure_report);
            return Err(rollback_corrosion(corrosion, corrosion_shutdown_timeout, error).await);
        }
        report.mark_schema_ready();

        let peer = match polis::PeerRuntime::start(
            config.peer_identity_path(),
            polis::PeerProbeDeadline::new(config.peer_boot_timeout()),
        )
        .await
        {
            Ok(peer) => peer,
            Err(source) => {
                let failure_report = report.with_peer_failed(PeerFailure::Startup);
                let error = DaemonError::peer(source, failure_report);
                return Err(rollback_corrosion(corrosion, corrosion_shutdown_timeout, error).await);
            }
        };
        report.mark_peer_ready(peer.endpoint_id());

        Ok(Self {
            corrosion,
            store,
            peer,
            report,
            peer_shutdown_timeout,
            corrosion_shutdown_timeout,
        })
    }

    #[must_use]
    pub fn startup_report(&self) -> &StartupReport {
        &self.report
    }

    #[must_use]
    pub fn endpoint_id(&self) -> polis::IrohEndpointId {
        self.peer.endpoint_id()
    }

    #[must_use]
    pub(crate) fn store(&self) -> &polis::CorrosionStore {
        &self.store
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn corrosion_process_id(&self) -> Option<u32> {
        self.corrosion.process_id()
    }

    pub async fn shutdown(self) -> Result<StartupReport, DaemonError> {
        let Self {
            corrosion,
            store: _store,
            peer,
            report,
            peer_shutdown_timeout,
            corrosion_shutdown_timeout,
        } = self;
        let peer_result = shutdown_peer(peer, peer_shutdown_timeout).await;
        let corrosion_result = corrosion.shutdown(corrosion_shutdown_timeout).await;

        match (&peer_result, &corrosion_result) {
            (Ok(()), Ok(corrosion)) if corrosion.is_expected_shutdown() => {
                Ok(shutdown_report(&report, peer_result.as_ref(), corrosion))
            }
            _ => {
                let report = failed_shutdown_report(&report, &peer_result, &corrosion_result);
                Err(DaemonError::shutdown(
                    peer_result.err(),
                    corrosion_shutdown_error(corrosion_result),
                    report,
                ))
            }
        }
    }
}

async fn start_corrosion(
    config: &DaemonConfig,
) -> Result<polis::LocalCorrosionAgent, polis::CorrosionAgentError> {
    let corrosion_config = config.corrosion_config().clone();
    match config.corrosion_start_mode() {
        #[cfg(test)]
        CorrosionStartMode::AdoptExisting => match adopt_existing_corrosion(corrosion_config).await
        {
            polis::CorrosionAdoption::Owned(agent) => return Ok(agent),
            polis::CorrosionAdoption::NotListening { api_addr } => {
                return Err(polis::CorrosionAgentError::ReadinessTimeout { api_addr });
            }
            polis::CorrosionAdoption::Foreign { source } => return Err(source),
        },
        CorrosionStartMode::StartOrAdopt => {
            match adopt_existing_corrosion(corrosion_config.clone()).await {
                polis::CorrosionAdoption::Owned(agent) => return Ok(agent),
                polis::CorrosionAdoption::NotListening { .. } => {}
                polis::CorrosionAdoption::Foreign { source } => return Err(source),
            }
        }
        #[cfg(test)]
        CorrosionStartMode::StartManaged => {}
    }

    match polis::LocalCorrosionAgent::start(corrosion_config.clone()).await {
        Ok(agent) => Ok(agent),
        Err(error)
            if config.corrosion_start_mode() == CorrosionStartMode::StartOrAdopt
                && error.is_retryable_startup_failure() =>
        {
            match adopt_existing_corrosion(corrosion_config).await {
                polis::CorrosionAdoption::Owned(agent) => Ok(agent),
                polis::CorrosionAdoption::NotListening { .. } => Err(error),
                polis::CorrosionAdoption::Foreign { source } => Err(source),
            }
        }
        Err(error) => Err(error),
    }
}

async fn adopt_existing_corrosion(config: polis::CorrosionAgentConfig) -> polis::CorrosionAdoption {
    polis::LocalCorrosionAgent::adopt_existing(config).await
}

fn shutdown_report(
    report: &StartupReport,
    peer: Result<&(), &polis::PeerError>,
    corrosion: &polis::CorrosionShutdown,
) -> StartupReport {
    let report = report.clone().with_corrosion_shutdown(corrosion);
    match peer {
        Ok(()) => report.with_peer_stopped(),
        Err(_) => report.with_peer_failed(PeerFailure::Shutdown),
    }
}

fn failed_shutdown_report(
    report: &StartupReport,
    peer: &Result<(), polis::PeerError>,
    corrosion: &Result<polis::CorrosionShutdown, polis::CorrosionAgentError>,
) -> StartupReport {
    let report = match corrosion {
        Ok(corrosion) => shutdown_report(report, peer.as_ref(), corrosion),
        Err(_) => report.clone().with_corrosion_cleanup_failed(),
    };
    match peer {
        Ok(()) => report.with_peer_stopped(),
        Err(_) => report.with_peer_failed(PeerFailure::Shutdown),
    }
}

fn corrosion_shutdown_error(
    corrosion: Result<polis::CorrosionShutdown, polis::CorrosionAgentError>,
) -> Option<polis::CorrosionAgentError> {
    match corrosion {
        Ok(polis::CorrosionShutdown::UnexpectedExit(exit)) => {
            Some(polis::CorrosionAgentError::UnexpectedExit { exit })
        }
        Ok(
            polis::CorrosionShutdown::Graceful
            | polis::CorrosionShutdown::Forced
            | polis::CorrosionShutdown::Unmanaged,
        ) => None,
        Err(error) => Some(error),
    }
}

fn substrate_schema_statements() -> Result<Vec<polis::StoreStatement>, polis::StoreError> {
    ployz::composition::product_schema_statements().map_err(map_product_schema_error)
}

fn map_product_schema_error(error: ployz::PrimitiveFailure) -> polis::StoreError {
    match error {
        ployz::PrimitiveFailure::MalformedPayload => polis::StoreError::MalformedPayload,
        ployz::PrimitiveFailure::Timeout => polis::StoreError::Timeout,
        ployz::PrimitiveFailure::Unauthorized
        | ployz::PrimitiveFailure::Conflict
        | ployz::PrimitiveFailure::StaleFence
        | ployz::PrimitiveFailure::NoResponder
        | ployz::PrimitiveFailure::FreshnessUnknown
        | ployz::PrimitiveFailure::OperationStateConflict
        | ployz::PrimitiveFailure::OperationAlreadySucceeded
        | ployz::PrimitiveFailure::OperationInProgress
        | ployz::PrimitiveFailure::OperationAlreadyFailed
        | ployz::PrimitiveFailure::OperationInterrupted => polis::StoreError::Client {
            message: format!("product schema construction failed: {error}"),
        },
    }
}

async fn rollback_corrosion(
    corrosion: polis::LocalCorrosionAgent,
    timeout: Duration,
    source: DaemonError,
) -> DaemonError {
    match corrosion.shutdown(timeout).await {
        Ok(shutdown) => source.with_corrosion_shutdown_report(shutdown),
        Err(corrosion) => DaemonError::startup_rollback(source, corrosion),
    }
}

async fn shutdown_peer(
    peer: polis::PeerRuntime,
    timeout: Duration,
) -> Result<(), polis::PeerError> {
    peer.shutdown(polis::PeerProbeDeadline::new(timeout)).await
}

fn setup_error(error: std::io::Error, report: &StartupReport) -> DaemonError {
    DaemonError::setup(error.to_string(), report.clone())
}
