use thiserror::Error;

use crate::config::DaemonConfig;
use crate::report::StartupReport;
use crate::substrate::DaemonSubstrate;

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
}

impl DaemonRuntime {
    pub async fn start(config: DaemonConfig) -> Result<Self, DaemonError> {
        let substrate = DaemonSubstrate::start(config).await?;
        Ok(Self { substrate })
    }

    #[must_use]
    pub fn startup_report(&self) -> &StartupReport {
        self.substrate.startup_report()
    }

    #[must_use]
    pub fn endpoint_id(&self) -> polis::IrohEndpointId {
        self.substrate.endpoint_id()
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
        self.substrate.shutdown().await
    }
}
