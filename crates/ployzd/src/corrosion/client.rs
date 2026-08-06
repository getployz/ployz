use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use ployz_core::corrosion::{
    CORROSION_NO_P99_LAG_SAMPLE, ChangeId, ChangeKind, CorrosionHealthResponse, EndOfQuery,
    QueryEvent, QueryIdentity, RowId, SqliteValue, Statement, SubscriptionEvent,
    TransactionResponse, TransactionResult,
};
use reqwest::Url;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};

use super::stream::{NdjsonStream, StreamReadError};

const QUERY_ID_HEADER: &str = "corro-query-id";
const QUERY_HASH_HEADER: &str = "corro-query-hash";

/// A bearer token whose debug representation never exposes the credential.
#[derive(Clone, PartialEq, Eq)]
pub struct BearerToken(HeaderValue);

impl BearerToken {
    pub fn new(value: impl Into<String>) -> Result<Self, CorrosionClientConfigError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CorrosionClientConfigError::EmptyBearerToken);
        }
        let authorization = format!("Bearer {value}");
        let authorization = HeaderValue::from_str(&authorization)
            .map_err(|_| CorrosionClientConfigError::InvalidBearerToken)?;
        Ok(Self(authorization))
    }

    fn into_authorization(self) -> HeaderValue {
        self.0
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerToken([REDACTED])")
    }
}

#[derive(Clone)]
struct RedactedAuthorization(HeaderValue);

impl fmt::Debug for RedactedAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Explicit resource and time bounds for one Corrosion client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorrosionClientBounds {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub stream_idle_timeout: Duration,
    pub max_ndjson_frame_bytes: usize,
    pub max_error_body_bytes: usize,
}

/// A validated connection to one machine-local Corrosion agent.
#[derive(Clone)]
pub struct CorrosionClientConfig {
    api_addr: SocketAddr,
    bearer_token: BearerToken,
    connect_timeout: Duration,
    request_timeout: Duration,
    stream_idle_timeout: Duration,
    max_ndjson_frame_bytes: usize,
    max_error_body_bytes: usize,
}

impl CorrosionClientConfig {
    pub fn new(
        api_addr: SocketAddr,
        bearer_token: BearerToken,
        bounds: CorrosionClientBounds,
    ) -> Result<Self, CorrosionClientConfigError> {
        let CorrosionClientBounds {
            connect_timeout,
            request_timeout,
            stream_idle_timeout,
            max_ndjson_frame_bytes,
            max_error_body_bytes,
        } = bounds;
        if !api_addr.ip().is_loopback() {
            return Err(CorrosionClientConfigError::NonLoopbackAddress(api_addr));
        }
        if connect_timeout.is_zero() {
            return Err(CorrosionClientConfigError::ZeroConnectTimeout);
        }
        if request_timeout.is_zero() {
            return Err(CorrosionClientConfigError::ZeroRequestTimeout);
        }
        if stream_idle_timeout.is_zero() {
            return Err(CorrosionClientConfigError::ZeroStreamIdleTimeout);
        }
        if max_ndjson_frame_bytes == 0 {
            return Err(CorrosionClientConfigError::ZeroFrameLimit);
        }
        if max_error_body_bytes == 0 {
            return Err(CorrosionClientConfigError::ZeroErrorBodyLimit);
        }

        Ok(Self {
            api_addr,
            bearer_token,
            connect_timeout,
            request_timeout,
            stream_idle_timeout,
            max_ndjson_frame_bytes,
            max_error_body_bytes,
        })
    }
}

impl fmt::Debug for CorrosionClientConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            api_addr,
            bearer_token,
            connect_timeout,
            request_timeout,
            stream_idle_timeout,
            max_ndjson_frame_bytes,
            max_error_body_bytes,
        } = self;
        formatter
            .debug_struct("CorrosionClientConfig")
            .field("api_addr", api_addr)
            .field("bearer_token", bearer_token)
            .field("connect_timeout", connect_timeout)
            .field("request_timeout", request_timeout)
            .field("stream_idle_timeout", stream_idle_timeout)
            .field("max_ndjson_frame_bytes", max_ndjson_frame_bytes)
            .field("max_error_body_bytes", max_error_body_bytes)
            .finish()
    }
}

/// A bounded client for the pinned Corrosion v1 HTTP API.
#[derive(Clone)]
pub struct CorrosionClient {
    http: reqwest::Client,
    base_url: Url,
    authorization: RedactedAuthorization,
    request_timeout: Duration,
    stream_idle_timeout: Duration,
    max_ndjson_frame_bytes: usize,
    max_error_body_bytes: usize,
}

