//! Immutable Ingress Proxy Backend values in the replicated Cluster store.

use ployz_core::{INGRESS_PROXY_BACKEND_CLUSTER_KEY, IngressProxyBackend};
use serde_json::json;

use super::{ReplicatedStore, text};
use crate::corrosion::{Error, Statement};

pub(crate) const PUBLISH_FOUNDING_INGRESS_PROXY_BACKEND: &str = "INSERT INTO cluster (key, value, updated_at) VALUES (?, ?, datetime('now')) ON CONFLICT (key) DO NOTHING";

impl ReplicatedStore {
    /// Record the founder's Ingress Proxy Backend without an update path.
    ///
    /// Repeating the same founding value is idempotent. A different existing
    /// value is refused rather than changed.
    ///
    /// # Errors
    ///
    /// Returns if the Cluster store cannot be written or strictly read, or if
    /// another backend is already recorded.
    pub(crate) async fn publish_founding_ingress_proxy_backend(
        &self,
        backend: IngressProxyBackend,
    ) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                PUBLISH_FOUNDING_INGRESS_PROXY_BACKEND,
                [
                    json!(INGRESS_PROXY_BACKEND_CLUSTER_KEY),
                    json!(backend.as_str()),
                ],
            )])
            .await?;
        let recorded = self.ingress_proxy_backend().await?;
        if recorded == backend {
            Ok(())
        } else {
            Err(Error::Protocol(format!(
                "Ingress Proxy Backend is already {recorded}; cannot change it to {backend}"
            )))
        }
    }

    /// Strictly read the Cluster's founding-time Ingress Proxy Backend.
    ///
    /// # Errors
    ///
    /// Returns when the row is missing, malformed, or unrecognized.
    pub async fn ingress_proxy_backend(&self) -> Result<IngressProxyBackend, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT value FROM cluster WHERE key = ?",
                [json!(INGRESS_PROXY_BACKEND_CLUSTER_KEY)],
            ))
            .await?;
        let rows = query.rows(["value"])?;
        let Some([value]) = rows.first() else {
            return Err(Error::Protocol("Ingress Proxy Backend is missing".into()));
        };
        Ok(IngressProxyBackend::parse(text(
            value,
            "Ingress Proxy Backend",
        )?)?)
    }

    /// Refuse when the Cluster's backend is absent, unrecognized, or different.
    ///
    /// # Errors
    ///
    /// Returns the strict read failure or a backend mismatch.
    pub(crate) async fn require_ingress_proxy_backend(
        &self,
        expected: IngressProxyBackend,
    ) -> Result<(), Error> {
        let recorded = self.ingress_proxy_backend().await?;
        if recorded == expected {
            Ok(())
        } else {
            Err(Error::Protocol(format!(
                "Cluster Ingress Proxy Backend is {recorded}, not {expected}"
            )))
        }
    }
}
