//! The HTTP/1.1 agent link: the plane's listener and the agent's client. Plain HTTP, because
//! both ends sit on the platform's private network; a shared bearer token keeps strangers out.

use std::convert::Infallible;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use uuid::Uuid;

use super::wire::{PullRequest, PullResponse, ResultsRequest, WireJob, WireObservation};
use super::{ControlLink, LinkError, PULL_WAIT, Pulled, Rejected, ResultBatch};
use crate::config::Secret;
use crate::domain::{Region, UnixMs};
use crate::run::{Fatal, Shutdown};

const PULL_PATH: &str = "/v1/agent/pull";
const RESULTS_PATH: &str = "/v1/agent/results";
const READY_PATH: &str = "/ready";
pub const REGION_HEADER: HeaderName = HeaderName::from_static("ployz-probe-region");
const JSON: HeaderValue = HeaderValue::from_static("application/json");

/// Jobs one pull may carry at most, whatever the agent asks for.
pub const PULL_CAP: u32 = 4096;
/// Observations one results request may carry at most.
pub const RESULTS_CAP: usize = 5000;
const PULL_BODY_LIMIT: usize = 64 * 1024;
const RESULTS_BODY_LIMIT: usize = 8 * 1024 * 1024;
const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;
/// The server holds a pull for [`PULL_WAIT`]; the client allows that plus slack.
const PULL_SLACK: Duration = Duration::from_secs(5);
const REPORT_TIMEOUT: Duration = Duration::from_secs(15);

/// What the listener asks of a caller before it reaches the plane.
#[derive(Clone, Debug)]
pub struct Gate {
    pub token: Secret,
    pub regions: Vec<Region>,
}

/// What the listener needs from the control plane. [`ControlPlane`](crate::control::plane) is
/// the production implementation; link tests use a fake.
pub trait PlaneApi: Send + Sync + 'static {
    fn pull(
        &self,
        region: &Region,
        want: u32,
        wait: Duration,
    ) -> impl Future<Output = Pulled> + Send;
    fn report(
        &self,
        region: &Region,
        batch: ResultBatch,
    ) -> impl Future<Output = Result<(), Rejected>> + Send;
    fn ready(&self) -> bool;
}

/// Serves until `shutdown` fires, then stops accepting and lets open requests finish. With no
/// gate it answers only `/ready`: `all` mode's agent is in-process, so nothing remote may pull.
pub async fn serve<P: PlaneApi>(
    listener: TcpListener,
    plane: Arc<P>,
    gate: Option<Gate>,
    shutdown: Shutdown,
) {
    let gate = Arc::new(gate);
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    conns.spawn(connection(stream, plane.clone(), gate.clone(), shutdown.clone()));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            () = shutdown.wait() => break,
        }
    }
    drop(listener);
    while conns.join_next().await.is_some() {}
}

async fn connection<P: PlaneApi>(
    stream: tokio::net::TcpStream,
    plane: Arc<P>,
    gate: Arc<Option<Gate>>,
    shutdown: Shutdown,
) {
    let service = service_fn(move |req| {
        let plane = plane.clone();
        let gate = gate.clone();
        async move { Ok::<_, Infallible>(handle(req, &*plane, gate.as_ref().as_ref()).await) }
    });
    let mut conn = pin!(http1::Builder::new().serve_connection(TokioIo::new(stream), service));
    tokio::select! {
        done = &mut conn => log_conn(done),
        () = shutdown.wait() => {
            conn.as_mut().graceful_shutdown();
            log_conn(conn.await);
        }
    }
}

fn log_conn(done: Result<(), hyper::Error>) {
    if let Err(e) = done
        && !e.is_incomplete_message()
    {
        tracing::debug!(error = %e, "agent connection ended with an error");
    }
}

async fn handle<P: PlaneApi>(
    req: Request<Incoming>,
    plane: &P,
    gate: Option<&Gate>,
) -> Response<Full<Bytes>> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, READY_PATH) => {
            if plane.ready() {
                reply(StatusCode::OK, "{\"ready\":true}")
            } else {
                reply(StatusCode::SERVICE_UNAVAILABLE, "{\"ready\":false}")
            }
        }
        (&Method::POST, PULL_PATH) if let Some(gate) = gate => match admit(&req, gate) {
            Ok(region) => pull(req, plane, &region).await,
            Err((status, body)) => reply(status, body),
        },
        (&Method::POST, RESULTS_PATH) if let Some(gate) = gate => match admit(&req, gate) {
            Ok(region) => results(req, plane, &region).await,
            Err((status, body)) => reply(status, body),
        },
        _ => reply(StatusCode::NOT_FOUND, "{\"error\":\"no such route\"}"),
    }
}