impl CorrosionClient {
    pub fn new(config: CorrosionClientConfig) -> Result<Self, CorrosionClientConfigError> {
        let CorrosionClientConfig {
            api_addr,
            bearer_token,
            connect_timeout,
            request_timeout,
            stream_idle_timeout,
            max_ndjson_frame_bytes,
            max_error_body_bytes,
        } = config;
        let base_url = Url::parse(&format!("http://{api_addr}/"))
            .map_err(|_| CorrosionClientConfigError::InvalidBaseUrl)?;
        let http = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .build()
            .map_err(|source| CorrosionClientConfigError::ClientBuild {
                detail: source.without_url().to_string(),
            })?;

        Ok(Self {
            http,
            base_url,
            authorization: RedactedAuthorization(bearer_token.into_authorization()),
            request_timeout,
            stream_idle_timeout,
            max_ndjson_frame_bytes,
            max_error_body_bytes,
        })
    }

    pub async fn execute(
        &self,
        statements: &[Statement],
    ) -> Result<TransactionResponse, CorrosionClientError> {
        if statements.is_empty() {
            return Err(CorrosionClientError::EmptyTransaction);
        }
        let url = self.endpoint(&["v1", "transactions"])?;
        let response = self
            .send_headers(self.authorized(self.http.post(url)).json(statements))
            .await?;
        let response = self.ensure_success(response).await?;
        let body = self
            .read_success_body(response, self.max_ndjson_frame_bytes)
            .await?;
        let decoded: TransactionResponse =
            serde_json::from_slice(&body).map_err(|source| CorrosionClientError::Protocol {
                detail: format!("invalid transaction response: {source}"),
            })?;
        if decoded.results.len() != statements.len() {
            return Err(CorrosionClientError::Protocol {
                detail: format!(
                    "transaction returned {} results for {} statements",
                    decoded.results.len(),
                    statements.len()
                ),
            });
        }
        for (index, result) in decoded.results.iter().enumerate() {
            if let TransactionResult::Error(error) = result {
                let (detail, _) = cap_text(&error.message, self.max_error_body_bytes);
                return Err(CorrosionClientError::StatementFailed { index, detail });
            }
        }
        Ok(decoded)
    }

    /// Samples the bounded machine-local Corrosion health endpoint.
    ///
    pub async fn health(&self) -> Result<CorrosionHealthResponse, CorrosionHealthReadError> {
        let response = async {
            let url = self.endpoint(&["v1", "health"])?;
            let response = self
                .send_headers(self.authorized(self.http.get(url)))
                .await?;
            let status = response.status();
            let (body, truncated) = self
                .read_body_capped(response, self.max_error_body_bytes)
                .await?;
            Ok::<_, CorrosionClientError>((status, body, truncated))
        }
        .await;

        match response {
            Ok((status, body, truncated)) => {
                decode_health_response(status.as_u16(), &body, truncated)
            }
            Err(
                CorrosionClientError::RequestTimeout { .. }
                | CorrosionClientError::Transport { .. },
            ) => Err(CorrosionHealthReadError::Unavailable),
            Err(_) => Err(CorrosionHealthReadError::InvalidResponse),
        }
    }

    pub async fn query(&self, statement: &Statement) -> Result<QueryStream, CorrosionClientError> {
        let url = self.endpoint(&["v1", "queries"])?;
        let response = self
            .send_headers(self.authorized(self.http.post(url)).json(statement))
            .await?;
        let response = self.ensure_success(response).await?;
        Ok(QueryStream {
            stream: NdjsonStream::new(
                response,
                self.stream_idle_timeout,
                self.max_ndjson_frame_bytes,
            ),
            diagnostic_limit: self.max_error_body_bytes,
            complete: false,
            failed: false,
        })
    }

    pub async fn subscribe(
        &self,
        statement: &Statement,
    ) -> Result<SubscriptionStream, CorrosionClientError> {
        let mut url = self.endpoint(&["v1", "subscriptions"])?;
        url.query_pairs_mut().append_pair("skip_rows", "false");
        let response = self
            .send_headers(self.authorized(self.http.post(url)).json(statement))
            .await?;
        self.subscription_response(response, None).await
    }

