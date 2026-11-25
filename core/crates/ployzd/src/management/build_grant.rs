//! Build Grants: Machine-minted, in-memory permission for one image push into this
//! Machine's image ingest. A grant key is admitted only on [`BUILD_GRANT_ALPN`], whose
//! connections reach a filtering registry proxy and never the Machine API, so the grant
//! is not a Management Capability (a Machine-local safety boundary, DESIGN bet 10).
//!
//! A grant allows the OCI Distribution calls one `docker push` makes into its one
//! repository, and ends after one tagged manifest lands (or the ingest confirms it
//! already holds that tag at its digest), after [`GRANT_LIFETIME`], or when the daemon
//! stops. The Machine RPC handlers only forward to [`mint`] and
//! [`BuildGrants::inspect`].
//!
//! Only a [`Minter`] mints: a Full caller, or a rule build every `rule_grant` check
//! passed. An Operator is neither. A Full caller's grant ends with its key's Full scope:
//! once a key change that took it returns, the grant serves nothing more. Its Sealed values
//! open only inside work that key change waits for: the mint, or a request on a connection
//! that holds the grant's [`Bound`].
//!
//! A grant minted for a holder can carry the built Service's Sealed values, still
//! sealed. Its holder reads them opened from `GET /ployz/sealed` once, before the push.
//! A grant minted under an automatic deploy rule, after every check in `rule_grant`,
//! is served, and serves its values only HPKE-sealed from the Cluster's attest
//! key to the build's recipient key, bound to the grant's audience. It serves that even
//! with no Sealed values, so the builder can tell a Cluster Machine from any endpoint.

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
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use iroh::{
    PublicKey, SecretKey,
    endpoint::{Connection, VarInt},
};
use ployz_core::{
    BUILD_GRANT_ENDED, BUILD_GRANT_REFUSED, BuildGrant, BuildGrantId, BuildGrantInspected,
    BuildGrantMinted, BuildGrantRepository, ImageDigest, ManagementIdentity, MintBuildGrantRequest,
    MintedKey, OpenedSealed, RETAINED_DIGEST_TAG_PREFIX, RpcError, RpcErrorCode, SealedRecord,
    authority::RecipientKey,
};
use sha2::{Digest as _, Sha256};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::machine::{Bound, Cause, Seen, Tie, local_machine::rule_grant::AdmittedRule};
use crate::sealed_env::SealingKeyStore;

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
    ingest: IngestAt,
    expires: Instant,
    push: Mutex<Push>,
    /// Locked only under `push`.
    sealed: Mutex<Sealing>,
    /// The key of the Full caller that minted it; `None` for a rule build's.
    tie: Option<Tie>,
}

/// Where a grant's pushes go: Image Ingest's address, and which of its processes runs.
#[derive(Clone)]
pub struct IngestAt {
    address: SocketAddr,
    process: IngestProcess,
}

/// Which process of Image Ingest runs now, `None` when none does. A request one process
/// took can't land once another runs or none does.
pub type IngestProcess = Arc<
    dyn Fn() -> futures_util::future::BoxFuture<'static, Result<Option<String>, Cause>>
        + Send
        + Sync,
>;

impl IngestAt {
    #[must_use]
    pub fn new(address: SocketAddr, process: IngestProcess) -> Self {
        Self { address, process }
    }
}

/// An Image Ingest at `address` whose processes nothing can name, so a manifest it never
/// answered stays in doubt.
impl From<SocketAddr> for IngestAt {
    fn from(address: SocketAddr) -> Self {
        Self::new(
            address,
            Arc::new(|| Box::pin(async { Err("no Docker to name Image Ingest's process".into()) })),
        )
    }
}

enum Sealing {
    None,
    Ready(Release),
    /// Its holder read them; nothing more is served.
    Served,
}

/// How a grant releases its build's Sealed values, kept sealed until its holder reads
/// them.
enum Release {
    /// Opened, to the holder over the grant connection.
    Plain {
        record: SealedRecord,
        sealing: SealingKeyStore,
    },
    /// Opened, then sealed from the attest key to a rule build's recipient. `None` holds
    /// no values and still seals the empty set.
    Attested {
        record: Option<SealedRecord>,
        sealing: SealingKeyStore,
        recipient: RecipientKey,
        audience: String,
    },
}

enum Push {
    Open,
    /// A tagged manifest is in flight; a second one is refused.
    Pushing,
    /// The digest this Machine verified and stored.
    Pushed(ImageDigest),
}

/// Who a mint is for: a caller at Full access, or a rule build that passed every
/// `rule_grant` check. An Operator is neither, so no Operator reaches a mint.
pub(crate) enum Minter {
    Full(Bound),
    Rule(AdmittedRule),
}

/// A mint request this Machine accepts, not yet minted.
pub struct Mint {
    repository: BuildGrantRepository,
    key: Key,
    release: Option<Release>,
    tie: Option<Tie>,
}

enum Key {
    /// This Machine made the key; the minted grant returns its secret.
    Made(SecretKey),
    /// The holder made it and keeps the secret.
    Holder(PublicKey),
}

impl Mint {
    /// Check a Full caller's `request` before anything is opened for it, and that its
    /// Sealed values open with `sealing`, which is `None` while Docker is unavailable.
    ///
    /// # Errors
    /// `InvalidArgument` when the holder is not a public key or Sealed values have no
    /// holder; `Conflict` when this Machine has no Sealing Key or they don't open here.
    pub fn check(
        request: MintBuildGrantRequest,
        sealing: Option<&SealingKeyStore>,
        full: Bound,
    ) -> Result<Mint, RpcError> {
        Self::check_admitted(request, sealing, Minter::Full(full))
    }

    /// [`Self::check`] for the request of `minter`, a rule build's after the caller ran
    /// every `rule_grant` check.
    ///
    /// # Errors
    /// As [`Self::check`].
    pub(crate) fn check_admitted(
        request: MintBuildGrantRequest,
        sealing: Option<&SealingKeyStore>,
        minter: Minter,
    ) -> Result<Mint, RpcError> {
        let (full, rule) = match minter {
            Minter::Full(bound) => (Some(bound), None),
            Minter::Rule(admitted) => (None, Some(admitted)),
        };
        let checked = Self::checked(request, sealing, full.as_ref(), rule);
        // The mint's own read is over, opened or refused, so only the grant's tie stays.
        let tie = full.map(Bound::into_tie);
        checked.map(|mint| Mint { tie, ..mint })
    }

