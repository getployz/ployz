use std::fs;
use std::path::PathBuf;

use clap::Args;
use ployz_core::deploy::{
    DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec, ImageReference, ReplicaCount,
};
use ployz_core::ids::{NamespaceId, ServiceId};
use ployz_core::ops::{OperationIdempotencyKey, RouteHostname, RoutePort};
use ployz_sdk_types::{AcceptedOperation, DeploySubmitRequest};

use crate::client_ids::generate_client_deploy_id;
use crate::commands::{PloyzctlCliError, cli_error, invalid_value};
use crate::compose::{ComposeInput, UnsupportedFieldMode};

/// Public port the `--route HOST:PORT` shorthand listens on: alpha route
/// shorthand is plain HTTP (KTD8).
const ROUTE_SHORTHAND_PUBLIC_HTTP_PORT: u16 = 80;
/// Replica count when `--replicas` is omitted (R10).
const DEFAULT_REPLICA_COUNT: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployCommand {
    pub idempotency_key: OperationIdempotencyKey,
    pub namespace_id: NamespaceId,
    pub services: Vec<DeployServiceSpec>,
    pub warnings: Vec<String>,
    pub detach: bool,
}

impl DeployCommand {
    #[must_use]
    pub fn into_request(self) -> DeploySubmitRequest {
        DeploySubmitRequest {
            idempotency_key: self.idempotency_key,
            target: DeployRequest {
                namespace_id: self.namespace_id,
                services: self.services,
            },
        }
    }

    #[must_use]
    pub fn first_service(&self) -> Option<&DeployServiceSpec> {
        self.services.first()
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
            "operation {}\nwatch ployz ops watch {}\n",
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
        file,
        namespace,
        service,
        image,
        replicas,
        route,
        route_hostname,
        route_port,
        endpoint_port,
        allow_unsupported,
        detach,
    } = parsed;

    if let Some(file) = file {
        if image.is_some()
            || service.is_some()
            || replicas.is_some()
            || !route.is_empty()
            || route_hostname.is_some()
            || route_port.is_some()
            || endpoint_port.is_some()
        {
            return Err(cli_error(
                "deploy -f conflicts with --image, --service, --replicas, and route flags",
            ));
        }
        let source = fs::read_to_string(&file)
            .map_err(|error| cli_error(format!("could not read {}: {error}", file.display())))?;
        let base_dir = file.parent().unwrap_or_else(|| std::path::Path::new("."));
        let namespace_override = namespace
            .map(NamespaceId::try_new)
            .transpose()
            .map_err(|error| invalid_value("--namespace", error))?;
        let (parsed, warnings) = crate::compose::parse_deploy_file(ComposeInput {
            source: &source,
            base_dir,
            interpolation_env: crate::compose::interpolation_env(base_dir)?,
            namespace_override,
            mode: if allow_unsupported {
                UnsupportedFieldMode::AllowUnsupported
            } else {
                UnsupportedFieldMode::Strict
            },
        })?;
        let service_id = parsed
            .services
            .first()
            .map(|service| service.service_id.clone())
            .ok_or_else(|| cli_error("compose file must define at least one service"))?;
        let generated_ids = generate_client_deploy_id(&service_id).map_err(|error| {
            cli_error(format!("could not generate client operation ids: {error}"))
        })?;
        return Ok(DeployCommand {
            idempotency_key: generated_ids.idempotency_key,
            namespace_id: parsed.namespace_id,
            services: parsed.services,
            warnings: warnings.into_iter().map(|warning| warning.0).collect(),
            detach,
        });
    }
    if allow_unsupported {
        return Err(cli_error("--allow-unsupported requires -f"));
    }

    let image = image.ok_or_else(|| cli_error("--image is required unless -f is used"))?;
    let image = ImageReference::try_new(image).map_err(|error| invalid_value("--image", error))?;
    let namespace_id = namespace
        .map(NamespaceId::try_new)
        .transpose()
        .map_err(|error| invalid_value("--namespace", error))?
        .unwrap_or_else(|| NamespaceId::try_new("default").expect("default namespace is valid"));
    let service_id = match service {
        Some(value) => {
            ServiceId::try_new(value).map_err(|error| invalid_value("--service", error))?
        }
        None => derive_service_id(&image)?,
    };
    let generated_ids = generate_client_deploy_id(&service_id)
        .map_err(|error| cli_error(format!("could not generate client operation ids: {error}")))?;
    let replicas = match replicas {
        Some(value) => parse_replicas(value)?,
        None => ReplicaCount::try_new(DEFAULT_REPLICA_COUNT)
            .expect("one replica is a valid replica count"),
    };
    let routes = if route.is_empty() {
        parse_deploy_route(
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
        )?
        .into_iter()
        .collect()
    } else {
        route
            .iter()
            .map(|shorthand| parse_route_shorthand(shorthand))
            .collect::<Result<Vec<_>, _>>()?
    };

    Ok(DeployCommand {
        idempotency_key: generated_ids.idempotency_key,
        namespace_id,
        services: vec![DeployServiceSpec {
            service_id,
            image,
            replicas,
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            routes,
        }],
        warnings: Vec::new(),
        detach,
    })
}

#[derive(Debug, Args)]
pub(crate) struct DeployCli {
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    file: Option<PathBuf>,
    #[arg(short = 'n', long = "namespace")]
    namespace: Option<String>,
    #[arg(long, hide = true)]
    service: Option<String>,
    #[arg(long)]
    image: Option<String>,
    #[arg(long)]
    replicas: Option<String>,
    /// Route HOST on public HTTP port 80 to container endpoint PORT.
    /// Repeat to bind multiple hostnames to the same service.
    #[arg(
        long,
        value_name = "HOST:PORT",
        conflicts_with_all = ["route_hostname", "route_port", "endpoint_port"]
    )]
    route: Vec<String>,
    #[arg(long, requires_all = ["route_port", "endpoint_port"])]
    route_hostname: Option<String>,
    #[arg(long, requires_all = ["route_hostname", "endpoint_port"])]
    route_port: Option<String>,
    #[arg(long, requires_all = ["route_hostname", "route_port"])]
    endpoint_port: Option<String>,
    #[arg(long)]
    allow_unsupported: bool,
    #[arg(long)]
    detach: bool,
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

/// Parses the `--route HOST:PORT` shorthand (KTD8): HOST becomes the public
/// route hostname on HTTP port 80 and PORT is the container endpoint port.
pub(crate) fn parse_route_shorthand(value: &str) -> Result<DeployRoute, PloyzctlCliError> {
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
        target: DeployRouteTarget::Hostname {
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
            target: DeployRouteTarget::Hostname { hostname, port },
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
