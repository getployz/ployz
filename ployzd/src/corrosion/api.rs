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
        Self::from_builder(address, token, |builder| builder.http2_prior_knowledge())
    }

    #[cfg(test)]
    pub(crate) fn http1(address: SocketAddr, token: &str) -> Result<Self, Error> {
        Self::from_builder(address, token, |builder| builder)
    }

    fn from_builder(
        address: SocketAddr,
        token: &str,
        configure: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
    ) -> Result<Self, Error> {
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
        let client = configure(Client::builder())
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
            snapshot_in_progress: false,
        };
        match subscription.next_event().await? {
            QueryEvent::Columns(_) => {
                subscription.snapshot_in_progress = true;
                subscription.finish_snapshot().await?;
                Ok(subscription)
            }
            QueryEvent::Change(_) => Err(Error::Protocol(
                "subscription change preceded end-of-query".into(),
            )),
            QueryEvent::Error(error) => Err(Error::Api(error)),
            QueryEvent::Row(_, _) | QueryEvent::EndOfQuery { .. } => Err(Error::Protocol(
                "subscription snapshot event arrived without columns".into(),
            )),
        }
    }
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>;

pub(crate) struct Subscription {
    lines: FramedRead<StreamReader<ByteStream, Bytes>, LinesCodec>,
    snapshot_in_progress: bool,
}

impl Subscription {
    pub(crate) async fn changed(&mut self) -> Result<(), Error> {
        if self.snapshot_in_progress {
            return self.finish_snapshot().await;
        }
        match self.next_event().await? {
            QueryEvent::Change(_) => Ok(()),
            QueryEvent::Error(error) => Err(Error::Api(error)),
            QueryEvent::Columns(_) => {
                self.snapshot_in_progress = true;
                self.finish_snapshot().await
            }
            QueryEvent::Row(_, _) | QueryEvent::EndOfQuery { .. } => Err(Error::Protocol(
                "subscription snapshot event arrived without columns".into(),
            )),
        }
    }

    async fn finish_snapshot(&mut self) -> Result<(), Error> {
        loop {
            match self.next_event().await? {
                QueryEvent::Row(_, _) => {}
                QueryEvent::EndOfQuery { .. } => {
                    self.snapshot_in_progress = false;
                    return Ok(());
                }
                QueryEvent::Error(error) => return Err(Error::Api(error)),
                QueryEvent::Columns(_) | QueryEvent::Change(_) => {
                    return Err(Error::Protocol(
                        "subscription snapshot events arrived out of order".into(),
                    ));
                }
            }
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

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use axum::{Router, body::Body, routing::post};
    use bytes::Bytes;
    use tokio::{net::TcpListener, sync::mpsc};
    use tokio_stream::wrappers::UnboundedReceiverStream;

    use super::{ApiClient, Statement};

    #[tokio::test]
    async fn subscription_accepts_live_change_resnapshot_and_later_change() {
        const BEFORE_CANCEL: &[u8] = b"{\"columns\":[\"id\"]}\n\
{\"row\":[1,[\"initial\"]]}\n\
{\"eoq\":{\"time\":0.0}}\n\
{\"change\":{}}\n\
{\"columns\":[\"id\"]}\n";
        const AFTER_CANCEL: &[u8] = b"\
{\"row\":[2,[\"replacement\"]]}\n\
{\"eoq\":{\"time\":0.0}}\n\
{\"change\":{}}\n";
        let (events, receiver) = mpsc::unbounded_channel::<Result<Bytes, Infallible>>();
        let receiver = Arc::new(Mutex::new(Some(receiver)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_receiver = receiver.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/v1/subscriptions",
                    post(move || {
                        let receiver = server_receiver.lock().unwrap().take().unwrap();
                        async move { Body::from_stream(UnboundedReceiverStream::new(receiver)) }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let client = ApiClient::http1(address, &"a".repeat(64)).unwrap();
        events.send(Ok(Bytes::from_static(BEFORE_CANCEL))).unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            let mut subscription = client
                .subscribe(Statement::new("SELECT id FROM test", []))
                .await
                .unwrap();
            subscription.changed().await.unwrap();
            tokio::time::timeout(Duration::from_millis(100), subscription.changed())
                .await
                .expect_err("replacement snapshot should wait for end-of-query");
            assert!(subscription.snapshot_in_progress);
            events.send(Ok(Bytes::from_static(AFTER_CANCEL))).unwrap();
            subscription.changed().await.unwrap();
            subscription.changed().await.unwrap();
        })
        .await
        .unwrap();
        server.abort();
    }
}
