//! Image DTOs: listing, ingest, pull, prune and disk space.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::error::{RpcError, RpcErrorCode};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListImagesRequest {
    #[serde(default)]
    pub reference: Option<String>,
    /// Inspect each listed image for [`ImageSummary::last_tagged`]; one Docker
    /// read per image, so only narrow listings should ask.
    #[serde(default)]
    pub last_tagged: bool,
}

/// Empty payload of the command that returns this Machine's image-ingest TCP destination.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnsureImageIngestRequest {}

/// Named failure in `RpcError.details.reason` when image ingest cannot be opened.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageIngestReason {
    NotParticipating,
    DockerUnavailable,
    UnsupportedContainerdStore,
    ContainerdSocketMissing,
    StartFailed,
}

impl ImageIngestReason {
    /// The `reason` field of an ingest RPC error, if it is one of the frozen names.
    #[must_use]
    pub fn from_details(details: &Value) -> Option<Self> {
        details
            .get("reason")
            .and_then(|reason| Self::deserialize(reason).ok())
    }

    /// An RPC error that carries this reason in `details`.
    #[must_use]
    pub fn rpc_error(self, message: impl Into<String>) -> RpcError {
        RpcError {
            code: self.rpc_code(),
            message: message.into(),
            details: serde_json::json!({ "reason": self }),
            cause: Vec::new(),
        }
    }

    const fn rpc_code(self) -> RpcErrorCode {
        match self {
            Self::UnsupportedContainerdStore => RpcErrorCode::Unsupported,
            Self::StartFailed => RpcErrorCode::Internal,
            Self::NotParticipating | Self::DockerUnavailable | Self::ContainerdSocketMissing => {
                RpcErrorCode::Unavailable
            }
        }
    }
}

/// Management-plane TCP bind that accepts `docker push` and peer `docker pull`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageIngestDestination {
    /// Machine management-plane address that owns the ingest endpoint.
    pub management_address: crate::ManagementAddress,
    /// Plain-HTTP OCI Distribution port on the management plane.
    pub port: u16,
}

/// Successful `EnsureImageIngest` payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageIngestOpened {
    pub destination: ImageIngestDestination,
}

/// Pull one image from another Machine's image-ingest TCP destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PullImageFromMachineRequest {
    /// Select reference delivery or verified exact-content publication.
    pub pull: PeerImagePull,
    pub source: ImageIngestDestination,
    /// Platform the destination must receive, so a partial source cannot
    /// answer with an index whose selected variant it does not hold.
    pub platform: String,
}

/// Whether peer delivery follows a reference or publishes a tag for exact content.
/// Strict by the Stable promise's security exception: the mode selects digest
/// verification, so a stray field must not blur reference into publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum PeerImagePull {
    /// Pull this reference; digest references receive a deterministic retention tag.
    Reference { image: String },
    /// Pull pinned content and publish `tag` only after verifying its digest.
    Publish {
        image: crate::ImageDigestReference,
        tag: String,
    },
}

impl PeerImagePull {
    /// The source reference to fetch.
    #[must_use]
    pub fn image(&self) -> &str {
        match self {
            Self::Reference { image } => image,
            Self::Publish { image, .. } => image.as_str(),
        }
    }

    /// The requested destination tag, if this delivery publishes one.
    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        match self {
            Self::Reference { .. } => None,
            Self::Publish { tag, .. } => Some(tag),
        }
    }
}

/// Make one image present on this Machine as `policy` asks, pulling from its registry.
/// The image must be present afterwards, so `Never` fails for an absent image.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PullImageRequest {
    pub image: String,
    pub policy: crate::PullPolicy,
    /// Credentials for a private image; used for this pull only, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_auth: Option<crate::RegistryAuth>,
}

/// Successful `PullImage` and `PullImageFromMachine` payload.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImagePulled {
    /// The ports the image's config exposes, as Docker reports them. A Machine that
    /// doesn't report them sends none, which claims nothing about the image.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposed_ports: Vec<ExposedPort>,
}

/// One port an image's config exposes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ExposedPort {
    pub port: std::num::NonZeroU16,
    pub protocol: crate::TransportProtocol,
}

impl ExposedPort {
    /// Read one of Docker's exposed-port keys, such as `80/tcp`. Answers `None` for a
    /// protocol Ployz doesn't route (sctp) and for anything it can't read.
    #[must_use]
    pub fn from_docker(key: &str) -> Option<Self> {
        let (port, protocol) = key.split_once('/').unwrap_or((key, "tcp"));
        let protocol = match protocol {
            "tcp" => crate::TransportProtocol::Tcp,
            "udp" => crate::TransportProtocol::Udp,
            _ => return None,
        };
        Some(Self {
            port: port.parse().ok()?,
            protocol,
        })
    }
}

/// Remove the superseded build images of each repository from this Machine's store.
/// Repositories are Docker's short names, so a registry prefix stays. Never forced:
/// a reference whose image any Container uses is kept and reported in use.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PruneImagesRequest {
    pub repositories: Vec<String>,
}

/// One result per superseded reference found; empty in the steady state.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ImagesRemoved {
    pub results: Vec<ImageRemoval>,
}

/// What happened to one requested reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ImageRemoval {
    pub reference: String,
    pub outcome: ImageRemovalOutcome,
}

/// Per-reference removal result. Open: a status this build does not know decodes as
/// `unrecognized`, so a newer Machine never breaks an older client's whole report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ImageRemovalOutcome {
    /// The reference no longer exists on this Machine.
    Removed,
    /// A Container, running or not, uses the image; it was kept.
    InUse,
    /// The Machine had no such reference.
    NotFound,
    /// Docker refused for another reason; the reference may remain.
    Failed { message: String },
    /// A status introduced after this build.
    #[serde(other)]
    Unrecognized,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageSummary {
    pub id: String,
    pub repo_tags: Vec<String>,
    pub created: i64,
    pub size: i64,
    pub containers: i64,
    pub platforms: Vec<String>,
    /// Unix seconds this Machine last tagged the image, when the listing asked.
    /// Unlike `created`, it differs between reproducible builds.
    #[serde(default)]
    pub last_tagged: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineImages {
    pub containerd_store: bool,
    pub images: Vec<ImageSummary>,
    /// Docker-root filesystem space, when the Machine could read it.
    #[serde(default)]
    pub docker_root: Option<DiskSpace>,
}

/// One filesystem's size and free bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiskSpace {
    pub total_bytes: u64,
    pub free_bytes: u64,
}