type Refused = (StatusCode, &'static str);

/// 401 before 403: a caller without the token learns nothing about the region list.
fn admit(req: &Request<Incoming>, gate: &Gate) -> Result<Region, Refused> {
    let presented = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.as_bytes().strip_prefix(b"Bearer "));
    if !presented.is_some_and(|p| gate.token.matches(p)) {
        return Err((StatusCode::UNAUTHORIZED, "{\"error\":\"unauthorized\"}"));
    }
    let region = req
        .headers()
        .get(REGION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|name| Region::parse(name).ok())
        .filter(|region| gate.regions.contains(region));
    region.ok_or((StatusCode::FORBIDDEN, "{\"error\":\"unknown region\"}"))
}

async fn pull<P: PlaneApi>(
    req: Request<Incoming>,
    plane: &P,
    region: &Region,
) -> Response<Full<Bytes>> {
    let body = match read(req, PULL_BODY_LIMIT).await {
        Ok(body) => body,
        Err((status, body)) => return reply(status, body),
    };
    let want = match serde_json::from_slice::<PullRequest>(&body) {
        Ok(ask) => ask.want.min(PULL_CAP),
        Err(_) => return reply(StatusCode::BAD_REQUEST, "{\"error\":\"malformed pull\"}"),
    };
    let pulled = plane.pull(region, want, PULL_WAIT).await;
    let response = PullResponse {
        jobs: pulled.jobs.iter().map(WireJob::of).collect(),
        server_now: pulled.server_now.0,
    };
    match serde_json::to_vec(&response) {
        Ok(json) => reply(StatusCode::OK, json),
        Err(e) => {
            tracing::error!(error = %e, "pull response failed to serialize");
            reply(
                StatusCode::INTERNAL_SERVER_ERROR,
                "{\"error\":\"serialize\"}",
            )
        }
    }
}

async fn results<P: PlaneApi>(
    req: Request<Incoming>,
    plane: &P,
    region: &Region,
) -> Response<Full<Bytes>> {
    let body = match read(req, RESULTS_BODY_LIMIT).await {
        Ok(body) => body,
        Err((status, body)) => return reply(status, body),
    };
    let Ok(request) = serde_json::from_slice::<ResultsRequest>(&body) else {
        return reply(StatusCode::BAD_REQUEST, "{\"error\":\"malformed results\"}");
    };
    let Ok(id) = request.batch_id.parse::<Uuid>() else {
        return reply(
            StatusCode::BAD_REQUEST,
            "{\"error\":\"malformed batch_id\"}",
        );
    };
    if request.results.len() > RESULTS_CAP {
        return reply(
            StatusCode::PAYLOAD_TOO_LARGE,
            "{\"error\":\"too many results\"}",
        );
    }
    let total = request.results.len();
    let observations: Vec<_> = request
        .results
        .into_iter()
        .filter_map(|wire| wire.into_observation().ok())
        .collect();
    if observations.len() < total {
        tracing::debug!(
            region = region.as_str(),
            dropped = total - observations.len(),
            "malformed observations dropped"
        );
    }
    match plane.report(region, ResultBatch { id, observations }).await {
        Ok(()) => reply(StatusCode::OK, "{}"),
        Err(Rejected::NotReady) => {
            reply(StatusCode::SERVICE_UNAVAILABLE, "{\"error\":\"not ready\"}")
        }
        Err(Rejected::Status(status)) => reply(
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
            "{\"error\":\"rejected\"}",
        ),
    }
}

async fn read(req: Request<Incoming>, limit: usize) -> Result<Bytes, Refused> {
    match Limited::new(req.into_body(), limit).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e)
            if e.downcast_ref::<http_body_util::LengthLimitError>()
                .is_some() =>
        {
            Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "{\"error\":\"body too large\"}",
            ))
        }
        Err(_) => Err((StatusCode::BAD_REQUEST, "{\"error\":\"unreadable body\"}")),
    }
}

fn reply(status: StatusCode, body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(body.into()));
    *response.status_mut() = status;
    response.headers_mut().insert(CONTENT_TYPE, JSON);
    response
}

/// The agent's side. Keeps one connection pool to the plane and counts what it had to drop.
pub struct HttpLink {
    client: Client<HttpConnector, Full<Bytes>>,
    pull_uri: Uri,
    results_uri: Uri,
    bearer: Option<HeaderValue>,
    region: HeaderValue,
    malformed_jobs: AtomicU64,
}

