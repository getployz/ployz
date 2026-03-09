use std::fmt;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use corro_api_types::{
    ChangeId, ExecResponse, ExecResult, QueryEvent, SqliteValue, Statement, TypedQueryEvent,
};
use futures_util::{Stream, ready};
use http::Uri;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;
use pin_project_lite::pin_project;
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio::time::{Sleep, sleep};
use tokio_util::codec::{Decoder, FramedRead, LinesCodecError};
use tokio_util::io::StreamReader;
use tracing::error;

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// How to reach the Corrosion API endpoint.
#[derive(Clone, Debug)]
pub enum Transport {
    /// Connect directly to the overlay address (host modes).
    Direct,
    /// Connect to a bridge-forwarded local address (Docker mode).
    Bridge { local_addr: SocketAddr },
}

impl Transport {
    /// Resolve the actual TCP connect target for a given logical address.
    fn resolve(&self, logical: SocketAddr) -> SocketAddr {
        match self {
            Transport::Direct => logical,
            Transport::Bridge { local_addr } => *local_addr,
        }
    }
}

// ---------------------------------------------------------------------------
// Connector (tower Service<Uri> → TcpStream)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct BridgeConnector {
    transport: Transport,
}

impl tower::Service<Uri> for BridgeConnector {
    type Response = hyper_util::rt::TokioIo<TcpStream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let transport = self.transport.clone();
        Box::pin(async move {
            let host = uri.host().unwrap_or("127.0.0.1");
            let port = uri.port_u16().unwrap_or(80);

            // Parse the logical address from the URI
            let logical: SocketAddr = if host.starts_with('[') && host.ends_with(']') {
                // IPv6 bracket notation
                let bare = &host[1..host.len() - 1];
                format!("[{bare}]:{port}").parse().map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("bad address: {e}"))
                })?
            } else {
                format!("{host}:{port}").parse().map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("bad address: {e}"))
                })?
            };

            let target = transport.resolve(logical);
            let stream = TcpStream::connect(target).await?;
            Ok(hyper_util::rt::TokioIo::new(stream))
        })
    }
}

// ---------------------------------------------------------------------------
// Body type for requests
// ---------------------------------------------------------------------------

type ReqBody = http_body_util::Full<Bytes>;

fn json_body(data: &impl serde::Serialize) -> Result<ReqBody, serde_json::Error> {
    let bytes = serde_json::to_vec(data)?;
    Ok(http_body_util::Full::new(Bytes::from(bytes)))
}

fn empty_body() -> ReqBody {
    http_body_util::Full::new(Bytes::new())
}

// ---------------------------------------------------------------------------
// CorrClient
// ---------------------------------------------------------------------------

/// Local Corrosion API client with pluggable transport.
#[derive(Clone)]
pub struct CorrClient {
    transport: Transport,
    api_addr: SocketAddr,
    http: HyperClient<BridgeConnector, ReqBody>,
}

impl CorrClient {
    #[must_use]
    pub fn new(api_addr: SocketAddr, transport: Transport) -> Self {
        let connector = BridgeConnector {
            transport: transport.clone(),
        };
        let http = HyperClient::builder(TokioExecutor::new())
            .http2_only(true)
            .build(connector);
        Self {
            transport,
            api_addr,
            http,
        }
    }

    #[must_use]
    pub fn api_addr(&self) -> SocketAddr {
        self.api_addr
    }

