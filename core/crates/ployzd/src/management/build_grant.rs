//! Build Grants: Machine-minted, in-memory permission for one image push into this
//! Machine's image ingest. A grant key is admitted only on [`BUILD_GRANT_ALPN`], whose
//! connections reach a filtering registry proxy and never the Machine API, so the grant
//! is not a Management Capability (a Machine-local safety boundary, DESIGN bet 10).
//!
//! A grant allows the OCI Distribution calls one `docker push` makes into its one
//! repository, and ends after one tagged manifest lands (or the ingest confirms it
//! already holds that tag at its digest), after [`GRANT_LIFETIME`], or when the daemon
//! stops. The Machine RPC handlers only forward to [`mint_for`] and
//! [`BuildGrants::inspect`].
//!
//! A grant minted for a holder can carry the built Service's Sealed values, still
//! sealed. Its holder reads them opened from `GET /ployz/sealed` once, before the push.

use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    path::{Path, PathBuf},
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
    PublicKey, SecretKey,
    endpoint::{Connection, VarInt},
};
use ployz_core::{
    BUILD_GRANT_ENDED, BUILD_GRANT_REFUSED, BuildGrant, BuildGrantId, BuildGrantInspected,
    BuildGrantMinted, BuildGrantRepository, ImageDigest, ManagementIdentity, MintBuildGrantRequest,
    MintedKey, RETAINED_DIGEST_TAG_PREFIX, RpcError, RpcErrorCode, SealedRecord,
};
use sha2::{Digest as _, Sha256};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::sealed_env::Unseal as _;

/// How long a grant lives. Ployz Cloud mints it at
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
    push: Mutex<Push>,
    /// Locked only under `push`.
    sealed: Mutex<Sealing>,
}

enum Sealing {
    None,
    Sealed(Sealed),
    /// Its holder read them; nothing more is served.
    Served,
}

/// A record this Machine opened at mint, kept sealed until its holder reads it.
struct Sealed {
    record: SealedRecord,
    key_dir: PathBuf,
}

enum Push {
    Open,
    /// A tagged manifest is in flight; a second one is refused.
    Pushing,
    /// The digest this Machine verified and stored.
    Pushed(ImageDigest),
}

/// A mint request this Machine accepts, not yet minted.
pub struct Mint {
    repository: BuildGrantRepository,
    key: Key,
    sealed: Option<Sealed>,
}

enum Key {
    /// This Machine made the key; the minted grant returns its secret.
    Made(SecretKey),
    /// The holder made it and keeps the secret.
    Holder(PublicKey),
}

impl Mint {
    /// Check `request` before anything is opened for it, and that its Sealed values
    /// open with the Sealing Key in `key_dir`.
    ///
    /// # Errors
    /// `InvalidArgument` when the holder is not a public key or Sealed values have no
    /// holder; `Conflict` when this Machine has no Sealing Key or they don't open here.
    pub fn check(request: MintBuildGrantRequest, key_dir: Option<&Path>) -> Result<Mint, RpcError> {
        let key = match request.holder {
            None => Key::Made(SecretKey::generate()),
            Some(holder) => Key::Holder(
                hex::decode(holder.as_str())
                    .ok()
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                    .and_then(|bytes| PublicKey::from_bytes(&bytes).ok())
                    .ok_or_else(|| {
                        refused(
                            RpcErrorCode::InvalidArgument,
                            "the Build Grant holder is not a public key".into(),
                        )
                    })?,
            ),
        };
        let sealed = match request.sealed {
            None => None,
            Some(_) if matches!(key, Key::Made(_)) => {
                return Err(refused(
                    RpcErrorCode::InvalidArgument,
                    "a Build Grant carrying Sealed values needs a holder".into(),
                ));
            }
            Some(record) => {
                let key_dir = key_dir.ok_or_else(|| {
                    refused(
                        RpcErrorCode::Conflict,
                        "this Machine has no Sealing Key to open the Build's Sealed values".into(),
                    )
                })?;
                record
                    .open_for_build(Some(key_dir))
                    .map_err(|error| refused(RpcErrorCode::Conflict, error.to_string()))?;
                Some(Sealed {
                    record,
                    key_dir: key_dir.to_owned(),
                })
            }
        };
        Ok(Mint {
            repository: request.repository,
            key,
            sealed,
        })
    }
}