    /// The checks of [`Self::check_admitted`], opening a Full caller's Sealed values under
    /// `full`, or a rule build's for `rule`.
    fn checked(
        request: MintBuildGrantRequest,
        sealing: Option<&SealingKeyStore>,
        full: Option<&Bound>,
        rule: Option<AdmittedRule>,
    ) -> Result<Mint, RpcError> {
        let key = match request.holder {
            None => Key::Made(SecretKey::generate()),
            Some(holder) => Key::Holder(
                hex::decode(holder.as_str())
                    .ok()
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                    .and_then(|bytes| PublicKey::from_bytes(&bytes).ok())
                    .ok_or_else(|| {
                        RpcError::new(
                            RpcErrorCode::InvalidArgument,
                            "the Build Grant holder is not a public key",
                        )
                    })?,
            ),
        };
        if matches!(key, Key::Made(_)) && (request.sealed.is_some() || rule.is_some()) {
            return Err(RpcError::new(
                RpcErrorCode::InvalidArgument,
                "a Build Grant carrying Sealed values needs a holder",
            ));
        }
        let sealing = || {
            sealing.ok_or_else(|| {
                RpcError::new(
                    RpcErrorCode::Conflict,
                    "this Machine has no Sealing Key to open the Build's Sealed values",
                )
            })
        };
        if let Some(record) = &request.sealed {
            let sealing = sealing()?;
            let opened = match full {
                Some(bound) => bound
                    .read(|| sealing.open_for_build(record))
                    .map_err(|_| ended())?,
                None => sealing.open_for_build(record),
            };
            opened.map_err(|error| RpcError::new(RpcErrorCode::Conflict, error.to_string()))?;
        }
        let release = match (rule, request.sealed) {
            (None, None) => None,
            (None, Some(record)) => Some(Release::Plain {
                record,
                sealing: sealing()?.clone(),
            }),
            (Some(admitted), record) => Some(Release::Attested {
                record,
                sealing: sealing()?.clone(),
                recipient: admitted.recipient,
                audience: admitted.audience,
            }),
        };
        Ok(Mint {
            repository: request.repository,
            key,
            release,
            tie: None,
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
        ingest: impl Into<IngestAt>,
    ) -> Result<BuildGrantMinted, RpcError> {
        if mint.tie.as_ref().is_some_and(Tie::is_ended) {
            return Err(ended());
        }
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
            ingest: ingest.into(),
            expires: Instant::now() + GRANT_LIFETIME,
            push: Mutex::new(Push::Open),
            sealed: Mutex::new(mint.release.map_or(Sealing::None, Sealing::Ready)),
            tie: mint.tie,
        });
        let mut grants = self.grants.lock().expect("grant registry lock");
        grants.retain(|_, grant| grant.live());
        if grants.contains_key(&id) {
            return Err(RpcError::new(
                RpcErrorCode::Conflict,
                "this holder already holds a live Build Grant",
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
                .ok_or_else(|| RpcError::not_found("this Machine holds no such Build Grant; it expired or the daemon restarted"))?,
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
        (grant.live() && !grant.pushed()).then_some(grant)
    }
}

/// Mint a grant for `request`, made by `minter`. Its Sealed values must open with this
/// Machine's Sealing Key before image ingest opens, and ingest opens before the grant
/// is held, so its failure reaches the minting caller rather than the pusher.
///
/// # Errors
/// The refusal [`Mint::check`] answers or [`BuildGrants::mint`] answers, in the inner
/// result; the [`crate::machine::LocalMachineError`] of an ingest that didn't open.
pub(crate) async fn mint(
    grants: &BuildGrants,
    local: &crate::machine::LocalMachine,
    ingest: Arc<crate::docker::ImageIngest>,
    request: MintBuildGrantRequest,
    minter: Minter,
) -> Result<Result<BuildGrantMinted, RpcError>, crate::machine::LocalMachineError> {
    let sealing = local
        .containers()
        .map(crate::docker::ContainerRuntime::sealing);
    let mint = match Mint::check_admitted(request, sealing, minter) {
        Ok(mint) => mint,
        Err(refused) => return Ok(Err(refused)),
    };
    let opened = local
        .ensure_image_ingest(Arc::clone(&ingest))
        .await?
        .destination;
    let process: IngestProcess = Arc::new(move || {
        let ingest = Arc::clone(&ingest);
        Box::pin(async move { ingest.process().await.map_err(Into::into) })
    });
    Ok(grants.mint(
        local.record().management_secret().public_key(),
        mint,
        IngestAt::new(
            SocketAddr::from((opened.management_address.0, opened.port)),
            process,
        ),
    ))
}

fn grant_id(key: &[u8; 32]) -> BuildGrantId {
    BuildGrantId::parse(hex::encode(key)).expect("hex of 32 bytes is a Build Grant ID")
}

impl Grant {
    fn pushed(&self) -> bool {
        matches!(*self.push.lock().expect("grant push lock"), Push::Pushed(_))
    }

    /// Unexpired, and not ended by a key change that left its minter short of Full scope.
    fn live(&self) -> bool {
        self.expires > Instant::now() && !self.tie.as_ref().is_some_and(Tie::is_ended)
    }

    /// Completes once a key change ended the grant; never for a rule build's.
    async fn ended(&self) {
        match &self.tie {
            Some(tie) => tie.ended().await,
            None => std::future::pending().await,
        }
    }
}

/// A grant as one connection serves it. A Full caller's carries the [`Bound`] a key change
/// that ends the grant waits for, and opens its Sealed values only under it.
#[derive(Clone)]
struct Serving {
    grant: Arc<Grant>,
    hold: Option<Arc<Bound>>,
}

impl Serving {
    /// `None` once a key change ended a Full caller's grant.
    fn new(grant: Arc<Grant>) -> Option<Self> {
        let hold = match &grant.tie {
            Some(tie) => Some(Arc::new(tie.hold().ok()?)),
            None => None,
        };
        Some(Self { grant, hold })
    }

    /// Stop serving, once every request that read under the hold has ended.
    fn stop(self) {
        if let Some(hold) = self.hold.and_then(Arc::into_inner) {
            hold.stopped(Seen::Joined);
        }
    }
}

/// The refusal for a grant whose minter no longer holds Full scope.
fn ended() -> RpcError {
    RpcError::new(
        RpcErrorCode::Unauthenticated,
        "this Device no longer has Full access to this Server",
    )
}

/// Serve one connection on the grant ALPN until its grant ends. Each bidirectional
/// stream carries one HTTP/1.1 connection of the pusher's registry client.
pub(super) async fn serve(
    connection: Connection,
    grants: &BuildGrants,
    shutdown: CancellationToken,
) {
    let Some(serving) = grants
        .admit(connection.remote_id().as_bytes())
        .and_then(Serving::new)
    else {
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
            () = tokio::time::sleep_until(serving.grant.expires) => break,
            () = serving.grant.ended() => break,
            () = shutdown.cancelled() => break,
            _ = streams.join_next(), if !streams.is_empty() => continue,
            accepted = connection.accept_bi() => match accepted {
                Ok(streams) => streams,
                Err(_) => break,
            },
        };
        let serving = serving.clone();
        let client = client.clone();
        streams.spawn(async move {
            let service = service_fn(move |request| {
                let serving = serving.clone();
                let client = client.clone();
                async move { Ok::<_, Infallible>(handle(request, &serving, &client).await) }
            });
            if let Err(error) = http1::Builder::new()
                .serve_connection(TokioIo::new(tokio::io::join(recv, send)), service)
                .await
            {
                tracing::debug!(error = %ployz_core::error_chain::inline(&error), "build grant stream ended");
            }
        });
    }
    // An ended grant pushes and opens nothing more: its requests stop before the hold that
    // a key change waits for goes. A manifest commit under way holds a Bound of its own.
    streams.abort_all();
    while streams.join_next().await.is_some() {}
    connection.close(VarInt::from_u32(BUILD_GRANT_ENDED), b"build grant ended");
    serving.stop();
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

async fn handle<B>(
    request: Request<B>,
    serving: &Serving,
    client: &reqwest::Client,
) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<axum::BoxError>,
{
    let grant = &*serving.grant;
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
    if !grant.live() {
        return denied(StatusCode::FORBIDDEN, "the Build Grant has ended");
    }
    let (parts, body) = request.into_parts();
    let (digest, tagged) = match route {
        Route::Sealed => return sealed(serving),
        Route::Ping | Route::Blob | Route::Upload | Route::ManifestHead => {
            let body = reqwest::Body::wrap_stream(Body::new(body).into_data_stream());
            return forward(client, grant.ingest.address, &parts, body).await;
        }
        Route::ManifestTagHead(digest) => {
            let response = forward(
                client,
                grant.ingest.address,
                &parts,
                reqwest::Body::from(""),
            )
            .await;
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
    // Image Ingest stores a manifest as it answers, so this is where a push lands. A Full
    // caller's grant takes a fresh Bound for it under the lock a key change ends work under.
    let commit = match &grant.tie {
        Some(tie) => match tie.hold() {
            Ok(bound) => Some(bound),
            Err(_) => return denied(StatusCode::FORBIDDEN, "the Build Grant has ended"),
        },
        None => None,
    };
    if tagged {
        let mut push = grant.push.lock().expect("grant push lock");
        if !matches!(*push, Push::Open) {
            drop(push);
            if let Some(commit) = commit {
                commit.stopped(Seen::NeverSent);
            }
            return denied(StatusCode::CONFLICT, "the Build Grant is already pushing");
        }
        *push = Push::Pushing;
    }
    // Its own task, so ending the grant's requests doesn't cut it off: the key change that
    // ends the grant waits until Image Ingest answered.
    let committing = tokio::spawn(commit_manifest(
        client.clone(),
        Arc::clone(&serving.grant),
        parts,
        manifest,
        tagged.then_some(digest),
        commit,
    ));
    committing.await.unwrap_or_else(|_| {
        denied(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the manifest's commit task failed",
        )
    })
}

/// How long Image Ingest has to answer a manifest it was sent.
const COMMIT_WITHIN: Duration = Duration::from_secs(60);

/// How long Image Ingest has to answer whether it holds a manifest.
const LOOK_WITHIN: Duration = Duration::from_secs(5);

/// Why a manifest's commit has no answer from Image Ingest.
#[derive(Debug, thiserror::Error)]
enum Uncommitted {
    #[error("Image Ingest did not answer a Build Grant's manifest within {} s", COMMIT_WITHIN.as_secs())]
    Late,
    #[error("Image Ingest did not answer a Build Grant's manifest")]
    Lost(#[source] reqwest::Error),
}

/// Send `manifest` to `grant`'s Image Ingest, and spend the grant on a stored `tag`. `bound`
/// goes once Image Ingest answered, or once nothing reached it. Without an answer it stays
/// in doubt, and a key change looks for the manifest again ([`manifest_landed`]).
async fn commit_manifest(
    client: reqwest::Client,
    grant: Arc<Grant>,
    parts: http::request::Parts,
    manifest: Bytes,
    tag: Option<ImageDigest>,
    bound: Option<Bound>,
) -> Response<Body> {
    let sent = tokio::time::timeout(
        COMMIT_WITHIN,
        send(&client, grant.ingest.address, &parts, manifest.into()),
    )
    .await;
    // What the commit saw: Image Ingest's answer, nothing sent, or neither.
    let (seen, response) = match sent {
        Ok(Ok(upstream)) => (Ok(Seen::Answered), relayed(upstream)),
        Ok(Err(error)) => {
            tracing::debug!(error = %ployz_core::error_chain::inline(&error), "build grant ingest request failed");
            let response = denied(StatusCode::BAD_GATEWAY, "image ingest is unavailable");
            if error.is_connect() {
                (Ok(Seen::NeverSent), response)
            } else {
                (Err(Uncommitted::Lost(error)), response)
            }
        }
        Err(_) => (
            Err(Uncommitted::Late),
            denied(StatusCode::GATEWAY_TIMEOUT, "image ingest did not answer"),
        ),
    };
    match (bound, seen) {
        (None, _) => {}
        (Some(bound), Ok(seen)) => bound.stopped(seen),
        (Some(bound), Err(uncommitted)) => {
            let reference = parts.uri.path().rsplit('/').next().unwrap_or_default();
            bound.name(format!(
                "a Build Grant's manifest {}:{reference}",
                grant.repository.as_str()
            ));
            // Which process took it, so a later look knows when it can no longer land.
            let took = (grant.ingest.process)().await.map_err(drop);
            let (grant, path) = (Arc::clone(&grant), parts.uri.path().to_owned());
            bound.unconfirmed(
                uncommitted,
                Some(Arc::new(move || {
                    let (grant, path, took) = (Arc::clone(&grant), path.clone(), took.clone());
                    let client = client.clone();
                    Box::pin(async move { manifest_landed(&client, &grant, &path, took).await })
                })),
            );
        }
    }
    if let Some(digest) = tag {
        // Only a stored manifest spends the grant; a refused one may be retried.
        *grant.push.lock().expect("grant push lock") = if response.status().is_success() {
            Push::Pushed(digest)
        } else {
            Push::Open
        };
    }
    response
}

/// Whether the manifest a grant PUT to `path`, unanswered, landed. Image Ingest holding it
/// is [`Seen::Landed`]. Not holding it ends the doubt only once the process that took it,
/// `took`, no longer runs, since that process may still store it; until then it is
/// [`Seen::Pending`]. The grant was ended by the key change that looks, so no other PUT
/// reaches Image Ingest for it.
async fn manifest_landed(
    client: &reqwest::Client,
    grant: &Grant,
    path: &str,
    took: Result<Option<String>, ()>,
) -> Result<Seen, Cause> {
    let now = (grant.ingest.process)().await?;
    let gone = match took {
        Ok(Some(took)) => now.as_ref() != Some(&took),
        Ok(None) => true,
        Err(()) => false,
    };
    // Looked at once its process is known gone, so a commit landing before then is seen.
    let held = tokio::time::timeout(
        LOOK_WITHIN,
        client
            .head(format!("http://{}{path}", grant.ingest.address))
            .header(header::ACCEPT, MANIFEST_TYPES)
            .send(),
    )
    .await;
    match held {
        Ok(Ok(answer)) if answer.status() == StatusCode::OK => Ok(Seen::Landed),
        Ok(Ok(answer)) if answer.status() == StatusCode::NOT_FOUND => Ok(if gone {
            Seen::ContainerGone
        } else {
            Seen::Pending
        }),
        // No Image Ingest runs to ask, and none that took the manifest can store it.
        Ok(Err(error)) if gone && now.is_none() && error.is_connect() => Ok(Seen::ContainerGone),
        Ok(Ok(answer)) => Err(format!("Image Ingest answered {}", answer.status()).into()),
        Ok(Err(error)) => Err(error.into()),
        Err(_) => Err(format!(
            "Image Ingest did not say within {} s whether it holds the manifest",
            LOOK_WITHIN.as_secs()
        )
        .into()),
    }
}

/// The manifest types a push writes, which a look asks for.
const MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json";

/// The grant's Sealed values, opened, for its holder once, while the grant is live and
/// nothing is pushing or pushed.
fn sealed(serving: &Serving) -> Response<Body> {
    let grant = &*serving.grant;
    let sealed = {
        let push = grant.push.lock().expect("grant push lock");
        if !matches!(*push, Push::Open) || !grant.live() {
            return denied(StatusCode::FORBIDDEN, "the Build Grant has ended");
        }
        let mut sealing = grant.sealed.lock().expect("grant sealed lock");
        match std::mem::replace(&mut *sealing, Sealing::Served) {
            Sealing::Ready(release) => release,
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
    // Opened only while its minter still holds Full scope, under the lock a key change
    // ends grants under, inside the request it waits for.
    let released = match &serving.hold {
        Some(hold) => hold.read(|| release(sealed)),
        None => Ok(release(sealed)),
    };
    let (body, content_type) = match released {
        Ok(Ok(released)) => released,
        Ok(Err(message)) => return denied(StatusCode::PRECONDITION_FAILED, &message),
        Err(_) => return denied(StatusCode::FORBIDDEN, "the Build Grant has ended"),
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .expect("static response")
}

/// The body a grant serves its holder, and its content type.
fn release(release: Release) -> Result<(Vec<u8>, &'static str), String> {
    match release {
        Release::Plain { record, sealing } => {
            let opened = sealing
                .open_for_build(&record)
                .map_err(|error| error.to_string())?;
            Ok((
                serde_json::to_vec(&opened).expect("opened values encode"),
                "application/json",
            ))
        }
        Release::Attested {
            record,
            sealing,
            recipient,
            audience,
        } => {
            let opened = match record {
                Some(record) => sealing
                    .open_for_build(&record)
                    .map_err(|error| error.to_string())?,
                None => OpenedSealed::new(Default::default(), Vec::new()),
            };
            let attest = sealing.attest().map_err(|error| error.to_string())?;
            let sealed = opened
                .seal_for_build(&attest, &recipient, &audience)
                .map_err(|error| error.to_string())?;
            Ok((sealed, "application/octet-stream"))
        }
    }
}

async fn forward(
    client: &reqwest::Client,
    ingest: SocketAddr,
    parts: &http::request::Parts,
    body: reqwest::Body,
) -> Response<Body> {
    match send(client, ingest, parts, body).await {
        Ok(upstream) => relayed(upstream),
        Err(error) => {
            tracing::debug!(error = %ployz_core::error_chain::inline(&error), "build grant ingest request failed");
            denied(StatusCode::BAD_GATEWAY, "image ingest is unavailable")
        }
    }
}

async fn send(
    client: &reqwest::Client,
    ingest: SocketAddr,
    parts: &http::request::Parts,
    body: reqwest::Body,
) -> Result<reqwest::Response, reqwest::Error> {
    let path = parts.uri.path_and_query().map_or("/", |path| path.as_str());
    client
        .request(parts.method.clone(), format!("http://{ingest}{path}"))
        .headers(end_to_end(&parts.headers))
        .body(body)
        .send()
        .await
}

/// Image Ingest's answer, as the pusher reads it.
fn relayed(upstream: reqwest::Response) -> Response<Body> {
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
            rule: None,
        }
    }

    fn mint(grants: &BuildGrants, request: MintBuildGrantRequest) -> BuildGrantMinted {
        let machine = ManagementIdentity::from_bytes([7; 32]);
        let ingest = SocketAddr::from(([127, 0, 0, 1], 5000));
        grants
            .mint(
                machine,
                Mint::check(request, None, Bound::for_test()).unwrap(),
                ingest,
            )
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
        let mint = Mint::check(
            sealed_request(cluster),
            Some(&cluster.store()),
            Bound::for_test(),
        )
        .unwrap();
        grants
            .mint(
                ManagementIdentity::from_bytes([7; 32]),
                mint,
                SocketAddr::from(([127, 0, 0, 1], 5000)),
            )
            .unwrap();
        grants.admit(holder().as_bytes()).unwrap()
    }

    /// `GET /ployz/sealed` of `grant`, from a connection admitted to it now.
    fn sealed_of(grant: &Arc<Grant>) -> Response<Body> {
        sealed(&Serving::new(Arc::clone(grant)).expect("a live grant serves"))
    }

    async fn read(response: Response<Body>) -> (StatusCode, String) {
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn a_holder_reads_its_grants_sealed_values_opened() {
        let cluster = Cluster::new();
        let (status, body) = read(sealed_of(&sealed_grant(&cluster))).await;
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
        let expired = Arc::new(Grant {
            repository: live.repository.clone(),
            ingest: live.ingest.clone(),
            expires: Instant::now(),
            push: Mutex::new(Push::Open),
            sealed: Mutex::new(Sealing::Ready(Release::Plain {
                record: sealed_request(&cluster).sealed.unwrap(),
                sealing: cluster.store(),
            })),
            tie: None,
        });
        for grant in [&pushing, &pushed, &expired] {
            let (status, body) = read(sealed_of(grant)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert!(!body.contains(MARKER));
        }
    }

    /// A registry holding a Sealed grant for [`holder`].
    fn sealed_grants(cluster: &Cluster) -> BuildGrants {
        let grants = BuildGrants::default();
        let mint = Mint::check(
            sealed_request(cluster),
            Some(&cluster.store()),
            Bound::for_test(),
        )
        .unwrap();
        grants
            .mint(
                ManagementIdentity::from_bytes([7; 32]),
                mint,
                SocketAddr::from(([127, 0, 0, 1], 5000)),
            )
            .unwrap();
        grants
    }

    /// The holder's `GET /ployz/sealed` through the request handler, as a connection
    /// admitted earlier would send it.
    async fn get_sealed(serving: &Serving) -> (StatusCode, String) {
        let request = Request::builder()
            .method(Method::GET)
            .uri("/ployz/sealed")
            .body(http_body_util::Empty::<Bytes>::new())
            .unwrap();
        read(handle(request, serving, &reqwest::Client::new()).await).await
    }

    #[tokio::test]
    async fn a_full_callers_grant_opens() {
        let cluster = Cluster::new();
        let grants = sealed_grants(&cluster);
        let admitted = grants.admit(holder().as_bytes()).unwrap();
        let (status, body) = get_sealed(&Serving::new(admitted).unwrap()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let opened: OpenedSealed = serde_json::from_str(&body).unwrap();
        assert_eq!(opened.get("NPM_TOKEN"), Some(MARKER));
    }

    #[tokio::test]
    async fn a_holder_reads_sealed_values_once() {
        let cluster = Cluster::new();
        let grant = sealed_grant(&cluster);
        assert_eq!(read(sealed_of(&grant)).await.0, StatusCode::OK);
        let (status, body) = read(sealed_of(&grant)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(!body.contains(MARKER));
    }

    #[tokio::test]
    async fn sealed_values_that_fail_to_open_are_not_served_later() {
        let cluster = Cluster::new();
        let elsewhere = Cluster::new();
        let grant = sealed_grant(&cluster);
        *grant.sealed.lock().unwrap() = Sealing::Ready(Release::Plain {
            record: sealed_request(&cluster).sealed.unwrap(),
            sealing: elsewhere.store(),
        });
        assert_eq!(
            read(sealed_of(&grant)).await.0,
            StatusCode::PRECONDITION_FAILED
        );
        let (status, body) = read(sealed_of(&grant)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    /// A Device whose key also holds Cloud's slot mints a grant carrying Sealed values, and a
    /// holder connects. Removing the Device waits for that connection to stop serving, which
    /// opens nothing more meanwhile, and once `device rm` returned no connection is admitted.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_removed_devices_grant_serves_nothing_once_its_removal_returns() {
        let cluster = Cluster::new();
        let dir = tempfile::tempdir().unwrap();
        let mut store = crate::machine::test_support::founder(dir.path(), "grants");
        let device = crate::machine::test_support::device_in_two_slots(&mut store);
        let local =
            crate::machine::LocalMachine::new(crate::machine::RecordOwner::spawn(store).unwrap());
        let from_device = local.clone().with_management_client(device.into());
        let grants = BuildGrants::default();
        let mint = Mint::check(
            sealed_request(&cluster),
            Some(&cluster.store()),
            from_device
                .bind_full(crate::machine::Work::BuildGrant)
                .unwrap(),
        )
        .unwrap();
        grants
            .mint(
                ManagementIdentity::from_bytes([7; 32]),
                mint,
                SocketAddr::from(([127, 0, 0, 1], 5000)),
            )
            .unwrap();
        let admitted = grants.admit(holder().as_bytes()).unwrap();
        let connected = Serving::new(Arc::clone(&admitted)).unwrap();

        let removal = tokio::spawn(async move {
            local
                .on_local_socket()
                .set_device(ployz_core::SetDeviceRequest::Remove {
                    code: device.code(),
                })
                .await
        });
        admitted.ended().await;
        let (status, body) = get_sealed(&connected).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(!body.contains(MARKER));
        assert!(
            !removal.is_finished(),
            "the removal returned while a connection still served its grant"
        );
        connected.stop();
        removal.await.unwrap().unwrap();

        assert!(Serving::new(admitted).is_none());
        assert!(grants.admit(holder().as_bytes()).is_none());
    }

    /// A stand-in Image Ingest, which counts each request it read whole and notifies
    /// `read` of it.
    struct Ingest {
        address: SocketAddr,
        received: Arc<std::sync::atomic::AtomicUsize>,
        read: Arc<tokio::sync::Notify>,
    }

    /// An [`Ingest`] that answers each request it read with `201 Created`, or with `lose`,
    /// drops it unanswered once `lose` is notified. A PUT it read is stored when `stores`,
    /// and a HEAD finds what it stored.
    async fn ingest(lose: Option<Arc<tokio::sync::Notify>>, stores: bool) -> Ingest {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ingest = Ingest {
            address: listener.local_addr().unwrap(),
            received: Arc::default(),
            read: Arc::default(),
        };
        let (received, read) = (Arc::clone(&ingest.received), Arc::clone(&ingest.read));
        let stored = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (received, read, lose) = (received.clone(), read.clone(), lose.clone());
                let stored = Arc::clone(&stored);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0; 4096];
                    loop {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(chunk.split_at(n).0);
                        let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(request.split_at(end).0).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .map_or(0, |length| length.trim().parse::<usize>().unwrap());
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                    let line = String::from_utf8_lossy(&request)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    let mut words = line.split_whitespace();
                    let (method, path) = (
                        words.next().unwrap_or_default(),
                        words.next().unwrap_or_default(),
                    );
                    if method == "HEAD" {
                        let found: &[u8] = if stored.lock().unwrap().contains(path) {
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        } else {
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        };
                        let _ = stream.write_all(found).await;
                        return;
                    }
                    if stores && method == "PUT" {
                        stored.lock().unwrap().insert(path.to_owned());
                    }
                    received.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    read.notify_one();
                    match lose {
                        Some(lose) => lose.notified().await,
                        None => stream
                            .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await
                            .unwrap(),
                    }
                });
            }
        });
        ingest
    }

    const MANIFEST: &[u8] = br#"{"schemaVersion":2,"layers":[]}"#;

    /// Where a holder PUTs [`MANIFEST`] tagged with its own digest.
    fn manifest_tag() -> String {
        format!(
            "/v2/ployz-build/web/manifests/ployz-sha256-{}",
            hex::encode(Sha256::digest(MANIFEST))
        )
    }

    /// A grant for [`holder`] pushing to `ingest`, minted by a Device whose key also holds
    /// Cloud's slot, with the Machine that minted it and the Device.
    fn device_grant(
        dir: &std::path::Path,
        ingest: impl Into<IngestAt>,
    ) -> (
        crate::machine::LocalMachine,
        ployz_core::DeviceKey,
        Arc<Grant>,
    ) {
        let mut store = crate::machine::test_support::founder(dir, "grants");
        let device = crate::machine::test_support::device_in_two_slots(&mut store);
        let local =
            crate::machine::LocalMachine::new(crate::machine::RecordOwner::spawn(store).unwrap());
        let mint = Mint::check(
            request(Some(&hex::encode(holder().as_bytes()))),
            None,
            local
                .clone()
                .with_management_client(device.into())
                .bind_full(crate::machine::Work::BuildGrant)
                .unwrap(),
        )
        .unwrap();
        let grants = BuildGrants::default();
        grants
            .mint(ManagementIdentity::from_bytes([7; 32]), mint, ingest)
            .unwrap();
        (local, device, grants.admit(holder().as_bytes()).unwrap())
    }

    /// A Device's grant is mid-way through sending a manifest when the Device is removed.
    /// Once the removal ended the grant, the rest of the push reaches no Image Ingest.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_manifest_still_arriving_when_its_devices_removal_began_reaches_no_ingest() {
        let dir = tempfile::tempdir().unwrap();
        let ingest = ingest(None, false).await;
        let (local, device, admitted) = device_grant(dir.path(), ingest.address);
        let connected = Serving::new(Arc::clone(&admitted)).unwrap();
        let (arriving, body) =
            tokio::sync::mpsc::channel::<Result<hyper::body::Frame<Bytes>, Infallible>>(1);
        let request = Request::builder()
            .method(Method::PUT)
            .uri(manifest_tag())
            .body(http_body_util::StreamBody::new(
                tokio_stream::wrappers::ReceiverStream::new(body),
            ))
            .unwrap();
        let pushing = tokio::spawn({
            let connected = connected.clone();
            async move { read(handle(request, &connected, &reqwest::Client::new()).await).await }
        });
        let (head, tail) = MANIFEST.split_at(8);
        let (first, second) = head.split_at(4);
        for part in [first, second] {
            arriving
                .send(Ok(hyper::body::Frame::data(Bytes::copy_from_slice(part))))
                .await
                .unwrap();
        }
        // The second part waited for the handler to take the first: it is reading the body.

        let removal = tokio::spawn(async move {
            local
                .on_local_socket()
                .set_device(ployz_core::SetDeviceRequest::Remove {
                    code: device.code(),
                })
                .await
        });
        admitted.ended().await;
        arriving
            .send(Ok(hyper::body::Frame::data(Bytes::copy_from_slice(tail))))
            .await
            .unwrap();
        drop(arriving);
        let (status, body) = pushing.await.unwrap();
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(
            ingest.received.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a removed Device's manifest reached Image Ingest"
        );
        connected.stop();
        removal.await.unwrap().unwrap();
    }

    /// A Device's grant has a manifest at Image Ingest when the Device is removed, and
    /// Image Ingest's answer never comes, so the manifest may have landed: `device rm` says
    /// the grant didn't confirm it stopped.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_removal_says_when_a_manifest_in_flight_may_have_landed() {
        let dir = tempfile::tempdir().unwrap();
        let lose = Arc::new(tokio::sync::Notify::new());
        let ingest = ingest(Some(Arc::clone(&lose)), false).await;
        let (local, device, admitted) = device_grant(dir.path(), ingest.address);
        let connected = Serving::new(Arc::clone(&admitted)).unwrap();
        let request = Request::builder()
            .method(Method::PUT)
            .uri(manifest_tag())
            .body(http_body_util::Full::new(Bytes::from_static(MANIFEST)))
            .unwrap();
        let pushing = tokio::spawn({
            let connected = connected.clone();
            async move { handle(request, &connected, &reqwest::Client::new()).await }
        });
        ingest.read.notified().await;

        let removal = tokio::spawn(async move {
            local
                .on_local_socket()
                .set_device(ployz_core::SetDeviceRequest::Remove {
                    code: device.code(),
                })
                .await
        });
        admitted.ended().await;
        // The grant's connection ends its requests, as `serve` does once the grant ended.
        pushing.abort();
        let _ = pushing.await;
        connected.stop();
        lose.notify_one();

        let removed = removal.await.unwrap();
        let Err(crate::machine::LocalMachineError::WorkNotStopped(unstopped)) = removed else {
            panic!("the removal said a push in flight stopped without knowing: {removed:?}");
        };
        assert_eq!(unstopped.works(), [crate::machine::Work::BuildGrant]);
    }

    /// A Device's grant has a manifest at Image Ingest when the Device is removed, and Image
    /// Ingest stores it but its answer never comes. `device rm` says it may have landed; the
    /// next key change finds it did and says so by name, once: the one after has nothing left
    /// to report.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_manifest_that_landed_after_its_devices_removal_is_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        let lose = Arc::new(tokio::sync::Notify::new());
        let ingest = ingest(Some(Arc::clone(&lose)), true).await;
        let (local, device, admitted) = device_grant(
            dir.path(),
            IngestAt::new(ingest.address, ingest_process(&running("ingest@1"))),
        );

        let removed = lose_manifest(&local, device, &admitted, &ingest, &lose).await;
        assert!(
            matches!(
                removed,
                Err(crate::machine::LocalMachineError::WorkNotStopped(_))
            ),
            "{removed:?}"
        );

        let looked = approve_key(&local, 11).await;
        let Err(crate::machine::LocalMachineError::WorkNotStopped(unstopped)) = looked else {
            panic!("a key change hid a manifest that landed after its grant ended: {looked:?}");
        };
        assert_eq!(unstopped.works(), [crate::machine::Work::BuildGrant]);
        let reported = ployz_core::error_chain::inline(&unstopped);
        let reference = manifest_tag().rsplit('/').next().unwrap().to_owned();
        assert!(reported.contains(&reference), "{reported}");
        assert!(reported.contains("landed after"), "{reported}");
        approve_key(&local, 12)
            .await
            .expect("a key change reported again a manifest already reported landed");
    }

    /// A Device's grant has a manifest at Image Ingest when the Device is removed, and the
    /// answer never comes, nor does Image Ingest hold it yet. While the Image Ingest that took
    /// it runs, it may still store it, so each key change reports it; once another Image
    /// Ingest runs in its place, it never will.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_lost_manifest_stays_reported_until_the_ingest_that_took_it_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let lose = Arc::new(tokio::sync::Notify::new());
        let ingest = ingest(Some(Arc::clone(&lose)), false).await;
        let process = running("ingest@1");
        let (local, device, admitted) = device_grant(
            dir.path(),
            IngestAt::new(ingest.address, ingest_process(&process)),
        );

        let removed = lose_manifest(&local, device, &admitted, &ingest, &lose).await;
        assert!(
            matches!(
                removed,
                Err(crate::machine::LocalMachineError::WorkNotStopped(_))
            ),
            "{removed:?}"
        );
        let again = approve_key(&local, 11).await;
        assert!(
            matches!(
                again,
                Err(crate::machine::LocalMachineError::WorkNotStopped(_))
            ),
            "a key change forgot a manifest the running Image Ingest may still store: {again:?}"
        );

        *process.lock().unwrap() = Some("ingest@2".to_owned());
        approve_key(&local, 12)
            .await
            .expect("a key change still reported a manifest whose Image Ingest is gone");
    }

    /// Which process of Image Ingest runs, as a test turns it.
    fn running(process: &str) -> Arc<std::sync::Mutex<Option<String>>> {
        Arc::new(std::sync::Mutex::new(Some(process.to_owned())))
    }

    fn ingest_process(running: &Arc<std::sync::Mutex<Option<String>>>) -> IngestProcess {
        let running = Arc::clone(running);
        Arc::new(move || {
            let now = running.lock().unwrap().clone();
            Box::pin(async move { Ok(now) })
        })
    }

    /// PUT [`MANIFEST`] through `admitted`, remove `device` once `ingest` read it, and lose
    /// Image Ingest's answer: what the removal returned.
    async fn lose_manifest(
        local: &crate::machine::LocalMachine,
        device: ployz_core::DeviceKey,
        admitted: &Arc<Grant>,
        ingest: &Ingest,
        lose: &tokio::sync::Notify,
    ) -> Result<ployz_core::DeviceSet, crate::machine::LocalMachineError> {
        let connected = Serving::new(Arc::clone(admitted)).unwrap();
        let request = Request::builder()
            .method(Method::PUT)
            .uri(manifest_tag())
            .body(http_body_util::Full::new(Bytes::from_static(MANIFEST)))
            .unwrap();
        let pushing = tokio::spawn({
            let connected = connected.clone();
            async move { handle(request, &connected, &reqwest::Client::new()).await }
        });
        ingest.read.notified().await;
        let removal = tokio::spawn({
            let local = local.clone();
            async move {
                local
                    .on_local_socket()
                    .set_device(ployz_core::SetDeviceRequest::Remove {
                        code: device.code(),
                    })
                    .await
            }
        });
        admitted.ended().await;
        pushing.abort();
        let _ = pushing.await;
        connected.stop();
        lose.notify_one();
        removal.await.unwrap()
    }

    /// Approve the Device key of `seed`, which nothing else holds: a key change that ends no
    /// work.
    async fn approve_key(
        local: &crate::machine::LocalMachine,
        seed: u8,
    ) -> Result<ployz_core::DeviceSet, crate::machine::LocalMachineError> {
        local
            .clone()
            .on_local_socket()
            .set_device(ployz_core::SetDeviceRequest::Approve {
                key: ployz_core::DeviceKey::from(
                    ployz_core::authority::Signer::from_seed(&[seed; 32]).public(),
                ),
            })
            .await
    }

    #[test]
    fn a_holder_with_a_live_grant_mints_no_second() {
        let grants = BuildGrants::default();
        let holder = hex::encode(holder().as_bytes());
        let first = mint(&grants, request(Some(&holder)));
        let Err(refused) = grants.mint(
            ManagementIdentity::from_bytes([7; 32]),
            Mint::check(request(Some(&holder)), None, Bound::for_test()).unwrap(),
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
        assert_eq!(read(sealed_of(&grant)).await.0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn sealed_values_need_a_holder_and_must_open_here() {
        let cluster = Cluster::new();
        let holderless = MintBuildGrantRequest {
            holder: None,
            ..sealed_request(&cluster)
        };
        let Err(refused) = Mint::check(holderless, Some(&cluster.store()), Bound::for_test())
        else {
            panic!("a holderless grant would return its key to Cloud");
        };
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument);

        let elsewhere = Cluster::new();
        for sealing in [Some(elsewhere.store()), None] {
            let Err(refused) = Mint::check(
                sealed_request(&cluster),
                sealing.as_ref(),
                Bound::for_test(),
            ) else {
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
        let Err(refused) = Mint::check(request(Some(&hex::encode(point))), None, Bound::for_test())
        else {
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
            Some(Route::ManifestByDigest(pushed))
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

    mod rule {
        //! A grant minted under a rule: it survives enforcement, and serves its values
        //! sealed from the attest key to the build's recipient, bound to its audience.

        use ployz_core::X25519Secret;

        use super::*;
        use crate::machine::local_machine::rule_grant::AdmittedRule;

        const AUDIENCE: &str = "ployz-build:rule-test";

        fn rule_grant(
            cluster: &Cluster,
            record: Option<SealedRecord>,
            recipient: &X25519Secret,
        ) -> Arc<Grant> {
            let admitted = AdmittedRule {
                recipient: recipient.recipient(),
                audience: AUDIENCE.to_owned(),
                rule: ployz_core::authority::RuleId::from_bytes([1; 16]),
                generation: 1,
                service: ployz_core::ServiceName::parse("web").unwrap(),
                commit: ployz_core::authority::CommitSha::from_bytes([2; 20]),
            };
            let request = MintBuildGrantRequest {
                sealed: record,
                ..request(Some(&hex::encode(holder().as_bytes())))
            };
            let mint =
                Mint::check_admitted(request, Some(&cluster.store()), Minter::Rule(admitted))
                    .unwrap();
            let grants = BuildGrants::default();
            grants
                .mint(
                    ManagementIdentity::from_bytes([7; 32]),
                    mint,
                    SocketAddr::from(([127, 0, 0, 1], 5000)),
                )
                .unwrap();
            grants.admit(holder().as_bytes()).unwrap()
        }

        async fn body(response: Response<Body>) -> (StatusCode, Vec<u8>) {
            let status = response.status();
            (
                status,
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            )
        }

        fn attest(cluster: &Cluster) -> ployz_core::authority::AttestKey {
            cluster.store().attest().unwrap().attest()
        }

        #[tokio::test]
        async fn a_rule_grant_serves_its_values_sealed_only_to_its_build_under_enforcement() {
            let cluster = Cluster::new();
            let recipient = X25519Secret::generate();
            let grant = rule_grant(&cluster, sealed_request(&cluster).sealed, &recipient);
            let (status, sealed) = body(sealed_of(&grant)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                !sealed
                    .windows(MARKER.len())
                    .any(|window| window == MARKER.as_bytes()),
                "the response is ciphertext"
            );
            let opened =
                OpenedSealed::open_build(&sealed, &recipient, &attest(&cluster), AUDIENCE).unwrap();
            assert_eq!(opened.get("NPM_TOKEN"), Some(MARKER));
            assert!(
                OpenedSealed::open_build(&sealed, &recipient, &attest(&Cluster::new()), AUDIENCE)
                    .is_err(),
                "another Cluster's attest key opens nothing"
            );
            assert!(
                OpenedSealed::open_build(&sealed, &recipient, &attest(&cluster), "ployz-build:x")
                    .is_err(),
                "another audience opens nothing"
            );
        }

        #[tokio::test]
        async fn a_rule_grant_without_secrets_still_proves_its_cluster() {
            let cluster = Cluster::new();
            let recipient = X25519Secret::generate();
            let grant = rule_grant(&cluster, None, &recipient);
            let (status, sealed) = body(sealed_of(&grant)).await;
            assert_eq!(status, StatusCode::OK);
            let opened =
                OpenedSealed::open_build(&sealed, &recipient, &attest(&cluster), AUDIENCE).unwrap();
            assert!(opened.is_empty());
        }

        #[tokio::test]
        async fn a_rule_grant_reaches_its_holder() {
            let cluster = Cluster::new();
            let grant = rule_grant(&cluster, None, &X25519Secret::generate());
            let request = Request::builder()
                .method(Method::GET)
                .uri("/ployz/sealed")
                .body(http_body_util::Empty::<Bytes>::new())
                .unwrap();
            let response = handle(
                request,
                &Serving::new(Arc::clone(&grant)).unwrap(),
                &reqwest::Client::new(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }
    }
}