    pub async fn resume(
        &self,
        identity: &QueryIdentity,
        from: ChangeId,
    ) -> Result<SubscriptionStream, CorrosionClientError> {
        let id = identity.id().to_string();
        let mut url = self.endpoint(&["v1", "subscriptions", &id])?;
        url.query_pairs_mut()
            .append_pair("skip_rows", "true")
            .append_pair("from", &from.get().to_string());
        let response = self
            .send_headers(self.authorized(self.http.get(url)))
            .await?;
        let stream = self.subscription_response(response, Some(from)).await?;
        if stream.identity() != identity {
            return Err(CorrosionClientError::SubscriptionIdentityChanged);
        }
        Ok(stream)
    }

    async fn subscription_response(
        &self,
        response: reqwest::Response,
        from: Option<ChangeId>,
    ) -> Result<SubscriptionStream, CorrosionClientError> {
        let response = self.ensure_success(response).await?;
        let identity = query_identity(response.headers())?;
        Ok(SubscriptionStream {
            identity,
            stream: NdjsonStream::new(
                response,
                self.stream_idle_timeout,
                self.max_ndjson_frame_bytes,
            ),
            diagnostic_limit: self.max_error_body_bytes,
            phase: match from {
                Some(cursor) => SubscriptionPhase::Live { cursor },
                None => SubscriptionPhase::Snapshot,
            },
            failed: false,
        })
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header(AUTHORIZATION, self.authorization.0.clone())
            .header(ACCEPT, "application/json")
    }

    async fn send_headers(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, CorrosionClientError> {
        tokio::time::timeout(self.request_timeout, request.send())
            .await
            .map_err(|_| CorrosionClientError::RequestTimeout {
                timeout: self.request_timeout,
            })?
            .map_err(|source| CorrosionClientError::Transport {
                detail: source.without_url().to_string(),
            })
    }

    async fn ensure_success(
        &self,
        response: reqwest::Response,
    ) -> Result<reqwest::Response, CorrosionClientError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let (detail, truncated) = self.read_error_body(response).await?;
        Err(CorrosionClientError::HttpStatus {
            status: status.as_u16(),
            detail,
            truncated,
        })
    }

    async fn read_error_body(
        &self,
        response: reqwest::Response,
    ) -> Result<(String, bool), CorrosionClientError> {
        let (bytes, truncated) = self
            .read_body_capped(response, self.max_error_body_bytes)
            .await?;
        Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
    }

    async fn read_success_body(
        &self,
        response: reqwest::Response,
        limit: usize,
    ) -> Result<Vec<u8>, CorrosionClientError> {
        let (bytes, truncated) = self.read_body_capped(response, limit).await?;
        if truncated {
            return Err(CorrosionClientError::ResponseTooLarge { limit });
        }
        Ok(bytes)
    }

    async fn read_body_capped(
        &self,
        response: reqwest::Response,
        limit: usize,
    ) -> Result<(Vec<u8>, bool), CorrosionClientError> {
        let read = async move {
            let mut body = response.bytes_stream();
            let mut collected = Vec::with_capacity(limit.min(8 * 1024));
            while collected.len() <= limit {
                let Some(chunk) = body.next().await else {
                    return Ok((collected, false));
                };
                let chunk = chunk.map_err(|source| CorrosionClientError::Transport {
                    detail: source.without_url().to_string(),
                })?;
                let remaining = limit.saturating_sub(collected.len());
                if chunk.len() > remaining {
                    let Some(prefix) = chunk.get(..remaining) else {
                        return Err(CorrosionClientError::Protocol {
                            detail: "HTTP body limit did not form a valid byte range".to_owned(),
                        });
                    };
                    collected.extend_from_slice(prefix);
                    return Ok((collected, true));
                }
                collected.extend_from_slice(&chunk);
            }
            Ok((collected, true))
        };

        tokio::time::timeout(self.request_timeout, read)
            .await
            .map_err(|_| CorrosionClientError::RequestTimeout {
                timeout: self.request_timeout,
            })?
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url, CorrosionClientError> {
        let mut url = self.base_url.clone();
        let Ok(mut path) = url.path_segments_mut() else {
            return Err(CorrosionClientError::Protocol {
                detail: "Corrosion base URL cannot contain path segments".to_owned(),
            });
        };
        path.clear().extend(segments);
        drop(path);
        Ok(url)
    }
}

fn decode_health_response(
    status: u16,
    body: &[u8],
    truncated: bool,
) -> Result<CorrosionHealthResponse, CorrosionHealthReadError> {
    if truncated {
        return Err(CorrosionHealthReadError::InvalidResponse);
    }
    let decoded = serde_json::from_slice::<CorrosionHealthResponse>(body);
    match (status, decoded) {
        (200..=299, Ok(response)) => Ok(response),
        (503, Ok(CorrosionHealthResponse::Error(message)))
            if message == CORROSION_NO_P99_LAG_SAMPLE =>
        {
            Ok(CorrosionHealthResponse::Error(message))
        }
        (_, Ok(_)) | (_, Err(_)) => Err(CorrosionHealthReadError::InvalidResponse),
    }
}

impl fmt::Debug for CorrosionClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            http: _,
            base_url,
            authorization,
            request_timeout,
            stream_idle_timeout,
            max_ndjson_frame_bytes,
            max_error_body_bytes,
        } = self;
        formatter
            .debug_struct("CorrosionClient")
            .field("base_url", base_url)
            .field("authorization", authorization)
            .field("request_timeout", request_timeout)
            .field("stream_idle_timeout", stream_idle_timeout)
            .field("max_ndjson_frame_bytes", max_ndjson_frame_bytes)
            .field("max_error_body_bytes", max_error_body_bytes)
            .finish_non_exhaustive()
    }
}