    #[must_use]
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.api_addr)
    }

    // -- schema (POST /v1/migrations) --

    pub async fn schema(&self, statements: &[Statement]) -> Result<ExecResponse, ClientError> {
        let uri: Uri = format!("{}/v1/migrations", self.base_url()).parse()?;
        let req = hyper::Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(json_body(&statements)?)?;
        let resp = self.http.request(req).await?;
        let status = resp.status();
        let body = resp.into_body().collect().await?.to_bytes();
        if !status.is_success() {
            return Err(ClientError::UnexpectedStatusCode(status));
        }
        Ok(serde_json::from_slice(&body)?)
    }

    // -- execute (POST /v1/transactions) --

    pub async fn execute(
        &self,
        statements: &[Statement],
        timeout: Option<u64>,
    ) -> Result<ExecResponse, ClientError> {
        let url = if let Some(t) = timeout {
            format!("{}/v1/transactions?timeout={t}", self.base_url())
        } else {
            format!("{}/v1/transactions", self.base_url())
        };
        let uri: Uri = url.parse()?;
        let req = hyper::Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(json_body(&statements)?)?;
        let resp = self.http.request(req).await?;
        let status = resp.status();
        let body = resp.into_body().collect().await?.to_bytes();
        if !status.is_success() {
            if let Ok(exec_resp) = serde_json::from_slice::<ExecResponse>(&body)
                && let Some(ExecResult::Error { error }) = exec_resp
                    .results
                    .into_iter()
                    .find(|r| matches!(r, ExecResult::Error { .. }))
            {
                return Err(ClientError::ResponseError(error));
            }
            return Err(ClientError::UnexpectedStatusCode(status));
        }
        Ok(serde_json::from_slice(&body)?)
    }

    // -- query (POST /v1/queries) --

    pub async fn query(
        &self,
        statement: &Statement,
        timeout: Option<u64>,
    ) -> Result<QueryStream<Vec<SqliteValue>>, ClientError> {
        let url = if let Some(t) = timeout {
            format!("{}/v1/queries?timeout={t}", self.base_url())
        } else {
            format!("{}/v1/queries", self.base_url())
        };
        let uri: Uri = url.parse()?;
        let req = hyper::Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(json_body(&statement)?)?;
        let resp = self.http.request(req).await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.into_body().collect().await?.to_bytes();
            if let Ok(res) = serde_json::from_slice::<ExecResult>(&body)
                && let ExecResult::Error { error } = res
            {
                return Err(ClientError::ResponseError(error));
            }
            return Err(ClientError::UnexpectedStatusCode(status));
        }
        Ok(QueryStream::new(resp.into_body()))
    }

    // -- subscribe (POST /v1/subscriptions) --

    pub async fn subscribe(
        &self,
        statement: &Statement,
        skip_rows: bool,
        from: Option<ChangeId>,
    ) -> Result<SubscriptionStream<Vec<SqliteValue>>, ClientError> {
        let (id, body) = self.subscribe_request(statement, skip_rows, from).await?;
        Ok(SubscriptionStream::new(id, self.clone(), body, from))
    }

    async fn subscribe_request(
        &self,
        statement: &Statement,
        skip_rows: bool,
        from: Option<ChangeId>,
    ) -> Result<(uuid::Uuid, Incoming), ClientError> {
        let mut url = format!("{}/v1/subscriptions?skip_rows={skip_rows}", self.base_url());
        if let Some(change_id) = from {
            use std::fmt::Write;
            write!(&mut url, "&from={change_id}").unwrap();
        }
        let uri: Uri = url.parse()?;
        let req = hyper::Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(json_body(&statement)?)?;
        let resp = self.http.request(req).await?;
        if !resp.status().is_success() {
            return Err(ClientError::UnexpectedStatusCode(resp.status()));
        }
        let id = resp
            .headers()
            .get("corro-query-id")
            .and_then(|v| v.to_str().ok().and_then(|v| v.parse().ok()))
            .ok_or(ClientError::ExpectedQueryId)?;
        Ok((id, resp.into_body()))
    }

    async fn resubscribe(
        &self,
        id: uuid::Uuid,
        from: Option<ChangeId>,
    ) -> Result<Incoming, ClientError> {
        let url = format!(
            "{}/v1/subscriptions/{}?from={}",
            self.base_url(),
            id,
            from.unwrap_or_default()
        );
        let uri: Uri = url.parse()?;
        let req = hyper::Request::builder()
            .method("GET")
            .uri(uri)
            .header("accept", "application/json")
            .body(empty_body())?;
        let resp = self.http.request(req).await?;
        if !resp.status().is_success() {
            return Err(ClientError::UnexpectedStatusCode(resp.status()));
        }
        Ok(resp.into_body())
    }

    // -- health (GET /v1/health?gaps=0) --

    pub async fn health(&self) -> Result<HealthResponse, ClientError> {
        let uri: Uri = format!("{}/v1/health?gaps=0", self.base_url()).parse()?;
        let req = hyper::Request::builder()
            .method("GET")
            .uri(uri)
            .header("accept", "application/json")
            .body(empty_body())?;
        let resp = self.http.request(req).await?;
        let status = resp.status();
        let body = resp.into_body().collect().await?.to_bytes();
        if !status.is_success() {
            return Err(ClientError::UnexpectedStatusCode(status));
        }
        let envelope: HealthEnvelope = serde_json::from_slice(&body)?;
        Ok(envelope.response)
    }
}