impl BuildGrants {
    /// Mint a grant for one push into `mint`'s repository through the ingest at
    /// `ingest`, dialed at `machine`.
    ///
    /// # Errors
    /// `Conflict` when the holder already holds a live grant.
    pub fn mint(
        &self,
        machine: ManagementIdentity,
        mint: Mint,
        ingest: SocketAddr,
    ) -> Result<BuildGrantMinted, RpcError> {
        let (id, key) = match mint.key {
            Key::Made(secret) => (
                grant_id(secret.public().as_bytes()),
                MintedKey::Grant {
                    grant: BuildGrant::new(machine, secret.to_bytes()),
                },
            ),
            Key::Holder(holder) => (grant_id(holder.as_bytes()), MintedKey::Holder { machine }),
        };
        let grant = Arc::new(Grant {
            repository: mint.repository,
            ingest,
            expires: Instant::now() + GRANT_LIFETIME,
            push: Mutex::new(Push::Open),
            sealed: Mutex::new(mint.sealed.map_or(Sealing::None, Sealing::Sealed)),
        });
        let mut grants = self.grants.lock().expect("grant registry lock");
        grants.retain(|_, grant| grant.expires > Instant::now());
        if grants.contains_key(&id) {
            return Err(refused(
                RpcErrorCode::Conflict,
                "this holder already holds a live Build Grant".into(),
            ));
        }
        grants.insert(id, grant);
        Ok(BuildGrantMinted {
            id,
            key,
            expires_in_seconds: GRANT_LIFETIME.as_secs(),
        })
    }

    /// Report what a live grant pushed. Reading does not spend or end it.
    ///
    /// # Errors
    /// `NotFound` when this Machine holds no such grant: it expired or the daemon restarted.
    pub fn inspect(&self, id: &BuildGrantId) -> Result<BuildGrantInspected, RpcError> {
        let grant = Arc::clone(
            self.grants
                .lock()
                .expect("grant registry lock")
                .get(id)
                .filter(|grant| grant.expires > Instant::now())
                .ok_or_else(|| RpcError {
                    code: RpcErrorCode::NotFound,
                    message:
                        "this Machine holds no such Build Grant; it expired or the daemon restarted"
                            .into(),
                    details: serde_json::Value::Null,
                    cause: Vec::new(),
                })?,
        );
        let pushed = match &*grant.push.lock().expect("grant push lock") {
            Push::Pushed(digest) => Some(digest.clone()),
            Push::Open | Push::Pushing => None,
        };
        Ok(BuildGrantInspected {
            pushed,
            expires_in_seconds: grant
                .expires
                .saturating_duration_since(Instant::now())
                .as_secs(),
        })
    }

    /// The live, unused grant `key` holds.
    fn admit(&self, key: &[u8; 32]) -> Option<Arc<Grant>> {
        let grant = Arc::clone(
            self.grants
                .lock()
                .expect("grant registry lock")
                .get(&grant_id(key))?,
        );
        (grant.expires > Instant::now() && !grant.pushed()).then_some(grant)
    }
}

/// Mint for a Machine RPC request, opening image ingest first so its failure
/// reaches the minting caller rather than the pusher.
pub(crate) async fn mint_for(
    grants: &BuildGrants,
    local: &crate::machine::LocalMachine,
    ingest: Arc<crate::docker::ImageIngest>,
    mint: Mint,
) -> Result<Result<BuildGrantMinted, RpcError>, crate::machine::LocalMachineError> {
    let opened = local.ensure_image_ingest(ingest).await?.destination;
    Ok(grants.mint(
        local.record().management_secret().public_key(),
        mint,
        SocketAddr::from((opened.management_address.0, opened.port)),
    ))
}

const fn refused(code: RpcErrorCode, message: String) -> RpcError {
    RpcError {
        code,
        message,
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
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
                tracing::debug!(error = %ployz_core::error_chain::inline(&error), "build grant stream ended");
            }
        });
    }
    // Dropping the set aborts in-flight requests: an ended grant pushes nothing more.
    drop(streams);
    connection.close(VarInt::from_u32(BUILD_GRANT_ENDED), b"build grant ended");
}

