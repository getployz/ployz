use std::{net::SocketAddr, time::Duration};

use reqwest::{Client, Url, header};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Error;

#[derive(Clone, Debug, Serialize)]
pub struct Statement {
    query: String,
    params: Vec<Value>,
}

impl Statement {
    pub fn new(query: impl Into<String>, params: impl IntoIterator<Item = Value>) -> Self {
        Self {
            query: query.into(),
            params: params.into_iter().collect(),
        }
    }
}

#[derive(Clone)]
pub struct ApiClient {
    base_url: Url,
    client: Client,
}

impl ApiClient {
    pub fn new(address: SocketAddr, token: &str) -> Result<Self, Error> {
        let base_url = Url::parse(&format!("http://{address}"))
            .map_err(|error| Error::Protocol(error.to_string()))?;
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|error| Error::Protocol(error.to_string()))?,
        );
        let retry = reqwest::retry::for_host(address.ip().to_string())
            .max_retries_per_request(2)
            .classify_fn(|request| {
                if request.error().is_some() {
                    request.retryable()
                } else {
                    request.success()
                }
            });
        let client = Client::builder()
            .http2_prior_knowledge()
            .connect_timeout(Duration::from_secs(3))
            .default_headers(headers)
            .retry(retry)
            .build()?;
        Ok(Self { base_url, client })
    }

    pub async fn execute(
        &self,
        statements: impl IntoIterator<Item = Statement>,
    ) -> Result<ExecResponse, Error> {
        let response = self
            .client
            .post(self.base_url.join("v1/transactions").expect("static path"))
            .json(&statements.into_iter().collect::<Vec<_>>())
            .send()
            .await?;
        let status = response.status();
        // ponytail: finite query responses are buffered; stream events if store size makes this measurable.
        let body = response.bytes().await?;
        let decoded = serde_json::from_slice::<ExecResponse>(&body);
        if !status.is_success() {
            return Err(decoded
                .ok()
                .and_then(|response| response.results.into_iter().find_map(|result| result.error))
                .map_or_else(
                    || Error::Api(format!("HTTP {status}: {}", String::from_utf8_lossy(&body))),
                    Error::Api,
                ));
        }
        let decoded = decoded?;
        let errors = decoded
            .results
            .iter()
            .filter_map(|result| result.error.as_deref())
            .collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(decoded)
        } else {
            Err(Error::Api(errors.join("; ")))
        }
    }

    pub async fn query(&self, statement: Statement) -> Result<QueryResult, Error> {
        let response = self
            .client
            .post(self.base_url.join("v1/queries").expect("static path"))
            .json(&statement)
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;
        if !status.is_success() {
            return Err(Error::Api(format!(
                "HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            )));
        }

        let mut columns = None;
        let mut rows = Vec::new();
        let mut change_id = None;
        for event in serde_json::Deserializer::from_slice(&body).into_iter::<QueryEvent>() {
            let event = event?;
            if let Some(error) = event.error {
                return Err(Error::Api(error));
            }
            if let Some(names) = event.columns {
                columns = Some(names);
            }
            if let Some((_row_id, values)) = event.row {
                rows.push(values);
            }
            if let Some(eoq) = event.eoq {
                change_id = eoq.change_id;
            }
        }
        let columns = columns.ok_or_else(|| Error::Protocol("missing columns event".into()))?;
        if rows.iter().any(|row| row.len() != columns.len()) {
            return Err(Error::Protocol("row length does not match columns".into()));
        }
        Ok(QueryResult {
            columns,
            rows,
            change_id,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct ExecResponse {
    pub results: Vec<ExecResult>,
    #[serde(default)]
    pub version: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ExecResult {
    pub rows_affected: u64,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub change_id: Option<u64>,
}

#[derive(Deserialize)]
struct QueryEvent {
    #[serde(default)]
    columns: Option<Vec<String>>,
    #[serde(default)]
    row: Option<(u64, Vec<Value>)>,
    #[serde(default)]
    eoq: Option<EndOfQuery>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct EndOfQuery {
    #[serde(default)]
    change_id: Option<u64>,
}
