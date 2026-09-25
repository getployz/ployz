//! Build Grants: Machine-minted, in-memory permission for one image push into this
//! Machine's image ingest. A grant key is admitted only on [`BUILD_GRANT_ALPN`], whose
//! connections reach a filtering registry proxy and never the Machine API, so the grant
//! is not a Management Capability (a Machine-local safety boundary, DESIGN bet 10).
//!
//! A grant allows the OCI Distribution calls one `docker push` makes into its one
//! repository, and ends after one tagged manifest lands (or the ingest confirms it
//! already holds that tag at its digest), when its Build ends
//! ([`BuildGrants::end`]), after [`GRANT_LIFETIME`], or when the daemon stops.
//! The Machine RPC handlers only forward to [`mint_for`] and [`BuildGrants::end`].

use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::body::Body;
use bytes::Bytes;
use http::{HeaderMap, Method, Request, Response, StatusCode, header};
use http_body_util::BodyExt as _;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use iroh::{
    SecretKey,
    endpoint::{Connection, VarInt},
};
use ployz_core::{
    BUILD_GRANT_ENDED, BUILD_GRANT_REFUSED, BuildGrant, BuildGrantEnded, BuildGrantId,
    BuildGrantMinted, BuildGrantRepository, ImageDigest, ManagementIdentity, MintBuildGrantRequest,
    RETAINED_DIGEST_TAG_PREFIX, RpcError, RpcErrorCode,
};
use sha2::{Digest as _, Sha256};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// How long a grant lives when its Build never ends it. Ployz Cloud mints it at
/// check-in and gives the run 2h from there (`GITHUB_RUN_BUDGET_MS` in the dashboard's
/// `github-image-builds.server.ts`), so this must outlive that budget: a run Cloud
/// still waits on never loses its grant mid-push.
pub const GRANT_LIFETIME: Duration = Duration::from_secs(3 * 60 * 60);

/// Largest manifest a grant push may write; the OCI Distribution limit.
const MANIFEST_LIMIT: usize = 4 * 1024 * 1024;

/// Live grants, keyed by their public key. Never persisted or replicated.
#[derive(Default)]
pub struct BuildGrants {
    grants: Mutex<HashMap<BuildGrantId, Arc<Grant>>>,
}

struct Grant {
    repository: BuildGrantRepository,
    ingest: SocketAddr,
    expires: Instant,
    ended: CancellationToken,
    push: Mutex<Push>,
}

enum Push {
    Open,
    /// A tagged manifest is in flight; a second one is refused.
    Pushing,
    /// The digest this Machine verified and stored.
    Pushed(ImageDigest),
}

impl BuildGrants {
    /// Mint a grant for one push into `repository` through the ingest at `ingest`.
    pub fn mint(
        &self,
        machine: ManagementIdentity,
        repository: BuildGrantRepository,
        ingest: SocketAddr,
    ) -> BuildGrantMinted {
        let secret = SecretKey::generate();
        let id = grant_id(secret.public().as_bytes());
        let grant = Arc::new(Grant {
            repository,
            ingest,
            expires: Instant::now() + GRANT_LIFETIME,
            ended: CancellationToken::new(),
            push: Mutex::new(Push::Open),
        });
        let mut grants = self.grants.lock().expect("grant registry lock");
        grants.retain(|_, grant| grant.expires > Instant::now());
        grants.insert(id, grant);
        BuildGrantMinted {
            id,
            grant: BuildGrant::new(machine, secret.to_bytes()),
            expires_in_seconds: GRANT_LIFETIME.as_secs(),
        }
    }

    /// End a grant, closing its connections, and report what it pushed. Ending again
    /// repeats the report, so a retried call loses nothing.
    ///
    /// # Errors
    /// `NotFound` when this Machine holds no such grant: it expired or the daemon restarted.
    pub fn end(&self, id: &BuildGrantId) -> Result<BuildGrantEnded, RpcError> {
        let grant = Arc::clone(
            self.grants
                .lock()
                .expect("grant registry lock")
                .get(id)
                .ok_or_else(|| RpcError {
                    code: RpcErrorCode::NotFound,
                    message:
                        "this Machine holds no such Build Grant; it expired or the daemon restarted"
                            .into(),
                    details: serde_json::Value::Null,
                })?,
        );
        grant.ended.cancel();
        let pushed = match &*grant.push.lock().expect("grant push lock") {
            Push::Pushed(digest) => Some(digest.clone()),
            Push::Open | Push::Pushing => None,
        };
        Ok(BuildGrantEnded { pushed })
    }