impl HttpLink {
    /// # Errors
    /// The URL is not `http://` or not a URL, or the token holds bytes a header can't carry.
    pub fn new(
        control_url: &str,
        token: Option<Secret>,
        region: Region,
    ) -> Result<HttpLink, Fatal> {
        let base = control_url.trim_end_matches('/');
        if !base.starts_with("http://") {
            return Err(Fatal(
                "UPTIME_CONTROL_URL must start with http://.".to_owned(),
            ));
        }
        let uri = |path: &str| {
            format!("{base}{path}")
                .parse::<Uri>()
                .map_err(|_| Fatal("UPTIME_CONTROL_URL is not a valid URL.".to_owned()))
        };
        let bearer = match token {
            None => None,
            Some(token) => {
                let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
                    .map_err(|_| {
                        Fatal(
                            "UPTIME_AGENT_TOKEN holds characters a header can't carry.".to_owned(),
                        )
                    })?;
                value.set_sensitive(true);
                Some(value)
            }
        };
        let region = HeaderValue::from_str(region.as_str())
            .map_err(|_| Fatal("REGION holds characters a header can't carry.".to_owned()))?;
        Ok(HttpLink {
            client: Client::builder(TokioExecutor::new()).build_http(),
            pull_uri: uri(PULL_PATH)?,
            results_uri: uri(RESULTS_PATH)?,
            bearer,
            region,
            malformed_jobs: AtomicU64::new(0),
        })
    }

    /// Jobs the plane sent that this build could not parse, since start.
    #[must_use]
    pub fn malformed_jobs(&self) -> u64 {
        self.malformed_jobs.load(Ordering::Relaxed)
    }

    fn request(&self, uri: &Uri, json: Vec<u8>) -> Request<Full<Bytes>> {
        let mut req = Request::new(Full::new(Bytes::from(json)));
        *req.method_mut() = Method::POST;
        *req.uri_mut() = uri.clone();
        req.headers_mut().insert(CONTENT_TYPE, JSON);
        req.headers_mut().insert(REGION_HEADER, self.region.clone());
        if let Some(bearer) = &self.bearer {
            req.headers_mut().insert(AUTHORIZATION, bearer.clone());
        }
        req
    }

    async fn post(
        &self,
        uri: &Uri,
        json: Vec<u8>,
        timeout: Duration,
    ) -> Result<(StatusCode, Bytes), LinkError> {
        let exchange = async {
            let response = self
                .client
                .request(self.request(uri, json))
                .await
                .map_err(|e| LinkError::Broken(format!("request failed: {e}")))?;
            let status = response.status();
            let body = Limited::new(response.into_body(), RESPONSE_LIMIT)
                .collect()
                .await
                .map_err(|e| LinkError::Broken(format!("response body unreadable: {e}")))?
                .to_bytes();
            Ok((status, body))
        };
        tokio::time::timeout(timeout, exchange)
            .await
            .map_err(|_| LinkError::Broken("request timed out".to_owned()))?
    }
}

impl ControlLink for HttpLink {
    async fn pull(&self, want: u32) -> Result<Pulled, LinkError> {
        let json = serde_json::to_vec(&PullRequest { want })
            .map_err(|e| LinkError::Broken(format!("pull request failed to serialize: {e}")))?;
        let (status, body) = self
            .post(&self.pull_uri, json, PULL_WAIT + PULL_SLACK)
            .await?;
        if status != StatusCode::OK {
            return Err(LinkError::Broken(format!("pull answered HTTP {status}")));
        }
        let response = serde_json::from_slice::<PullResponse>(&body)
            .map_err(|_| LinkError::Broken("pull answered malformed JSON".to_owned()))?;
        let total = response.jobs.len();
        let jobs: Vec<_> = response
            .jobs
            .into_iter()
            .filter_map(|wire| wire.into_job().ok())
            .collect();
        if jobs.len() < total {
            let dropped = u64::try_from(total - jobs.len()).unwrap_or(u64::MAX);
            self.malformed_jobs.fetch_add(dropped, Ordering::Relaxed);
            tracing::warn!(dropped, "malformed jobs dropped from a pull");
        }
        Ok(Pulled {
            jobs,
            server_now: UnixMs(response.server_now),
        })
    }

    async fn report(&self, batch: &ResultBatch) -> Result<(), LinkError> {
        let request = ResultsRequest {
            batch_id: batch.id.to_string(),
            results: batch.observations.iter().map(WireObservation::of).collect(),
        };
        let json = serde_json::to_vec(&request)
            .map_err(|e| LinkError::Broken(format!("results failed to serialize: {e}")))?;
        let (status, _) = self.post(&self.results_uri, json, REPORT_TIMEOUT).await?;
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::SERVICE_UNAVAILABLE => Err(LinkError::Rejected(Rejected::NotReady)),
            StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE => {
                Err(LinkError::Rejected(Rejected::Status(status.as_u16())))
            }
            other => Err(LinkError::Broken(format!("results answered HTTP {other}"))),
        }
    }
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;