/// One validated frame from a bounded query response.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryStreamEvent {
    Columns(Vec<String>),
    Row(RowId, Vec<SqliteValue>),
    EndOfQuery(EndOfQuery),
}

/// A query response that is consumed one bounded NDJSON frame at a time.
#[derive(Debug)]
pub struct QueryStream {
    stream: NdjsonStream,
    diagnostic_limit: usize,
    complete: bool,
    failed: bool,
}

impl QueryStream {
    pub async fn next(&mut self) -> Result<Option<QueryStreamEvent>, CorrosionClientError> {
        if self.complete || self.failed {
            return Ok(None);
        }
        let event = match self.stream.next().await {
            Ok(Some(event)) => serde_json::from_value::<QueryEvent>(event).map_err(|source| {
                self.failed = true;
                CorrosionClientError::InvalidFrame {
                    detail: source.to_string(),
                }
            })?,
            Ok(None) => {
                self.failed = true;
                return Err(CorrosionClientError::MissingEndOfQuery);
            }
            Err(error) => {
                self.failed = true;
                return Err(map_stream_error(error));
            }
        };
        match event {
            QueryEvent::EndOfQuery(end) if end.change_id.is_some() => {
                self.failed = true;
                Err(CorrosionClientError::Protocol {
                    detail: "query end-of-query frame unexpectedly included a change id".to_owned(),
                })
            }
            QueryEvent::EndOfQuery(end) => {
                self.complete = true;
                Ok(Some(QueryStreamEvent::EndOfQuery(end)))
            }
            QueryEvent::Error(detail) => {
                self.failed = true;
                let (detail, _) = cap_text(&detail, self.diagnostic_limit);
                Err(CorrosionClientError::FatalStream { detail })
            }
            QueryEvent::Columns(columns) => Ok(Some(QueryStreamEvent::Columns(columns))),
            QueryEvent::Row(row_id, values) => Ok(Some(QueryStreamEvent::Row(row_id, values))),
        }
    }
}