    /// The live, unused grant `key` holds.
    fn admit(&self, key: &[u8; 32]) -> Option<Arc<Grant>> {
        let grant = Arc::clone(
            self.grants
                .lock()
                .expect("grant registry lock")
                .get(&grant_id(key))?,
        );
        (grant.expires > Instant::now() && !grant.ended.is_cancelled() && !grant.pushed())
            .then_some(grant)
    }
}

/// Mint for a Machine RPC request, opening image ingest first so its failure
/// reaches the minting caller rather than the pusher.
pub(crate) async fn mint_for(
    grants: &BuildGrants,
    local: &crate::machine::LocalMachine,
    ingest: Arc<crate::docker::ImageIngest>,
    request: MintBuildGrantRequest,
) -> Result<BuildGrantMinted, crate::machine::LocalMachineError> {
    let opened = local.ensure_image_ingest(ingest).await?.destination;
    Ok(grants.mint(
        local.record().management_secret().public_key(),
        request.repository,
        SocketAddr::from((opened.management_address.0, opened.port)),
    ))
}

fn grant_id(key: &[u8; 32]) -> BuildGrantId {
    BuildGrantId::parse(hex::encode(key)).expect("hex of 32 bytes is a Build Grant ID")
}

impl Grant {
    fn pushed(&self) -> bool {
        matches!(*self.push.lock().expect("grant push lock"), Push::Pushed(_))
    }
}

/// Serve one connection on the grant ALPN until its grant ends. Each bidirectional
/// stream carries one HTTP/1.1 connection of the pusher's registry client.
pub(super) async fn serve(
    connection: Connection,
    grants: &BuildGrants,
    shutdown: CancellationToken,
) {
    let Some(grant) = grants.admit(connection.remote_id().as_bytes()) else {
        connection.close(
            VarInt::from_u32(BUILD_GRANT_REFUSED),
            b"build grant refused",
        );
        return;
    };
    let client = reqwest::Client::new();
    let mut streams = tokio::task::JoinSet::new();
    loop {
        let (send, recv) = tokio::select! {
            () = grant.ended.cancelled() => break,
            () = tokio::time::sleep_until(grant.expires) => break,
            () = shutdown.cancelled() => break,
            _ = streams.join_next(), if !streams.is_empty() => continue,
            accepted = connection.accept_bi() => match accepted {
                Ok(streams) => streams,
                Err(_) => return,
            },
        };
        let grant = Arc::clone(&grant);
        let client = client.clone();
        streams.spawn(async move {
            let service = service_fn(move |request| {
                let grant = Arc::clone(&grant);
                let client = client.clone();
                async move { Ok::<_, Infallible>(handle(request, &grant, &client).await) }
            });
            if let Err(error) = http1::Builder::new()
                .serve_connection(TokioIo::new(tokio::io::join(recv, send)), service)
                .await
            {
                tracing::debug!(%error, "build grant stream ended");
            }
        });
    }
    // Dropping the set aborts in-flight requests: an ended grant pushes nothing more.
    drop(streams);
    connection.close(VarInt::from_u32(BUILD_GRANT_ENDED), b"build grant ended");
}

/// One registry call a grant allows, in its one repository.
#[derive(Debug, Eq, PartialEq)]
enum Route {
    Ping,
    Blob,
    Upload,
    ManifestHead,
    /// A HEAD of the one tag a push may write, which the pusher sends first and
    /// stops at when the Machine already holds the image.
    ManifestTagHead(ImageDigest),
    /// A manifest pushed by digest, such as an index's platform manifest.
    ManifestByDigest(ImageDigest),
    /// The one tagged manifest; its tag names its own digest.
    ManifestTag(ImageDigest),
}

