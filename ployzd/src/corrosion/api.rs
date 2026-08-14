use std::{io, net::SocketAddr, pin::Pin, time::Duration};

use bytes::Bytes;
use futures_util::{Stream, StreamExt, TryStreamExt};
use reqwest::{Client, Url, header};
use serde::{Deserialize, Serialize, de::IgnoredAny};
use serde_json::Value;
use tokio_util::{
    codec::{FramedRead, LinesCodec},
    io::StreamReader,
};

use super::Error;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Statement {
    query: String,
    params: Vec<Value>,
}

impl Statement {
    pub(crate) fn new(query: impl Into<String>, params: impl IntoIterator<Item = Value>) -> Self {
        Self {
            query: query.into(),
            params: params.into_iter().collect(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ApiClient {
    base_url: Url,
    client: Client,
}

impl ApiClient {
    pub(crate) fn new(address: SocketAddr, token: &str) -> Result<Self, Error> {
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

    pub(crate) async fn execute(
        &self,
        statements: impl IntoIterator<Item = Statement>,
    ) -> Result<(), Error> {
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
                .and_then(|response| {
                    response
                        .results
                        .into_iter()
                        .find_map(|result| match result {
                            ExecResult::Error { error } => Some(error),
                            ExecResult::Executed { .. } => None,
                        })
                })
                .map_or_else(
                    || Error::Api(format!("HTTP {status}: {}", String::from_utf8_lossy(&body))),
                    Error::Api,
                ));
        }
        let decoded = decoded?;
        let errors = decoded
            .results
            .into_iter()
            .filter_map(|result| match result {
                ExecResult::Error { error } => Some(error),
                ExecResult::Executed { .. } => None,
            });
        let errors = errors.collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::Api(errors.join("; ")))
        }
    }

    pub(crate) async fn query(&self, statement: Statement) -> Result<QueryResult, Error> {
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
        let mut ended = false;
        for event in serde_json::Deserializer::from_slice(&body).into_iter::<QueryEvent>() {
            if ended {
                return Err(Error::Protocol("query event followed end-of-query".into()));
            }
            let event = event?;
            match event {
                QueryEvent::Columns(names) if columns.is_none() && rows.is_empty() => {
                    columns = Some(names);
                }
                QueryEvent::Row(_row_id, values) if columns.is_some() => rows.push(values),
                QueryEvent::EndOfQuery { .. } if columns.is_some() => ended = true,
                QueryEvent::Error(error) => return Err(Error::Api(error)),
                QueryEvent::Columns(_)
                | QueryEvent::Row(_, _)
                | QueryEvent::EndOfQuery { .. }
                | QueryEvent::Change(_) => {
                    return Err(Error::Protocol("query events arrived out of order".into()));
                }
            }
        }
        if !ended {
            return Err(Error::Protocol("missing end-of-query event".into()));
        }
        let columns = columns.ok_or_else(|| Error::Protocol("missing columns event".into()))?;
        if rows.iter().any(|row| row.len() != columns.len()) {
            return Err(Error::Protocol("row length does not match columns".into()));
        }
        Ok(QueryResult { columns, rows })
    }

    pub(crate) async fn subscribe(&self, statement: Statement) -> Result<Subscription, Error> {
        let response = self
            .client
            .post(self.base_url.join("v1/subscriptions").expect("static path"))
            .json(&statement)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Api(format!(
                "HTTP {status}: {}",
                response.text().await?
            )));
        }
        let chunks: ByteStream = Box::pin(response.bytes_stream().map_err(io::Error::other));
        let mut subscription = Subscription {
            lines: FramedRead::new(
                StreamReader::new(chunks),
                LinesCodec::new_with_max_length(8 * 1024 * 1024),
            ),
        };
        loop {
            match subscription.next_event().await? {
                QueryEvent::Columns(_) | QueryEvent::Row(_, _) => {}
                QueryEvent::EndOfQuery { .. } => return Ok(subscription),
                QueryEvent::Change(_) => {
                    return Err(Error::Protocol(
                        "subscription change preceded end-of-query".into(),
                    ));
                }
                QueryEvent::Error(error) => return Err(Error::Api(error)),
            }
        }
    }
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>;

pub(crate) struct Subscription {
    lines: FramedRead<StreamReader<ByteStream, Bytes>, LinesCodec>,
}

impl Subscription {
    pub(crate) async fn changed(&mut self) -> Result<(), Error> {
        match self.next_event().await? {
            QueryEvent::Change(_) => Ok(()),
            QueryEvent::Error(error) => Err(Error::Api(error)),
            QueryEvent::Columns(_) | QueryEvent::Row(_, _) | QueryEvent::EndOfQuery { .. } => Err(
                Error::Protocol("initial subscription event followed end-of-query".into()),
            ),
        }
    }

    async fn next_event(&mut self) -> Result<QueryEvent, Error> {
        loop {
            let line = self
                .lines
                .next()
                .await
                .ok_or_else(|| Error::Protocol("subscription stream closed".into()))?
                .map_err(|error| Error::Protocol(error.to_string()))?;
            if !line.trim().is_empty() {
                return Ok(serde_json::from_str(&line)?);
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecResponse {
    results: Vec<ExecResult>,
    #[serde(rename = "time")]
    _time: f64,
    #[serde(rename = "version")]
    _version: Option<u64>,
    #[serde(rename = "actor_id")]
    _actor_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, untagged)]
enum ExecResult {
    Executed {
        #[serde(rename = "rows_affected")]
        _rows_affected: usize,
        #[serde(rename = "time")]
        _time: f64,
    },
    Error {
        error: String,
    },
}

#[derive(Debug)]
pub(crate) struct QueryResult {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

impl QueryResult {
    pub(crate) fn rows<const N: usize>(
        self,
        expected: [&str; N],
    ) -> Result<Vec<[Value; N]>, Error> {
        if self.columns != expected {
            return Err(Error::Protocol(format!(
                "unexpected columns: {:?}",
                self.columns
            )));
        }
        self.rows
            .into_iter()
            .map(|row| {
                row.try_into()
                    .map_err(|_| Error::Protocol("row has unexpected width".into()))
            })
            .collect()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum QueryEvent {
    Columns(Vec<String>),
    Row(u64, Vec<Value>),
    #[serde(rename = "eoq")]
    EndOfQuery {
        #[serde(rename = "time")]
        _time: f64,
        #[serde(default)]
        #[serde(rename = "change_id")]
        _change_id: Option<u64>,
    },
    Change(IgnoredAny),
    Error(String),
}
