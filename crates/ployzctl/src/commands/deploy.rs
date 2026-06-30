use clap::Args;
use ployz_core::deploy::{
    DeployRequest, DeployRoute, DeployServiceSpec, ImageReference, ReplicaCount,
};
use ployz_core::ids::{NamespaceId, OperationId, RevisionId, ServiceId};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_sdk_types::{AcceptedOperation, DeploySubmitRequest};

use crate::client_ids::generate_client_deploy_id;
use crate::commands::{PloyzctlCliError, cli_error, invalid_value};

/// Public port the `--route HOST:PORT` shorthand listens on: alpha route
/// shorthand is plain HTTP (KTD8).
const ROUTE_SHORTHAND_PUBLIC_HTTP_PORT: u16 = 80;
/// Replica count when `--replicas` is omitted (R10).
const DEFAULT_REPLICA_COUNT: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployCommand {
    pub operation_id: OperationId,
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
    pub image: ImageReference,
    pub replicas: ReplicaCount,
    pub route: Option<DeployRoute>,
}

impl DeployCommand {
    #[must_use]
    pub fn into_request(self) -> DeploySubmitRequest {
        DeploySubmitRequest {
            operation_id: self.operation_id,
            target: DeployRequest {
                namespace_id: NamespaceId::try_new("default").expect("default namespace is valid"),
                target_revision: self.revision_id,
                services: vec![DeployServiceSpec {
                    service_id: self.service_id,
                    image: self.image,
                    replicas: self.replicas,
                    route: self.route,
                }],
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOutput {
    pub accepted: AcceptedOperation,
}

impl DeployOutput {
    #[must_use]
    pub const fn from_accepted(accepted: AcceptedOperation) -> Self {
        Self { accepted }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nwatch ployzctl ops watch {}\n",
            self.accepted.operation_id.as_str(),
            self.accepted.operation_id.as_str()
        )
    }
}

/// Builds the deploy request from the parsed flags. The alpha happy path is
/// `deploy --image IMAGE --route HOST:PORT` (R10): every internal field the
/// expert flags expose is derived from the image reference and command
/// intent (R11), and explicit flags override the derived values (R12).
pub(crate) fn deploy_command(parsed: DeployCli) -> Result<DeployCommand, PloyzctlCliError> {
    let DeployCli {
        operation,
        service,
        revision,
        image,
        replicas,
        route,
        route_hostname,
        route_port,
        endpoint_port,
    } = parsed;

    let image = ImageReference::try_new(image).map_err(|error| invalid_value("--image", error))?;
    let service_id = match service {
        Some(value) => {
            ServiceId::try_new(value).map_err(|error| invalid_value("--service", error))?
        }
        None => derive_service_id(&image)?,
    };
    let generated_ids = generate_client_deploy_id(&service_id)
        .map_err(|error| cli_error(format!("could not generate client operation ids: {error}")))?;
    let revision_id = match revision {
        Some(value) => {
            RevisionId::try_new(value).map_err(|error| invalid_value("--revision", error))?
        }
        None => derive_revision_id(&image, &generated_ids.suffix),
    };
    let operation_id = match operation {
        Some(value) => {
            OperationId::try_new(value).map_err(|error| invalid_value("--operation", error))?
        }
        None => generated_ids.operation_id,
    };
    let replicas = match replicas {
        Some(value) => parse_replicas(value)?,
        None => ReplicaCount::try_new(DEFAULT_REPLICA_COUNT)
            .expect("one replica is a valid replica count"),
    };
    let route = match route {
        Some(shorthand) => Some(parse_route_shorthand(&shorthand)?),
        None => parse_deploy_route(
            route_hostname
                .map(RouteHostname::try_new)
                .transpose()
                .map_err(|error| invalid_value("--route-hostname", error))?,
            route_port
                .map(|value| parse_port(value, "--route-port"))
                .transpose()?,
            endpoint_port
                .map(|value| parse_port(value, "--endpoint-port"))
                .transpose()?,
        )?,
    };

    Ok(DeployCommand {
        operation_id,
        service_id,
        revision_id,
        image,
        replicas,
        route,
    })
}

#[derive(Debug, Args)]
pub(crate) struct DeployCli {
    #[arg(long)]
    operation: Option<String>,
    #[arg(long)]
    service: Option<String>,
    #[arg(long)]
    revision: Option<String>,
    #[arg(long)]
    image: String,
    #[arg(long)]
    replicas: Option<String>,
    /// Route HOST on public HTTP port 80 to container endpoint PORT.
    #[arg(
        long,
        value_name = "HOST:PORT",
        conflicts_with_all = ["route_hostname", "route_port", "endpoint_port"]
    )]
    route: Option<String>,
    #[arg(long, requires_all = ["route_port", "endpoint_port"])]
    route_hostname: Option<String>,
    #[arg(long, requires_all = ["route_hostname", "endpoint_port"])]
    route_port: Option<String>,
    #[arg(long, requires_all = ["route_hostname", "route_port"])]
    endpoint_port: Option<String>,
}

/// Repository leaf and tag of an image reference, with any `@digest`
/// suffix removed: `ghcr.io/acme/web:latest` is `("web", Some("latest"))`,
/// `localhost:5000/web` is `("web", None)`, `redis:7` is `("redis",
/// Some("7"))`.
fn image_leaf_and_tag(image: &ImageReference) -> (&str, Option<&str>) {
    let reference = image.as_str();
    let without_digest = match reference.split_once('@') {
        Some((before_digest, _digest)) => before_digest,
        None => reference,
    };
    // Registry ports only appear before the first `/`, so any colon in the
    // last path segment separates the tag.
    let last_segment = match without_digest.rsplit_once('/') {
        Some((_path, segment)) => segment,
        None => without_digest,
    };
    match last_segment.split_once(':') {
        Some((leaf, tag)) => (leaf, Some(tag)),
        None => (last_segment, None),
    }
}

/// Derives the service id from the image repository leaf (R11). Leaves that
/// are not valid service ids (for example dotted repository names) fail
/// with the `--service` escape hatch instead of being silently rewritten.
fn derive_service_id(image: &ImageReference) -> Result<ServiceId, PloyzctlCliError> {
    let (leaf, _tag) = image_leaf_and_tag(image);
    ServiceId::try_new(leaf).map_err(|_| {
        cli_error(format!(
            "cannot derive a service id from image {}: repository leaf {leaf:?} is not a valid service id; pass --service",
            image.as_str()
        ))
    })
}

/// Derives a fresh revision id from the image tag plus the generated
/// suffix, so redeploying a moving tag like `latest` still mints a new
/// revision (R11, KTD9).
fn derive_revision_id(image: &ImageReference, suffix: &str) -> RevisionId {
    let (_leaf, tag) = image_leaf_and_tag(image);
    let sanitized_tag = tag
        .map(sanitize_id_fragment)
        .filter(|fragment| !fragment.is_empty());
    let value = match sanitized_tag {
        Some(tag) => format!("rev_{tag}_{suffix}"),
        None => format!("rev_{suffix}"),
    };
    RevisionId::try_new(value).expect("generated revision id uses subject-token characters")
}

/// Maps characters outside the subject-token alphabet to `-` so image tags
/// like `1.2.3` fit generated ids.
fn sanitize_id_fragment(fragment: &str) -> String {
    fragment
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// Parses the `--route HOST:PORT` shorthand (KTD8): HOST becomes the public
/// route hostname on HTTP port 80 and PORT is the container endpoint port.
fn parse_route_shorthand(value: &str) -> Result<DeployRoute, PloyzctlCliError> {
    let Some((host, port)) = value.rsplit_once(':') else {
        return Err(PloyzctlCliError::InvalidValue {
            flag: "--route",
            message: format!(
                "expected HOST:PORT (route hostname and container endpoint port), got {value:?}"
            ),
        });
    };
    let hostname = RouteHostname::try_new(host).map_err(|error| invalid_value("--route", error))?;
    let endpoint_port = port
        .parse::<u16>()
        .map_err(|_| PloyzctlCliError::InvalidValue {
            flag: "--route",
            message: format!("endpoint port {port:?} is not a valid port number"),
        })?;
    let endpoint_port =
        RoutePort::try_new(endpoint_port).map_err(|error| invalid_value("--route", error))?;
    Ok(DeployRoute {
        target: RouteTarget {
            hostname,
            port: RoutePort::try_new(ROUTE_SHORTHAND_PUBLIC_HTTP_PORT)
                .expect("80 is a valid route port"),
        },
        endpoint_port,
    })
}

fn parse_replicas(value: String) -> Result<ReplicaCount, PloyzctlCliError> {
    let value = value
        .parse::<u16>()
        .map_err(|error| PloyzctlCliError::InvalidValue {
            flag: "--replicas",
            message: error.to_string(),
        })?;
    ReplicaCount::try_new(value).map_err(|error| invalid_value("--replicas", error))
}

fn parse_port(value: String, flag: &'static str) -> Result<RoutePort, PloyzctlCliError> {
    let value = value
        .parse::<u16>()
        .map_err(|error| PloyzctlCliError::InvalidValue {
            flag,
            message: error.to_string(),
        })?;
    RoutePort::try_new(value).map_err(|error| invalid_value(flag, error))
}

fn parse_deploy_route(
    hostname: Option<RouteHostname>,
    port: Option<RoutePort>,
    endpoint_port: Option<RoutePort>,
) -> Result<Option<DeployRoute>, PloyzctlCliError> {
    match (hostname, port, endpoint_port) {
        (Some(hostname), Some(port), Some(endpoint_port)) => Ok(Some(DeployRoute {
            target: RouteTarget { hostname, port },
            endpoint_port,
        })),
        (None, None, None) => Ok(None),
        (Some(_), Some(_), None)
        | (None, None, Some(_))
        | (Some(_), None, Some(_))
        | (Some(_), None, None)
        | (None, Some(_), Some(_))
        | (None, Some(_), None) => Err(cli_error(
            "--route-hostname, --route-port, and --endpoint-port must be provided together",
        )),
    }
}