impl Route {
    fn parse(
        method: &Method,
        path: &str,
        query: Option<&str>,
        repository: &BuildGrantRepository,
    ) -> Option<Self> {
        if path == "/v2/" {
            return matches!(*method, Method::GET | Method::HEAD).then_some(Self::Ping);
        }
        let rest = path
            .strip_prefix("/v2/")?
            .strip_prefix(repository.as_str())?
            .strip_prefix('/')?;
        if let Some(digest) = rest.strip_prefix("blobs/") {
            return match (method, digest.strip_prefix("uploads/")) {
                (&Method::HEAD, None) => ImageDigest::parse(digest).is_ok().then_some(Self::Blob),
                // A cross-repository mount would read another repository's blob.
                (&Method::POST, Some(""))
                    if !query.is_some_and(|query| query.contains("mount=")) =>
                {
                    Some(Self::Upload)
                }
                (&Method::GET | &Method::PATCH | &Method::PUT, Some(id))
                    if !id.is_empty() && !id.contains('/') =>
                {
                    Some(Self::Upload)
                }
                _ => None,
            };
        }
        let reference = rest.strip_prefix("manifests/")?;
        match *method {
            Method::HEAD => {
                Some(tagged(reference).map_or(Self::ManifestHead, Self::ManifestTagHead))
            }
            Method::PUT => match reference.strip_prefix(RETAINED_DIGEST_TAG_PREFIX) {
                Some(_) => tagged(reference).map(Self::ManifestTag),
                None => ImageDigest::parse(reference)
                    .ok()
                    .map(Self::ManifestByDigest),
            },
            _ => None,
        }
    }
}

/// The digest a `ployz-sha256-<hex>` tag names.
fn tagged(reference: &str) -> Option<ImageDigest> {
    let hex = reference.strip_prefix(RETAINED_DIGEST_TAG_PREFIX)?;
    ImageDigest::parse(format!("sha256:{hex}")).ok()
}

async fn handle(
    request: Request<Incoming>,
    grant: &Grant,
    client: &reqwest::Client,
) -> Response<Body> {
    let uri = request.uri();
    let Some(route) = Route::parse(request.method(), uri.path(), uri.query(), &grant.repository)
    else {
        return denied(
            StatusCode::FORBIDDEN,
            "the Build Grant does not allow this request",
        );
    };
    if grant.pushed() {
        return denied(StatusCode::FORBIDDEN, "the Build Grant was already used");
    }
    let (parts, body) = request.into_parts();
    let (digest, tagged) = match route {
        Route::Ping | Route::Blob | Route::Upload | Route::ManifestHead => {
            let body = reqwest::Body::wrap_stream(Body::new(body).into_data_stream());
            return forward(client, grant.ingest, &parts, body).await;
        }
        Route::ManifestTagHead(digest) => {
            let response = forward(client, grant.ingest, &parts, reqwest::Body::from("")).await;
            // containerd's pusher HEADs the tag and never PUTs a manifest the Machine
            // already holds under it; the ingest confirming that digest is the push.
            let held = response.status() == StatusCode::OK
                && response
                    .headers()
                    .get("docker-content-digest")
                    .is_some_and(|value| value.as_bytes() == digest.as_str().as_bytes());
            let mut push = grant.push.lock().expect("grant push lock");
            if held && matches!(*push, Push::Open) {
                *push = Push::Pushed(digest);
            }
            return response;
        }
        Route::ManifestByDigest(digest) => (digest, false),
        Route::ManifestTag(digest) => (digest, true),
    };
    let Ok(manifest) = http_body_util::Limited::new(body, MANIFEST_LIMIT)
        .collect()
        .await
    else {
        return denied(
            StatusCode::BAD_REQUEST,
            "the manifest could not be read within its size limit",
        );
    };
    let manifest = manifest.to_bytes();
    if hex::encode(Sha256::digest(&manifest)) != digest.hex() {
        return denied(
            StatusCode::BAD_REQUEST,
            "the manifest does not match the digest it is pushed as",
        );
    }
    if !tagged {
        return forward(client, grant.ingest, &parts, manifest.into()).await;
    }
    {
        let mut push = grant.push.lock().expect("grant push lock");
        if !matches!(*push, Push::Open) {
            return denied(StatusCode::CONFLICT, "the Build Grant is already pushing");
        }
        *push = Push::Pushing;
    }
    let response = forward(client, grant.ingest, &parts, manifest.into()).await;
    // Only a stored manifest spends the grant; a refused one may be retried.
    *grant.push.lock().expect("grant push lock") = if response.status().is_success() {
        Push::Pushed(digest)
    } else {
        Push::Open
    };
    response
}