// ---------------------------------------------------------------------------
// Health types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct HealthEnvelope {
    response: HealthResponse,
}

#[derive(serde::Deserialize)]
pub struct HealthResponse {
    pub gaps: i64,
    pub members: i64,
}

// ---------------------------------------------------------------------------
// ClientError
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Hyper(#[from] hyper_util::client::legacy::Error),
    #[error(transparent)]
    HyperHttp(#[from] hyper::http::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    InvalidUri(#[from] http::uri::InvalidUri),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("unexpected status code: {0}")]
    UnexpectedStatusCode(http::StatusCode),
    #[error("{0}")]
    ResponseError(String),
    #[error("could not retrieve subscription id from headers")]
    ExpectedQueryId,
    #[error(transparent)]
    BodyCollect(#[from] hyper::Error),
}

impl fmt::Display for CorrClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            transport: _,
            api_addr,
            http: _,
        } = self;
        write!(f, "CorrClient({})", api_addr)
    }
}

// ---------------------------------------------------------------------------
// Incoming → AsyncRead adapter
// ---------------------------------------------------------------------------

pin_project! {
    struct IncomingBodyStream {
        #[pin]
        body: Incoming,
    }
}

impl Stream for IncomingBodyStream {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        use hyper::body::Body;
        let this = self.project();
        let res = ready!(this.body.poll_frame(cx));
        match res {
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    Poll::Ready(Some(Ok(data)))
                } else {
                    // Trailers frame — skip
                    Poll::Ready(Some(Ok(Bytes::new())))
                }
            }
            Some(Err(e)) => {
                let io_err = io::Error::other(e.to_string());
                Poll::Ready(Some(Err(io_err)))
            }
            None => Poll::Ready(None),
        }
    }
}

type IncomingReader = StreamReader<IncomingBodyStream, Bytes>;
type FramedBody = FramedRead<IncomingReader, LinesBytesCodec>;

fn framed_body(body: Incoming) -> FramedBody {
    FramedRead::new(
        StreamReader::new(IncomingBodyStream { body }),
        LinesBytesCodec::default(),
    )
}

// ---------------------------------------------------------------------------
// QueryStream
// ---------------------------------------------------------------------------

pub struct QueryStream<T> {
    stream: FramedBody,
    _deser: std::marker::PhantomData<T>,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Deserialize(#[from] serde_json::Error),
    #[error("max line length exceeded")]
    MaxLineLengthExceeded,
}

impl<T: DeserializeOwned + Unpin> QueryStream<T> {
    fn new(body: Incoming) -> Self {
        Self {
            stream: framed_body(body),
            _deser: std::marker::PhantomData,
        }
    }
}

impl<T: DeserializeOwned + Unpin> Stream for QueryStream<T> {
    type Item = Result<TypedQueryEvent<T>, QueryError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(Pin::new(&mut self.stream).poll_next(cx)) {
            Some(Ok(b)) => match serde_json::from_slice(&b) {
                Ok(evt) => Poll::Ready(Some(Ok(evt))),
                Err(e) => Poll::Ready(Some(Err(e.into()))),
            },
            Some(Err(e)) => match e {
                LinesCodecError::MaxLineLengthExceeded => {
                    Poll::Ready(Some(Err(QueryError::MaxLineLengthExceeded)))
                }
                LinesCodecError::Io(io_err) => Poll::Ready(Some(Err(io_err.into()))),
            },
            None => Poll::Ready(None),
        }
    }
}

// ---------------------------------------------------------------------------
// SubscriptionStream — resilient with reconnection
// ---------------------------------------------------------------------------