/// One validated frame from a bounded subscription response.
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionStreamEvent {
    Columns(Vec<String>),
    Row(RowId, Vec<SqliteValue>),
    EndOfQuery(EndOfQuery),
    Change(ChangeKind, RowId, Vec<SqliteValue>, ChangeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubscriptionPhase {
    Snapshot,
    Live { cursor: ChangeId },
}

/// A subscription response with explicit identity and contiguous cursor tracking.
#[derive(Debug)]
pub struct SubscriptionStream {
    identity: QueryIdentity,
    stream: NdjsonStream,
    diagnostic_limit: usize,
    phase: SubscriptionPhase,
    failed: bool,
}

impl SubscriptionStream {
    #[must_use]
    pub fn identity(&self) -> &QueryIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn last_contiguous_change_id(&self) -> Option<ChangeId> {
        match self.phase {
            SubscriptionPhase::Snapshot => None,
            SubscriptionPhase::Live { cursor } => Some(cursor),
        }
    }

    pub async fn next(&mut self) -> Result<Option<SubscriptionStreamEvent>, CorrosionClientError> {
        if self.failed {
            return Ok(None);
        }
        let event = match self.stream.next().await {
            Ok(Some(event)) => {
                serde_json::from_value::<SubscriptionEvent>(event).map_err(|source| {
                    self.failed = true;
                    CorrosionClientError::InvalidFrame {
                        detail: source.to_string(),
                    }
                })?
            }
            Ok(None) => {
                self.failed = true;
                return Err(CorrosionClientError::SubscriptionEnded);
            }
            Err(error) => {
                self.failed = true;
                return Err(map_stream_error(error));
            }
        };
        match (self.phase, event) {
            (SubscriptionPhase::Snapshot, SubscriptionEvent::Columns(columns)) => {
                Ok(Some(SubscriptionStreamEvent::Columns(columns)))
            }
            (SubscriptionPhase::Snapshot, SubscriptionEvent::Row(row_id, values)) => {
                Ok(Some(SubscriptionStreamEvent::Row(row_id, values)))
            }
            (SubscriptionPhase::Snapshot, SubscriptionEvent::EndOfQuery(end)) => {
                let Some(change_id) = end.change_id else {
                    self.failed = true;
                    return Err(CorrosionClientError::Protocol {
                        detail: "subscription end-of-query frame omitted its change id".to_owned(),
                    });
                };
                self.phase = SubscriptionPhase::Live { cursor: change_id };
                Ok(Some(SubscriptionStreamEvent::EndOfQuery(end)))
            }
            (
                SubscriptionPhase::Live { cursor },
                SubscriptionEvent::Change(kind, row_id, values, actual),
            ) => {
                let Some(expected) = cursor.next() else {
                    self.failed = true;
                    return Err(CorrosionClientError::ChangeIdExhausted { last: cursor });
                };
                if actual != expected {
                    self.failed = true;
                    return Err(CorrosionClientError::NonContiguousChange { expected, actual });
                }
                self.phase = SubscriptionPhase::Live { cursor: actual };
                Ok(Some(SubscriptionStreamEvent::Change(
                    kind, row_id, values, actual,
                )))
            }
            (SubscriptionPhase::Snapshot, SubscriptionEvent::Error(detail))
            | (SubscriptionPhase::Live { .. }, SubscriptionEvent::Error(detail)) => {
                self.failed = true;
                let (detail, _) = cap_text(&detail, self.diagnostic_limit);
                Err(CorrosionClientError::FatalStream { detail })
            }
            (SubscriptionPhase::Snapshot, SubscriptionEvent::Change(_, _, _, _))
            | (SubscriptionPhase::Live { .. }, SubscriptionEvent::Columns(_))
            | (SubscriptionPhase::Live { .. }, SubscriptionEvent::Row(_, _))
            | (SubscriptionPhase::Live { .. }, SubscriptionEvent::EndOfQuery(_)) => {
                self.failed = true;
                Err(CorrosionClientError::Protocol {
                    detail: "subscription frame arrived outside its legal phase".to_owned(),
                })
            }
        }
    }
}

fn query_identity(headers: &HeaderMap) -> Result<QueryIdentity, CorrosionClientError> {
    let id = header_value(headers, QUERY_ID_HEADER)?;
    let hash = header_value(headers, QUERY_HASH_HEADER)?;
    QueryIdentity::from_parts(id, hash).map_err(|source| CorrosionClientError::Protocol {
        detail: source.to_string(),
    })
}

fn header_value<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a str>, CorrosionClientError> {
    headers
        .get(name)
        .map(HeaderValue::to_str)
        .transpose()
        .map_err(|_| CorrosionClientError::Protocol {
            detail: format!("Corrosion response carried a non-text {name} header"),
        })
}

fn map_stream_error(error: StreamReadError) -> CorrosionClientError {
    match error {
        StreamReadError::IdleTimeout { timeout } => {
            CorrosionClientError::StreamIdleTimeout { timeout }
        }
        StreamReadError::FrameTooLarge { limit } => CorrosionClientError::FrameTooLarge { limit },
        StreamReadError::TruncatedFrame => CorrosionClientError::TruncatedFrame,
        StreamReadError::InvalidJson { detail } => CorrosionClientError::InvalidFrame { detail },
        StreamReadError::Transport { detail } => CorrosionClientError::Transport { detail },
    }
}

fn cap_text(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let Some(prefix) = value.get(..boundary) else {
        return (String::new(), true);
    };
    (prefix.to_owned(), true)
}

/// Invalid client configuration rejected before any external I/O.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorrosionClientConfigError {
    #[error("Corrosion bearer token is empty")]
    EmptyBearerToken,
    #[error("Corrosion bearer token cannot be represented as an HTTP header")]
    InvalidBearerToken,
    #[error("Corrosion API address {0} is not loopback")]
    NonLoopbackAddress(SocketAddr),
    #[error("Corrosion connect timeout must be nonzero")]
    ZeroConnectTimeout,
    #[error("Corrosion request timeout must be nonzero")]
    ZeroRequestTimeout,
    #[error("Corrosion stream idle timeout must be nonzero")]
    ZeroStreamIdleTimeout,
    #[error("Corrosion NDJSON frame limit must be nonzero")]
    ZeroFrameLimit,
    #[error("Corrosion HTTP error body limit must be nonzero")]
    ZeroErrorBodyLimit,
    #[error("Corrosion API address could not form a base URL")]
    InvalidBaseUrl,
    #[error("Corrosion HTTP client could not be built: {detail}")]
    ClientBuild { detail: String },
}

/// A bounded Corrosion request or stream failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorrosionClientError {
    #[error("Corrosion transaction contains no statements")]
    EmptyTransaction,
    #[error("Corrosion request timed out after {timeout:?}")]
    RequestTimeout { timeout: Duration },
    #[error("Corrosion transport failed: {detail}")]
    Transport { detail: String },
    #[error("Corrosion returned HTTP {status}: {detail}")]
    HttpStatus {
        status: u16,
        detail: String,
        truncated: bool,
    },
    #[error("Corrosion response exceeded the {limit}-byte limit")]
    ResponseTooLarge { limit: usize },
    #[error("Corrosion statement {index} failed: {detail}")]
    StatementFailed { index: usize, detail: String },
    #[error("Corrosion protocol failure: {detail}")]
    Protocol { detail: String },
    #[error("Corrosion NDJSON frame exceeded the {limit}-byte limit")]
    FrameTooLarge { limit: usize },
    #[error("Corrosion stream ended with an unterminated NDJSON frame")]
    TruncatedFrame,
    #[error("Corrosion stream contained an invalid frame: {detail}")]
    InvalidFrame { detail: String },
    #[error("Corrosion stream was idle for {timeout:?}")]
    StreamIdleTimeout { timeout: Duration },
    #[error("Corrosion stream failed: {detail}")]
    FatalStream { detail: String },
    #[error("Corrosion query ended before its end-of-query frame")]
    MissingEndOfQuery,
    #[error("Corrosion subscription ended unexpectedly")]
    SubscriptionEnded,
    #[error("Corrosion subscription identity changed while resuming")]
    SubscriptionIdentityChanged,
    #[error("Corrosion subscription change id is exhausted after {last:?}")]
    ChangeIdExhausted { last: ChangeId },
    #[error("Corrosion subscription skipped a change (expected {expected:?}, got {actual:?})")]
    NonContiguousChange {
        expected: ChangeId,
        actual: ChangeId,
    },
}

