use clap::{ArgAction, Parser};
use ployz_core::deploy::{DeployRequest, DeployRoute, ImageReference, ReplicaCount};
use ployz_core::ids::{OperationId, RevisionId, ServiceId};
use ployz_core::ops::{OperationIdempotencyKey, RouteHostname, RoutePort, RouteTarget};
use ployz_sdk_types::{AcceptedOperation, DeploySubmitRequest};

use crate::commands::{PloyzctlCliError, clap_error, cli_error, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedDeployCommand {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
    pub image: ImageReference,
    pub replicas: ReplicaCount,
    pub route: Option<DeployRoute>,
}

impl DetachedDeployCommand {
    #[must_use]
    pub fn into_request(self) -> DeploySubmitRequest {
        DeploySubmitRequest {
            operation_id: self.operation_id,
            idempotency_key: self.idempotency_key,
            target: DeployRequest {
                service_id: self.service_id,
                target_revision: self.revision_id,
                image: self.image,
                replicas: self.replicas,
                route: self.route,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedDeployOutput {
    pub accepted: AcceptedOperation,
}

impl DetachedDeployOutput {
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

pub fn parse_deploy_command(args: &[String]) -> Result<DetachedDeployCommand, PloyzctlCliError> {
    let parsed =
        DeployCli::try_parse_from(std::iter::once("deploy".to_owned()).chain(args.iter().cloned()))
            .map_err(clap_error)?;

    Ok(DetachedDeployCommand {
        operation_id: OperationId::try_new(parsed.operation)
            .map_err(|error| invalid_value("--operation", error))?,
        idempotency_key: OperationIdempotencyKey::try_new(parsed.idempotency_key)
            .map_err(|error| invalid_value("--idempotency-key", error))?,
        service_id: ServiceId::try_new(parsed.service)
            .map_err(|error| invalid_value("--service", error))?,
        revision_id: RevisionId::try_new(parsed.revision)
            .map_err(|error| invalid_value("--revision", error))?,
        image: ImageReference::try_new(parsed.image)
            .map_err(|error| invalid_value("--image", error))?,
        replicas: parse_replicas(parsed.replicas)?,
        route: parse_deploy_route(
            parsed
                .route_hostname
                .map(RouteHostname::try_new)
                .transpose()
                .map_err(|error| invalid_value("--route-hostname", error))?,
            parsed
                .route_port
                .map(|value| parse_port(value, "--route-port"))
                .transpose()?,
            parsed
                .endpoint_port
                .map(|value| parse_port(value, "--endpoint-port"))
                .transpose()?,
        )?,
    })
}

#[derive(Debug, Parser)]
#[command(name = "deploy")]
struct DeployCli {
    #[arg(long, action = ArgAction::SetTrue, required = true)]
    detach: bool,
    #[arg(long)]
    operation: String,
    #[arg(long)]
    idempotency_key: String,
    #[arg(long)]
    service: String,
    #[arg(long)]
    revision: String,
    #[arg(long)]
    image: String,
    #[arg(long)]
    replicas: String,
    #[arg(long, requires_all = ["route_port", "endpoint_port"])]
    route_hostname: Option<String>,
    #[arg(long, requires_all = ["route_hostname", "endpoint_port"])]
    route_port: Option<String>,
    #[arg(long, requires_all = ["route_hostname", "route_port"])]
    endpoint_port: Option<String>,
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