pub struct SubscriptionStream<T> {
    id: uuid::Uuid,
    client: CorrClient,
    observed_eoq: bool,
    last_change_id: Option<ChangeId>,
    stream: Option<FramedBody>,
    backoff: Option<Pin<Box<Sleep>>>,
    backoff_count: u32,
    #[allow(clippy::type_complexity)]
    response: Option<Pin<Box<dyn Future<Output = Result<Incoming, ClientError>> + Send>>>,
    _deser: std::marker::PhantomData<T>,
}

#[derive(Debug, thiserror::Error)]
pub enum SubscriptionError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Deserialize(#[from] serde_json::Error),
    #[error("missed a change (expected: {expected}, got: {got}), inconsistent state")]
    MissedChange { expected: ChangeId, got: ChangeId },
    #[error("max line length exceeded")]
    MaxLineLengthExceeded,
    #[error("initial query never finished")]
    UnfinishedQuery,
    #[error("max retry attempts exceeded")]
    MaxRetryAttempts,
}

impl<T: DeserializeOwned + Unpin> SubscriptionStream<T> {
    fn new(
        id: uuid::Uuid,
        client: CorrClient,
        body: Incoming,
        change_id: Option<ChangeId>,
    ) -> Self {
        Self {
            id,
            client,
            observed_eoq: change_id.is_some(),
            last_change_id: change_id,
            stream: Some(framed_body(body)),
            backoff: None,
            backoff_count: 0,
            response: None,
            _deser: std::marker::PhantomData,
        }
    }

    fn poll_stream(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TypedQueryEvent<T>, SubscriptionError>>> {
        let stream = loop {
            match self.stream.as_mut() {
                None => match ready!(self.as_mut().poll_reconnect(cx)) {
                    Ok(stream) => {
                        self.stream = Some(stream);
                    }
                    Err(e) => return Poll::Ready(Some(Err(e))),
                },
                Some(stream) => break stream,
            }
        };

        let res = ready!(Pin::new(stream).poll_next(cx));
        match res {
            Some(Ok(b)) => match serde_json::from_slice(&b) {
                Ok(evt) => {
                    if let TypedQueryEvent::EndOfQuery { change_id, .. } = &evt {
                        self.handle_eoq(*change_id);
                    }
                    if let TypedQueryEvent::Change(_, _, _, change_id) = &evt
                        && let Err(e) = self.handle_change(*change_id)
                    {
                        return Poll::Ready(Some(Err(e)));
                    }
                    Poll::Ready(Some(Ok(evt)))
                }
                Err(deser_err) => {
                    // Try untyped variant to extract metadata
                    if let Ok(evt) = serde_json::from_slice::<QueryEvent>(&b) {
                        if let TypedQueryEvent::EndOfQuery { change_id, .. } = &evt {
                            self.handle_eoq(*change_id);
                        }
                        if let TypedQueryEvent::Change(_, _, _, change_id) = &evt {
                            let _ = self.handle_change(*change_id);
                        }
                    }
                    Poll::Ready(Some(Err(deser_err.into())))
                }
            },
            Some(Err(e)) => match e {
                LinesCodecError::MaxLineLengthExceeded => {
                    Poll::Ready(Some(Err(SubscriptionError::MaxLineLengthExceeded)))
                }
                LinesCodecError::Io(io_err) => Poll::Ready(Some(Err(io_err.into()))),
            },
            None => Poll::Ready(None),
        }
    }

    fn handle_eoq(&mut self, change_id: Option<ChangeId>) {
        self.observed_eoq = true;
        self.last_change_id = change_id;
    }

    fn handle_change(&mut self, change_id: ChangeId) -> Result<(), SubscriptionError> {
        if let Some(id) = self.last_change_id
            && id + 1 != change_id
        {
            return Err(SubscriptionError::MissedChange {
                expected: id + 1,
                got: change_id,
            });
        }
        self.last_change_id = Some(change_id);
        Ok(())
    }

    fn poll_reconnect(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<FramedBody, SubscriptionError>> {
        loop {
            if let Some(res_fut) = self.response.as_mut() {
                let res = ready!(res_fut.as_mut().poll(cx));
                self.response = None;
                return match res {
                    Ok(body) => Poll::Ready(Ok(framed_body(body))),
                    Err(e) => {
                        let io_err = io::Error::other(e.to_string());
                        Poll::Ready(Err(io_err.into()))
                    }
                };
            } else if self.observed_eoq {
                let client = self.client.clone();
                let id = self.id;
                let from = self.last_change_id;
                self.response = Some(Box::pin(async move { client.resubscribe(id, from).await }));
                // loop around
            } else {
                return Poll::Ready(Err(SubscriptionError::UnfinishedQuery));
            }
        }
    }
}

impl<T: DeserializeOwned + Unpin> Stream for SubscriptionStream<T> {
    type Item = Result<TypedQueryEvent<T>, SubscriptionError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Check backoff
        if let Some(backoff) = self.backoff.as_mut() {
            ready!(backoff.as_mut().poll(cx));
            self.backoff = None;
        }

        let io_err = match ready!(self.as_mut().poll_stream(cx)) {
            Some(Err(SubscriptionError::Io(io_err))) => io_err,
            other => {
                self.backoff_count = 0;
                return Poll::Ready(other);
            }
        };

        // Reset stream for reconnection
        self.stream = None;

        if self.backoff_count >= 10 {
            return Poll::Ready(Some(Err(SubscriptionError::MaxRetryAttempts)));
        }

        error!("subscription stream IO error: {io_err}, retrying in 1s");

        let mut backoff = Box::pin(sleep(Duration::from_secs(1)));
        // Register with waker
        let _ = backoff.as_mut().poll(cx);
        self.backoff = Some(backoff);
        self.backoff_count += 1;

        Poll::Pending
    }
}

// ---------------------------------------------------------------------------
// LinesBytesCodec — ported from corro_client
// ---------------------------------------------------------------------------

struct LinesBytesCodec {
    next_index: usize,
    max_length: usize,
    is_discarding: bool,
}

impl Default for LinesBytesCodec {
    fn default() -> Self {
        Self {
            next_index: 0,
            max_length: usize::MAX,
            is_discarding: false,
        }
    }
}

impl Decoder for LinesBytesCodec {
    type Item = BytesMut;
    type Error = LinesCodecError;