/// One call a grant allows.
#[derive(Debug, Eq, PartialEq)]
enum Route {
    Sealed,
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
        if path == "/ployz/sealed" {
            return (*method == Method::GET).then_some(Self::Sealed);
        }
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
        Route::Sealed => return sealed(grant),
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

/// The grant's Sealed values, opened, for its holder once, while the grant is live and
/// nothing is pushing or pushed.
fn sealed(grant: &Grant) -> Response<Body> {
    let sealed = {
        let push = grant.push.lock().expect("grant push lock");
        if !matches!(*push, Push::Open) || grant.expires <= Instant::now() {
            return denied(StatusCode::FORBIDDEN, "the Build Grant has ended");
        }
        let mut sealing = grant.sealed.lock().expect("grant sealed lock");
        match std::mem::replace(&mut *sealing, Sealing::Served) {
            Sealing::Sealed(sealed) => sealed,
            Sealing::Served => {
                return denied(
                    StatusCode::FORBIDDEN,
                    "the Build Grant's Sealed values were already read",
                );
            }
            Sealing::None => {
                *sealing = Sealing::None;
                return denied(
                    StatusCode::NOT_FOUND,
                    "the Build Grant carries no Sealed values",
                );
            }
        }
    };
    match sealed.record.open_for_build(Some(&sealed.key_dir)) {
        Ok(opened) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from(
                serde_json::to_vec(&opened).expect("opened values encode"),
            ))
            .expect("static response"),
        Err(error) => denied(StatusCode::PRECONDITION_FAILED, &error.to_string()),
    }
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
            tracing::debug!(error = %ployz_core::error_chain::inline(&error), "build grant ingest request failed");
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
    use std::collections::BTreeMap;

    use ployz_core::{EnvValue, OpenedSealed, SealedScope};

    use super::*;
    use crate::sealed_env::test_support::Cluster;

    fn request(holder: Option<&str>) -> MintBuildGrantRequest {
        MintBuildGrantRequest {
            repository: BuildGrantRepository::parse("ployz-build/web").unwrap(),
            holder: holder.map(|holder| BuildGrantId::parse(holder).unwrap()),
            sealed: None,
        }
    }

    fn mint(grants: &BuildGrants, request: MintBuildGrantRequest) -> BuildGrantMinted {
        let machine = ManagementIdentity::from_bytes([7; 32]);
        let ingest = SocketAddr::from(([127, 0, 0, 1], 5000));
        grants
            .mint(machine, Mint::check(request, None).unwrap(), ingest)
            .unwrap()
    }

    const MARKER: &str = "marker-sealed-0123456789";

    fn holder() -> PublicKey {
        SecretKey::from_bytes(&[3; 32]).public()
    }

    fn sealed_request(cluster: &Cluster) -> MintBuildGrantRequest {
        let env = BTreeMap::from([(
            "NPM_TOKEN".to_owned(),
            EnvValue::Sealed(cluster.seal("NPM_TOKEN", SealedScope::Shared, MARKER)),
        )]);
        MintBuildGrantRequest {
            sealed: Some(cluster.open(env).unwrap().record.unwrap()),
            ..request(Some(&hex::encode(holder().as_bytes())))
        }
    }

    fn sealed_grant(cluster: &Cluster) -> Arc<Grant> {
        let grants = BuildGrants::default();
        let mint = Mint::check(sealed_request(cluster), Some(&cluster.dir.0)).unwrap();
        grants
            .mint(
                ManagementIdentity::from_bytes([7; 32]),
                mint,
                SocketAddr::from(([127, 0, 0, 1], 5000)),
            )
            .unwrap();
        grants.admit(holder().as_bytes()).unwrap()
    }