async fn forward(
    client: &reqwest::Client,
    ingest: SocketAddr,
    parts: &http::request::Parts,
    body: reqwest::Body,
) -> Response<Body> {
    let path = parts.uri.path_and_query().map_or("/", |path| path.as_str());
    let result = client
        .request(parts.method.clone(), format!("http://{ingest}{path}"))
        .headers(end_to_end(&parts.headers))
        .body(body)
        .send()
        .await;
    let upstream = match result {
        Ok(upstream) => upstream,
        Err(error) => {
            tracing::debug!(%error, "build grant ingest request failed");
            return denied(StatusCode::BAD_GATEWAY, "image ingest is unavailable");
        }
    };
    let mut response = Response::builder().status(upstream.status());
    let headers = response.headers_mut().expect("fresh response builder");
    *headers = end_to_end(upstream.headers());
    // Upload locations name the ingest address; the pusher resolves a path against
    // the registry it dialed instead.
    if let Some(location) = headers
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        && let Some(relative) = location
            .strip_prefix("http://")
            .and_then(|rest| rest.find('/').map(|slash| rest[slash..].to_owned()))
    {
        headers.insert(
            header::LOCATION,
            relative.parse().expect("a suffix of a valid header value"),
        );
    }
    response
        .body(Body::from_stream(upstream.bytes_stream()))
        .expect("status and headers came from a valid response")
}

fn end_to_end(headers: &HeaderMap) -> HeaderMap {
    let mut headers = headers.clone();
    for name in [
        header::HOST,
        header::CONNECTION,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
    ] {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers
}

fn denied(status: StatusCode, message: &str) -> Response<Body> {
    let body = serde_json::json!({"errors": [{"code": "DENIED", "message": message}]});
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(Bytes::from(body.to_string())))
        .expect("static response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_allows_only_push_calls_into_its_repository() {
        let digest = "a".repeat(64);
        let pushed = ImageDigest::parse(format!("sha256:{digest}")).unwrap();
        let repository = BuildGrantRepository::parse("ployz-build/web").unwrap();
        let route = |method: Method, path: &str, query: Option<&str>| {
            Route::parse(&method, path, query, &repository)
        };
        assert_eq!(route(Method::GET, "/v2/", None), Some(Route::Ping));
        assert_eq!(
            route(
                Method::HEAD,
                &format!("/v2/ployz-build/web/blobs/sha256:{digest}"),
                None
            ),
            Some(Route::Blob)
        );
        assert_eq!(
            route(Method::POST, "/v2/ployz-build/web/blobs/uploads/", None),
            Some(Route::Upload)
        );
        assert_eq!(
            route(Method::PATCH, "/v2/ployz-build/web/blobs/uploads/u1", None),
            Some(Route::Upload)
        );
        assert_eq!(
            route(
                Method::PUT,
                &format!("/v2/ployz-build/web/manifests/ployz-sha256-{digest}"),
                None
            ),
            Some(Route::ManifestTag(pushed.clone()))
        );
        assert_eq!(
            route(
                Method::HEAD,
                &format!("/v2/ployz-build/web/manifests/ployz-sha256-{digest}"),
                None
            ),
            Some(Route::ManifestTagHead(pushed.clone()))
        );
        assert_eq!(
            route(Method::HEAD, "/v2/ployz-build/web/manifests/latest", None),
            Some(Route::ManifestHead)
        );
        assert_eq!(
            route(
                Method::PUT,
                &format!("/v2/ployz-build/web/manifests/sha256:{digest}"),
                None
            ),
            Some(Route::ManifestByDigest(pushed.clone()))
        );
        for (method, path, query) in [
            // Reading content back, or any other repository, is not a push.
            (
                Method::GET,
                format!("/v2/ployz-build/web/blobs/sha256:{digest}"),
                None,
            ),
            (
                Method::GET,
                format!("/v2/ployz-build/web/manifests/sha256:{digest}"),
                None,
            ),
            (
                Method::HEAD,
                format!("/v2/ployz-build/api/blobs/sha256:{digest}"),
                None,
            ),
            (
                Method::HEAD,
                format!("/v2/ployz-build/web-x/blobs/sha256:{digest}"),
                None,
            ),
            (Method::GET, "/v2/_catalog".into(), None),
            (Method::GET, "/v2/ployz-build/web/tags/list".into(), None),
            (
                Method::DELETE,
                format!("/v2/ployz-build/web/manifests/sha256:{digest}"),
                None,
            ),
            (
                Method::POST,
                "/v2/ployz-build/web/blobs/uploads/".into(),
                Some("mount=sha256:x&from=other"),
            ),
            // A tag must name its own digest, so it cannot move another image.
            (
                Method::PUT,
                "/v2/ployz-build/web/manifests/latest".into(),
                None,
            ),
            (
                Method::PUT,
                "/v2/ployz-build/web/manifests/ployz-sha256-short".into(),
                None,
            ),
        ] {
            assert_eq!(route(method.clone(), &path, query), None, "{method} {path}");
        }
    }
}