    #[allow(clippy::indexing_slicing)]
    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<BytesMut>, LinesCodecError> {
        loop {
            let read_to = std::cmp::min(self.max_length.saturating_add(1), buf.len());
            let newline_offset = buf[self.next_index..read_to]
                .iter()
                .position(|b| *b == b'\n');

            match (self.is_discarding, newline_offset) {
                (true, Some(offset)) => {
                    buf.advance(offset + self.next_index + 1);
                    self.is_discarding = false;
                    self.next_index = 0;
                }
                (true, None) => {
                    buf.advance(read_to);
                    self.next_index = 0;
                    if buf.is_empty() {
                        return Ok(None);
                    }
                }
                (false, Some(offset)) => {
                    let newline_index = offset + self.next_index;
                    self.next_index = 0;
                    let mut line = buf.split_to(newline_index + 1);
                    line.truncate(line.len() - 1);
                    if let Some(&b'\r') = line.last() {
                        line.truncate(line.len() - 1);
                    }
                    return Ok(Some(line));
                }
                (false, None) if buf.len() > self.max_length => {
                    self.is_discarding = true;
                    return Err(LinesCodecError::MaxLineLengthExceeded);
                }
                (false, None) => {
                    self.next_index = read_to;
                    return Ok(None);
                }
            }
        }
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<BytesMut>, LinesCodecError> {
        Ok(match self.decode(buf)? {
            Some(frame) => Some(frame),
            None => {
                if buf.is_empty() || buf == &b"\r"[..] {
                    None
                } else {
                    let mut line = buf.split_to(buf.len());
                    line.truncate(line.len() - 1);
                    if let Some(&b'\r') = line.last() {
                        line.truncate(line.len() - 1);
                    }
                    self.next_index = 0;
                    Some(line)
                }
            }
        })
    }
}