    async fn read(response: Response<Body>) -> (StatusCode, String) {
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn a_holder_reads_its_grants_sealed_values_opened() {
        let cluster = Cluster::new();
        let (status, body) = read(sealed(&sealed_grant(&cluster))).await;
        assert_eq!(status, StatusCode::OK);
        let opened: OpenedSealed = serde_json::from_str(&body).unwrap();
        assert_eq!(opened.get("NPM_TOKEN"), Some(MARKER));
    }

    #[tokio::test]
    async fn sealed_values_end_with_the_push_or_the_grant() {
        let cluster = Cluster::new();
        let pushing = sealed_grant(&cluster);
        *pushing.push.lock().unwrap() = Push::Pushing;
        let pushed = sealed_grant(&cluster);
        *pushed.push.lock().unwrap() =
            Push::Pushed(ImageDigest::parse(format!("sha256:{}", "a".repeat(64))).unwrap());
        let live = sealed_grant(&cluster);
        let expired = Grant {
            repository: live.repository.clone(),
            ingest: live.ingest,
            expires: Instant::now(),
            push: Mutex::new(Push::Open),
            sealed: Mutex::new(Sealing::Sealed(Sealed {
                record: sealed_request(&cluster).sealed.unwrap(),
                key_dir: cluster.dir.0.clone(),
            })),
        };
        for grant in [&*pushing, &*pushed, &expired] {
            let (status, body) = read(sealed(grant)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert!(!body.contains(MARKER));
        }
    }

    #[tokio::test]
    async fn a_holder_reads_sealed_values_once() {
        let cluster = Cluster::new();
        let grant = sealed_grant(&cluster);
        assert_eq!(read(sealed(&grant)).await.0, StatusCode::OK);
        let (status, body) = read(sealed(&grant)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(!body.contains(MARKER));
    }

    #[tokio::test]
    async fn sealed_values_that_fail_to_open_are_not_served_later() {
        let cluster = Cluster::new();
        let elsewhere = Cluster::new();
        let grant = sealed_grant(&cluster);
        *grant.sealed.lock().unwrap() = Sealing::Sealed(Sealed {
            record: sealed_request(&cluster).sealed.unwrap(),
            key_dir: elsewhere.dir.0.clone(),
        });
        assert_eq!(
            read(sealed(&grant)).await.0,
            StatusCode::PRECONDITION_FAILED
        );
        let (status, body) = read(sealed(&grant)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[test]
    fn a_holder_with_a_live_grant_mints_no_second() {
        let grants = BuildGrants::default();
        let holder = hex::encode(holder().as_bytes());
        let first = mint(&grants, request(Some(&holder)));
        let Err(refused) = grants.mint(
            ManagementIdentity::from_bytes([7; 32]),
            Mint::check(request(Some(&holder)), None).unwrap(),
            SocketAddr::from(([127, 0, 0, 1], 5000)),
        ) else {
            panic!("a second mint would replace the live grant");
        };
        assert_eq!(refused.code, RpcErrorCode::Conflict);
        assert!(grants.inspect(&first.id).is_ok());
    }

    #[tokio::test]
    async fn a_grant_without_sealed_values_has_none_to_read() {
        let grants = BuildGrants::default();
        mint(&grants, request(Some(&hex::encode(holder().as_bytes()))));
        let grant = grants.admit(holder().as_bytes()).unwrap();
        assert_eq!(read(sealed(&grant)).await.0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn sealed_values_need_a_holder_and_must_open_here() {
        let cluster = Cluster::new();
        let holderless = MintBuildGrantRequest {
            holder: None,
            ..sealed_request(&cluster)
        };
        let Err(refused) = Mint::check(holderless, Some(&cluster.dir.0)) else {
            panic!("a holderless grant would return its key to Cloud");
        };
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument);

        let elsewhere = Cluster::new();
        for key_dir in [Some(elsewhere.dir.0.as_path()), None] {
            let Err(refused) = Mint::check(sealed_request(&cluster), key_dir) else {
                panic!("another Cluster's key opens nothing");
            };
            assert_eq!(refused.code, RpcErrorCode::Conflict);
            assert!(!refused.message.contains(MARKER), "{}", refused.message);
        }
    }

    #[test]
    fn a_holder_mint_names_the_holder_and_returns_no_secret() {
        let grants = BuildGrants::default();
        let holder = SecretKey::from_bytes(&[3; 32]).public();
        let minted = mint(&grants, request(Some(&hex::encode(holder.as_bytes()))));
        assert_eq!(minted.id.as_str(), hex::encode(holder.as_bytes()));
        assert_eq!(
            minted.key,
            MintedKey::Holder {
                machine: ManagementIdentity::from_bytes([7; 32])
            }
        );
        assert!(grants.admit(holder.as_bytes()).is_some());
    }

    #[test]
    fn a_holderless_mint_returns_the_secret_it_made() {
        let grants = BuildGrants::default();
        let minted = mint(&grants, request(None));
        let MintedKey::Grant { grant } = minted.key else {
            panic!("a holderless mint returns its grant");
        };
        let public = SecretKey::from_bytes(grant.secret()).public();
        assert_eq!(minted.id.as_str(), hex::encode(public.as_bytes()));
        assert_eq!(grant.machine(), &ManagementIdentity::from_bytes([7; 32]));
        assert!(grants.admit(public.as_bytes()).is_some());
    }

    #[test]
    fn a_holder_that_is_not_a_public_key_is_refused() {
        let mut point = [0; 32];
        point[0] = 2;
        let Err(refused) = Mint::check(request(Some(&hex::encode(point))), None) else {
            panic!("y = 2 is not on the curve");
        };
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    }

    #[test]
    fn a_grant_serves_sealed_values_only_to_a_get() {
        let repository = BuildGrantRepository::parse("ployz-build/web").unwrap();
        assert_eq!(
            Route::parse(&Method::GET, "/ployz/sealed", None, &repository),
            Some(Route::Sealed)
        );
        for method in [Method::HEAD, Method::POST, Method::PUT] {
            assert_eq!(
                Route::parse(&method, "/ployz/sealed", None, &repository),
                None
            );
        }
        assert_eq!(
            Route::parse(&Method::GET, "/ployz/sealed/x", None, &repository),
            None
        );
    }

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
