use ployz_core::deploy::{DeployRequest, DeployRoute, ImageReference, ReplicaCount};
use ployz_core::ids::{OperationId, RevisionId, ServiceId};
use ployz_core::ops::{OperationIdempotencyKey, RouteHostname, RoutePort, RouteTarget};
use ployz_sdk_types::{AcceptedOperation, DeploySubmitRequest};

use crate::commands::{ArgCursor, PloyzctlCliError, invalid_value, required, set_once};

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
    let mut detached = false;
    let mut operation_id = None;
    let mut idempotency_key = None;
    let mut service_id = None;
    let mut revision_id = None;
    let mut image = None;
    let mut replicas = None;
    let mut route_hostname = None;
    let mut route_port = None;
    let mut endpoint_port = None;
    let mut args = ArgCursor::new(args);

    while !args.is_empty() {
        if args.take_flag("--detach") {
            if detached {
                return Err(PloyzctlCliError::DuplicateArgument { flag: "--detach" });
            }
            detached = true;
            continue;
        }
        if let Some(value) = args.take_value("--operation")? {
            let parsed =
                OperationId::try_new(value).map_err(|error| invalid_value("--operation", error))?;
            set_once(&mut operation_id, parsed, "--operation")?;
            continue;
        }
        if let Some(value) = args.take_value("--idempotency-key")? {
            let parsed = OperationIdempotencyKey::try_new(value)
                .map_err(|error| invalid_value("--idempotency-key", error))?;
            set_once(&mut idempotency_key, parsed, "--idempotency-key")?;
            continue;
        }
        if let Some(value) = args.take_value("--service")? {
            let parsed =
                ServiceId::try_new(value).map_err(|error| invalid_value("--service", error))?;
            set_once(&mut service_id, parsed, "--service")?;
            continue;
        }
        if let Some(value) = args.take_value("--revision")? {
            let parsed =
                RevisionId::try_new(value).map_err(|error| invalid_value("--revision", error))?;
            set_once(&mut revision_id, parsed, "--revision")?;
            continue;
        }
        if let Some(value) = args.take_value("--image")? {
            let parsed =
                ImageReference::try_new(value).map_err(|error| invalid_value("--image", error))?;
            set_once(&mut image, parsed, "--image")?;
            continue;
        }
        if let Some(value) = args.take_value("--replicas")? {
            let parsed = parse_replicas(value)?;
            set_once(&mut replicas, parsed, "--replicas")?;
            continue;
        }
        if let Some(value) = args.take_value("--route-hostname")? {
            let parsed = RouteHostname::try_new(value)
                .map_err(|error| invalid_value("--route-hostname", error))?;
            set_once(&mut route_hostname, parsed, "--route-hostname")?;
            continue;
        }
        if let Some(value) = args.take_value("--route-port")? {
            let parsed = parse_route_port(value)?;
            set_once(&mut route_port, parsed, "--route-port")?;
            continue;
        }
        if let Some(value) = args.take_value("--endpoint-port")? {
            let parsed = parse_port(value, "--endpoint-port")?;
            set_once(&mut endpoint_port, parsed, "--endpoint-port")?;
            continue;
        }
        return Err(args.unexpected());
    }

    if !detached {
        return Err(PloyzctlCliError::MissingRequiredArgument { flag: "--detach" });
    }

    Ok(DetachedDeployCommand {
        operation_id: required(operation_id, "--operation")?,
        idempotency_key: required(idempotency_key, "--idempotency-key")?,
        service_id: required(service_id, "--service")?,
        revision_id: required(revision_id, "--revision")?,
        image: required(image, "--image")?,
        replicas: required(replicas, "--replicas")?,
        route: parse_deploy_route(route_hostname, route_port, endpoint_port)?,
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

fn parse_route_port(value: String) -> Result<RoutePort, PloyzctlCliError> {
    parse_port(value, "--route-port")
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
        (Some(_), Some(_), None) => Err(PloyzctlCliError::MissingRequiredArgument {
            flag: "--endpoint-port",
        }),
        (None, None, Some(_)) => Err(PloyzctlCliError::MissingRequiredArgument {
            flag: "--route-hostname",
        }),
        (Some(_), None, Some(_)) | (Some(_), None, None) => {
            Err(PloyzctlCliError::MissingRequiredArgument {
                flag: "--route-port",
            })
        }
        (None, Some(_), Some(_)) | (None, Some(_), None) => {
            Err(PloyzctlCliError::MissingRequiredArgument {
                flag: "--route-hostname",
            })
        }
    }
}
