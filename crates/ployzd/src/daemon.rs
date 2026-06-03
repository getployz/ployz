use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;

use crate::commands::DaemonCommandService;
use crate::config::DaemonConfig;
use crate::operations::{OperationLeasePolicy, OperationRegistry};
use crate::report::StartupReport;
use crate::substrate::DaemonSubstrate;
use ployz::operation::{OperationLedgerPort, OperationOwner};

#[derive(Debug, Error)]
#[error("{kind}")]
pub struct DaemonError {
    kind: DaemonErrorKind,
    report: StartupReport,
}

#[derive(Debug, Error)]
pub enum DaemonErrorKind {
    #[error("daemon setup failed: {message}")]
    Setup { message: String },

    #[error("corrosion agent failed: {source}")]
    Corrosion { source: polis::CorrosionAgentError },

    #[error("corrosion store failed: {source}")]
    Store { source: polis::StoreError },

    #[error("peer runtime failed: {source}")]
    Peer { source: polis::PeerError },

    #[error("daemon shutdown failed: peer={peer:?}, corrosion={corrosion:?}")]
    Shutdown {
        peer: Option<polis::PeerError>,
        corrosion: Option<polis::CorrosionAgentError>,
    },

    #[error(
        "daemon startup failed and corrosion rollback failed: startup={source}; corrosion={corrosion}"
    )]
    StartupRollback {
        source: Box<DaemonErrorKind>,
        corrosion: polis::CorrosionAgentError,
    },
}

impl DaemonError {
    pub(crate) fn corrosion(source: polis::CorrosionAgentError, report: StartupReport) -> Self {
        Self {
            kind: DaemonErrorKind::Corrosion { source },
            report,
        }
    }

    pub(crate) fn store(source: polis::StoreError, report: StartupReport) -> Self {
        Self {
            kind: DaemonErrorKind::Store { source },
            report,
        }
    }

    pub(crate) fn peer(source: polis::PeerError, report: StartupReport) -> Self {
        Self {
            kind: DaemonErrorKind::Peer { source },
            report,
        }
    }

    pub(crate) fn setup(message: String, report: StartupReport) -> Self {
        Self {
            kind: DaemonErrorKind::Setup { message },
            report,
        }
    }

    pub(crate) fn startup_rollback(
        source: DaemonError,
        corrosion: polis::CorrosionAgentError,
    ) -> Self {
        let DaemonError { kind, report } = source.with_corrosion_cleanup_failed();
        Self {
            kind: DaemonErrorKind::StartupRollback {
                source: Box::new(kind),
                corrosion,
            },
            report,
        }
    }

    pub(crate) fn with_corrosion_shutdown_report(
        mut self,
        shutdown: polis::CorrosionShutdown,
    ) -> Self {
        self.report = self.report.with_corrosion_shutdown(&shutdown);
        self
    }

    pub(crate) fn with_corrosion_cleanup_failed(mut self) -> Self {
        self.report = self.report.with_corrosion_cleanup_failed();
        self
    }

    pub(crate) fn shutdown(
        peer: Option<polis::PeerError>,
        corrosion: Option<polis::CorrosionAgentError>,
        report: StartupReport,
    ) -> Self {
        Self {
            kind: DaemonErrorKind::Shutdown { peer, corrosion },
            report,
        }
    }

    #[must_use]
    pub fn kind(&self) -> &DaemonErrorKind {
        &self.kind
    }

    #[must_use]
    pub fn startup_report(&self) -> &StartupReport {
        &self.report
    }
}

pub struct DaemonRuntime {
    substrate: DaemonSubstrate,
    shutting_down: Arc<AtomicBool>,
}

impl DaemonRuntime {
    pub async fn start(config: DaemonConfig) -> Result<Self, DaemonError> {
        let substrate = DaemonSubstrate::start(config).await?;
        Ok(Self {
            substrate,
            shutting_down: Arc::new(AtomicBool::new(false)),
        })
    }

    #[must_use]
    pub fn startup_report(&self) -> &StartupReport {
        self.substrate.startup_report()
    }

    #[must_use]
    pub fn endpoint_id(&self) -> polis::IrohEndpointId {
        self.substrate.endpoint_id()
    }

    #[must_use]
    pub fn command_service(
        &self,
    ) -> Result<
        DaemonCommandService<impl OperationLedgerPort + Clone + Send + Sync + 'static>,
        ployz::PrimitiveFailure,
    > {
        self.command_service_with_registry(
            OperationRegistry::empty(),
            OperationLeasePolicy::default(),
        )
    }

    pub(crate) fn command_service_with_registry(
        &self,
        registry: OperationRegistry,
        lease_policy: OperationLeasePolicy,
    ) -> Result<
        DaemonCommandService<impl OperationLedgerPort + Clone + Send + Sync + 'static>,
        ployz::PrimitiveFailure,
    > {
        let owner = OperationOwner::parse(self.endpoint_id().as_str())?;
        Ok(DaemonCommandService::new(
            self.startup_report().clone(),
            ployz::composition::corrosion_operation_ledger(self.substrate.store().clone()),
            owner,
            registry,
            lease_policy,
            self.shutting_down.clone(),
        ))
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn store(&self) -> &polis::CorrosionStore {
        self.substrate.store()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn corrosion_process_id(&self) -> Option<u32> {
        self.substrate.corrosion_process_id()
    }

    pub async fn shutdown(self) -> Result<StartupReport, DaemonError> {
        self.shutting_down.store(true, Ordering::Release);
        self.substrate.shutdown().await
    }
}
