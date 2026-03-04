use crate::dataplane::traits::{PortError, PortResult};
use corro_api_types::{QueryEvent, RqliteResult, Statement};
use futures_util::stream::BoxStream;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use uuid::Uuid;

#[derive(Clone)]
pub struct Client {
    addr: SocketAddr,
    inner: corro_client::CorrosionClient,
    http: hyper::Client<hyper::client::HttpConnector>,
}

#[derive(Debug, Clone, Copy)]
pub struct CorrosionHealth {
    pub status: hyper::StatusCode,
    pub gaps: i64,
    pub members: i64,
}

impl Client {
    pub fn new(endpoint: &str, db_path: &Path) -> Result<Self, String> {
        let addr: SocketAddr = endpoint
            .parse()
            .map_err(|e| format!("invalid corrosion endpoint '{endpoint}': {e}"))?;
        let inner = corro_client::CorrosionClient::new(addr, db_path);
        let http = hyper::Client::new();
        Ok(Self { addr, inner, http })
    }

    pub async fn read_conn(&self) -> PortResult<rusqlite::Connection> {
        self.inner
            .pool()
            .dedicated_connection()
            .await
            .map_err(|e| PortError::operation("corrosion read_conn", format!("db connection: {e}")))
    }

    pub async fn execute(
        &self,
        statements: &[Statement],
        op: &'static str,
    ) -> PortResult<Vec<RqliteResult>> {
        let response = self
            .inner
            .execute(statements)
            .await
            .map_err(|e| PortError::operation(op, e.to_string()))?;
        Ok(response.results)
    }

    pub async fn schema(&self, statements: &[Statement]) -> PortResult<()> {
        let schema_res = self
            .inner
            .schema(statements)
            .await
            .map_err(|e| PortError::operation("corrosion schema", e.to_string()))?;
        if let Some(RqliteResult::Error { error }) = schema_res.results.first() {
            return Err(PortError::operation("corrosion schema", error.clone()));
        }
        Ok(())
    }

    pub async fn watch(
        &self,
        statement: &Statement,
    ) -> PortResult<(Uuid, BoxStream<'static, io::Result<QueryEvent>>)> {
        let (watch_id, stream) = self
            .inner
            .watch(statement)
            .await
            .map_err(|e| PortError::operation("watch_machines", e.to_string()))?;
        Ok((watch_id, Box::pin(stream)))
    }

    pub async fn resume_watch(
        &self,
        watch_id: Uuid,
    ) -> PortResult<BoxStream<'static, io::Result<QueryEvent>>> {
        let stream = self
            .inner
            .watched_query(watch_id)
            .await
            .map_err(|e| PortError::operation("watch_machines", e.to_string()))?;
        Ok(Box::pin(stream))
    }

    pub async fn health(&self) -> PortResult<CorrosionHealth> {
        let uri: hyper::Uri = format!("http://{}/v1/health?gaps=0", self.addr)
            .parse()
            .map_err(|e| PortError::operation("sync_status", format!("bad uri: {e}")))?;

        let resp = self
            .http
            .get(uri)
            .await
            .map_err(|e| PortError::operation("sync_status", format!("health request: {e}")))?;

        let status = resp.status();
        let body = hyper::body::to_bytes(resp.into_body())
            .await
            .map_err(|e| PortError::operation("sync_status", format!("read body: {e}")))?;

        let envelope: HealthEnvelope = serde_json::from_slice(&body)
            .map_err(|e| PortError::operation("sync_status", format!("decode health: {e}")))?;
        let health = envelope.response;

        Ok(CorrosionHealth {
            status,
            gaps: health.gaps,
            members: health.members,
        })
    }
}

#[derive(serde::Deserialize)]
struct HealthEnvelope {
    response: InnerHealth,
}

#[derive(serde::Deserialize)]
struct InnerHealth {
    gaps: i64,
    members: i64,
}