/// A normalized failure to sample the machine-local Corrosion health endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CorrosionHealthReadError {
    #[error("Corrosion health is unavailable")]
    Unavailable,
    #[error("Corrosion returned an invalid health response")]
    InvalidResponse,
}

#[cfg(test)]
mod health_tests {
    use ployz_core::corrosion::CorrosionHealthResponse;

    use super::{CorrosionHealthReadError, decode_health_response};

    #[test]
    fn literal_success_health_response_decodes() {
        let health = decode_health_response(
            200,
            br#"{"response":{"gaps":0,"members":3,"p99_lag":0.125,"queue_size":0}}"#,
            false,
        );

        assert_eq!(
            health,
            Ok(CorrosionHealthResponse::Response {
                gaps: 0,
                members: 3,
                p99_lag: 0.125,
                queue_size: 0,
            })
        );
    }

    #[test]
    fn exact_cold_start_503_is_a_health_reply_not_degradation() {
        let health = decode_health_response(
            503,
            br#"{"error":"no p99 lag information available"}"#,
            false,
        );

        assert_eq!(
            health,
            Ok(CorrosionHealthResponse::Error(
                "no p99 lag information available".to_owned()
            ))
        );
    }

    #[test]
    fn noncanonical_health_errors_and_malformed_successes_are_protocol_failures() {
        let unexpected = decode_health_response(503, br#"{"error":"database unavailable"}"#, false);
        let malformed = decode_health_response(200, br#"{"response":{}}"#, false);

        assert_eq!(unexpected, Err(CorrosionHealthReadError::InvalidResponse));
        assert_eq!(malformed, Err(CorrosionHealthReadError::InvalidResponse));
    }

    #[test]
    fn oversized_health_body_is_a_protocol_failure() {
        assert_eq!(
            decode_health_response(200, b"{}", true),
            Err(CorrosionHealthReadError::InvalidResponse)
        );
    }
}
