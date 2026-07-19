use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use ployz_core::deploy::{
    DeployOrigin, DeployRequest, DeployReservationId, DeployRoute, DeployRouteTarget,
    DeployServiceSpec, ImageReference, ReplicaCount,
};
use ployz_core::ids::{NamespaceId, OperationId, ServiceId};
use ployz_core::operation::{OperationIdempotencyKey, RouteHostname, RoutePort};
use ployz_sdk_types::{
    AcceptedOperation, DeploySubmitRequest, SystemDeployRequest, SystemDeployTarget,
};

use crate::commands::{PloyzctlCliError, cli_error, invalid_value};
use crate::deploy::compose::{RenderedWarning, UnsupportedFieldMode};
use crate::execution_support::generate_client_deploy_id;

/// Public port the `--route HOST:PORT` shorthand listens on: alpha route
/// shorthand is plain HTTP (KTD8).
/// Replica count for the default replicated service mode.
const DEFAULT_REPLICA_COUNT: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployCommand {
    pub idempotency_key: OperationIdempotencyKey,
    pub target: DeployCommandTarget,
    pub warnings: Vec<String>,
    pub detach: bool,
    pub from_registry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployCommandTarget {
    Ordinary(DeployRequest),
    System(SystemDeployTarget),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploySubmissionRequest {
    Ordinary(DeploySubmitRequest),
    System(SystemDeployRequest),
}

impl DeployCommand {
    #[must_use]
    pub fn into_request(self, reservation_id: DeployReservationId) -> DeploySubmissionRequest {
        match self.target {
            DeployCommandTarget::Ordinary(target) => {
                DeploySubmissionRequest::Ordinary(DeploySubmitRequest {
                    registry_credentials: BTreeMap::new(),
                    idempotency_key: self.idempotency_key,
                    reservation_id,
                    target,
                })
            }
            DeployCommandTarget::System(target) => {
                DeploySubmissionRequest::System(SystemDeployRequest {
                    registry_credentials: BTreeMap::new(),
                    idempotency_key: self.idempotency_key,
                    reservation_id,
                    target,
                })
            }
        }
    }

    #[must_use]
    pub fn first_service(&self) -> Option<&DeployServiceSpec> {
        self.target.services().first()
    }

    #[must_use]
    pub fn reservation_namespace(&self) -> NamespaceId {
        match &self.target {
            DeployCommandTarget::Ordinary(target) => target.namespace_id.clone(),
            DeployCommandTarget::System(_) => ployz_core::namespace::reserved_system_namespace(),
        }
    }
}

impl DeployCommandTarget {
    #[must_use]
    pub fn services(&self) -> &[DeployServiceSpec] {
        match self {
            Self::Ordinary(target) => &target.services,
            Self::System(target) => &target.services,
        }
    }

    pub fn services_mut(&mut self) -> &mut Vec<DeployServiceSpec> {
        match self {
            Self::Ordinary(target) => &mut target.services,
            Self::System(target) => &mut target.services,
        }
    }

    #[must_use]
    pub fn origin(&self) -> Option<&DeployOrigin> {
        match self {
            Self::Ordinary(target) => target.origin.as_ref(),
            Self::System(target) => target.origin.as_ref(),
        }
    }

    #[must_use]
    pub fn ordinary(&self) -> Option<&DeployRequest> {
        match self {
            Self::Ordinary(target) => Some(target),
            Self::System(_) => None,
        }
    }
}

impl DeploySubmissionRequest {
    #[must_use]
    pub fn services(&self) -> &[DeployServiceSpec] {
        match self {
            Self::Ordinary(request) => &request.target.services,
            Self::System(request) => &request.target.services,
        }
    }

    pub fn set_registry_credentials(
        &mut self,
        credentials: BTreeMap<ServiceId, ployz_core::image::RegistryCredential>,
    ) {
        match self {
            Self::Ordinary(request) => request.registry_credentials = credentials,
            Self::System(request) => request.registry_credentials = credentials,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployHistoryCommand {
    pub namespace_id: NamespaceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployRollbackCommand {
    pub namespace_id: NamespaceId,
    pub selection: DeployRollbackSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployRollbackSelection {
    Interactive,
    Operation(OperationId),
    LastGood,
}

pub(crate) enum ParsedDeployCommand {
    Deploy(DeployCommand),
    History(DeployHistoryCommand),
    Rollback(DeployRollbackCommand),
}

#[derive(Debug, Args)]
pub(crate) struct SystemDeployCli {
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    file: PathBuf,
    #[arg(long)]
    allow_unsupported: bool,
    #[arg(long)]
    detach: bool,
    #[arg(long)]
    from_registry: bool,
    #[arg(long, value_name = "CAPTION")]
    origin: Option<String>,
}

pub(crate) fn system_deploy_command(
    parsed: SystemDeployCli,
) -> Result<DeployCommand, PloyzctlCliError> {
    let origin = parsed
        .origin
        .map(DeployOrigin::try_new)
        .transpose()
        .map_err(|error| invalid_value("--origin", error))?;
    let (mut target, warnings) = crate::deploy::compose::parse_compose_file(
        &parsed.file,
        Some(ployz_core::namespace::reserved_system_namespace()),
        if parsed.allow_unsupported {
            UnsupportedFieldMode::AllowUnsupported
        } else {
            UnsupportedFieldMode::Strict
        },
    )?;
    target.origin = origin;
    let target = system_target_from_compose(target)?;
    compose_deploy_command(
        DeployCommandTarget::System(target),
        warnings,
        parsed.detach,
        parsed.from_registry,
    )
}

fn compose_deploy_command(
    target: DeployCommandTarget,
    warnings: Vec<RenderedWarning>,
    detach: bool,
    from_registry: bool,
) -> Result<DeployCommand, PloyzctlCliError> {
    let service_id = target
        .services()
        .first()
        .map(|service| service.service_id.clone())
        .ok_or_else(|| cli_error("compose file must define at least one service"))?;
    let generated_ids = generate_client_deploy_id(&service_id)
        .map_err(|error| cli_error(format!("could not generate client operation ids: {error}")))?;
    Ok(DeployCommand {
        idempotency_key: generated_ids.idempotency_key,
        target,
        warnings: warnings.into_iter().map(|warning| warning.0).collect(),
        detach,
        from_registry,
    })
}

fn system_target_from_compose(
    target: DeployRequest,
) -> Result<SystemDeployTarget, PloyzctlCliError> {
    if let Some(service) = target
        .services
        .iter()
        .find(|service| !service.runtime.volume_mounts.is_empty())
    {
        return Err(invalid_value(
            "-f",
            format!(
                "system deploy does not support Compose volume mounts on service {}",
                service.service_id.as_str()
            ),
        ));
    }
    if !target.volumes.is_empty() {
        return Err(invalid_value(
            "-f",
            "system deploy does not support top-level Compose volume declarations",
        ));
    }
    Ok(SystemDeployTarget {
        origin: target.origin,
        services: target.services,
    })
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct DeployRootCli {
    #[command(flatten)]
    deploy: DeployCli,
    #[command(subcommand)]
    command: Option<DeploySubcommand>,
}

#[derive(Debug, Subcommand)]
enum DeploySubcommand {
    History(DeployHistoryCli),
    /// Replay a successful digest-pinned deploy from local history.
    Rollback(DeployRollbackCli),
}

#[derive(Debug, Args)]
struct DeployHistoryCli {
    #[arg(short = 'n', long = "namespace")]
    namespace: Option<String>,
}

#[derive(Debug, Args)]
struct DeployRollbackCli {
    #[arg(short = 'n', long = "namespace")]
    namespace: Option<String>,
    /// Replay the successful payload recorded for this operation.
    #[arg(long = "to", value_name = "OPERATION_ID", conflicts_with = "last_good")]
    to: Option<String>,
    /// Replay the successful payload immediately before the newest one.
    #[arg(long)]
    last_good: bool,
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
pub(crate) fn deploy_command(
    parsed: DeployRootCli,
) -> Result<ParsedDeployCommand, PloyzctlCliError> {
    match parsed.command {
        Some(DeploySubcommand::History(history)) => {
            deploy_history_command(history).map(ParsedDeployCommand::History)
        }
        Some(DeploySubcommand::Rollback(rollback)) => {
            deploy_rollback_command(rollback).map(ParsedDeployCommand::Rollback)
        }
        None => deploy_submit_command(parsed.deploy).map(ParsedDeployCommand::Deploy),
    }
}

fn deploy_history_command(
    parsed: DeployHistoryCli,
) -> Result<DeployHistoryCommand, PloyzctlCliError> {
    let namespace_id = namespace_id_or_default(parsed.namespace)?;
    Ok(DeployHistoryCommand { namespace_id })
}

fn deploy_rollback_command(
    parsed: DeployRollbackCli,
) -> Result<DeployRollbackCommand, PloyzctlCliError> {
    let namespace_id = namespace_id_or_default(parsed.namespace)?;
    let operation = parsed
        .to
        .map(OperationId::try_new)
        .transpose()
        .map_err(|error| invalid_value("--to", error))?;
    let selection = if parsed.last_good {
        DeployRollbackSelection::LastGood
    } else {
        operation.map_or(
            DeployRollbackSelection::Interactive,
            DeployRollbackSelection::Operation,
        )
    };
    Ok(DeployRollbackCommand {
        namespace_id,
        selection,
    })
}

fn deploy_submit_command(parsed: DeployCli) -> Result<DeployCommand, PloyzctlCliError> {
    let DeployCli {
        file,
        namespace,
        service,
        image,
        replicas,
        global,
        route,
        route_hostname,
        endpoint_port,
        allow_unsupported,
        detach,
        from_registry,
        origin,
    } = parsed;
    let origin = origin
        .map(DeployOrigin::try_new)
        .transpose()
        .map_err(|error| invalid_value("--origin", error))?;

    if let Some(file) = file {
        if image.is_some()
            || service.is_some()
            || replicas.is_some()
            || global
            || !route.is_empty()
            || route_hostname.is_some()
            || endpoint_port.is_some()
        {
            return Err(cli_error(
                "deploy -f conflicts with --image, --service, --replicas, --global, and route flags",
            ));
        }
        let namespace_override = namespace
            .map(NamespaceId::try_new)
            .transpose()
            .map_err(|error| invalid_value("--namespace", error))?;
        let (mut target, warnings) = crate::deploy::compose::parse_compose_file(
            &file,
            namespace_override,
            if allow_unsupported {
                UnsupportedFieldMode::AllowUnsupported
            } else {
                UnsupportedFieldMode::Strict
            },
        )?;
        target.origin = origin;
        return compose_deploy_command(
            DeployCommandTarget::Ordinary(target),
            warnings,
            detach,
            from_registry,
        );
    }
    if allow_unsupported {
        return Err(cli_error("--allow-unsupported requires -f"));
    }

    let image = image.ok_or_else(|| cli_error("--image is required unless -f is used"))?;
    let image = ImageReference::try_new(image).map_err(|error| invalid_value("--image", error))?;
    let namespace_id = namespace_id_or_default(namespace)?;
    let service_id = match service {
        Some(value) => {
            ServiceId::try_new(value).map_err(|error| invalid_value("--service", error))?
        }
        None => derive_service_id(&image)?,
    };
    let generated_ids = generate_client_deploy_id(&service_id)
        .map_err(|error| cli_error(format!("could not generate client operation ids: {error}")))?;
    let mode = if global {
        ployz_core::deploy::ServiceMode::Global
    } else {
        let replicas = match replicas {
            Some(value) => parse_replicas(value)?,
            None => ReplicaCount::try_new(DEFAULT_REPLICA_COUNT)
                .expect("one replica is a valid replica count"),
        };
        ployz_core::deploy::ServiceMode::Replicated { replicas }
    };
    let routes = if route.is_empty() {
        parse_deploy_route(
            route_hostname
                .map(RouteHostname::try_new)
                .transpose()
                .map_err(|error| invalid_value("--route-hostname", error))?,
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
        target: DeployCommandTarget::Ordinary(DeployRequest {
            namespace_id,
            origin,
            volumes: BTreeMap::new(),
            services: vec![DeployServiceSpec {
                keep: None,
                service_id,
                image,
                image_source: ployz_core::deploy::ImageSource::Registry,
                mode,
                runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes,
            }],
        }),
        warnings: Vec::new(),
        detach,
        from_registry,
    })
}

fn namespace_id_or_default(namespace: Option<String>) -> Result<NamespaceId, PloyzctlCliError> {
    let Some(namespace) = namespace else {
        return Ok(NamespaceId::try_new("default").expect("default namespace is valid"));
    };
    NamespaceId::try_new(namespace).map_err(|error| invalid_value("--namespace", error))
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
    #[arg(long, conflicts_with = "replicas")]
    global: bool,
    /// Resolve the image from its registry even when it exists in local Docker.
    #[arg(long)]
    from_registry: bool,
    /// Route HOST on the fixed ingress listeners to container endpoint PORT.
    /// Repeat to bind multiple hostnames to the same service.
    #[arg(
        long,
        value_name = "HOST:PORT",
        conflicts_with_all = ["route_hostname", "endpoint_port"]
    )]
    route: Vec<String>,
    #[arg(long, requires = "endpoint_port")]
    route_hostname: Option<String>,
    #[arg(long, requires = "route_hostname")]
    endpoint_port: Option<String>,
    #[arg(long)]
    allow_unsupported: bool,
    #[arg(long)]
    detach: bool,
    #[arg(long, value_name = "CAPTION")]
    origin: Option<String>,
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

/// Parses `--route HOST:PORT`: HOST is the declared route hostname and PORT is
/// the container endpoint port. Ingress owns the fixed public listeners.
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
        target: DeployRouteTarget::Hostname { hostname },
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
    endpoint_port: Option<RoutePort>,
) -> Result<Option<DeployRoute>, PloyzctlCliError> {
    match (hostname, endpoint_port) {
        (Some(hostname), Some(endpoint_port)) => Ok(Some(DeployRoute {
            target: DeployRouteTarget::Hostname { hostname },
            endpoint_port,
        })),
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => Err(cli_error(
            "--route-hostname and --endpoint-port must be provided together",
        )),
    }
}
