use std::collections::BTreeMap;

use ployz_core::deploy::{DeployServiceSpec, ImageReference, ReplicaCount};
use ployz_core::ids::{NamespaceId, ServiceId};
use serde::Deserialize;

use crate::commands::deploy::parse_route_shorthand;
use crate::commands::{PloyzctlCliError, cli_error, invalid_value};

const DEFAULT_REPLICA_COUNT: u16 = 1;

pub(crate) fn parse_deploy_file(
    source: &str,
    namespace_override: Option<NamespaceId>,
) -> Result<ParsedComposeDeploy, PloyzctlCliError> {
    let compose: ComposeFile = serde_yaml::from_str(source)
        .map_err(|error| cli_error(format!("invalid compose YAML: {error}")))?;
    let namespace_id = match namespace_override {
        Some(namespace_id) => namespace_id,
        None => {
            let Some(name) = compose.name else {
                return Err(cli_error(
                    "compose file needs top-level name or deploy -n NAMESPACE",
                ));
            };
            NamespaceId::try_new(name).map_err(|error| invalid_value("name", error))?
        }
    };
    let mut services = Vec::with_capacity(compose.services.len());
    for (name, service) in compose.services {
        let service_id =
            ServiceId::try_new(name).map_err(|error| invalid_value("services", error))?;
        let image = ImageReference::try_new(service.image)
            .map_err(|error| invalid_value("services.*.image", error))?;
        let replicas = ReplicaCount::try_new(
            service
                .deploy
                .and_then(|deploy| deploy.replicas)
                .unwrap_or(DEFAULT_REPLICA_COUNT),
        )
        .map_err(|error| invalid_value("services.*.deploy.replicas", error))?;
        let routes = service
            .x_route
            .map(ComposeRoutes::into_shorthands)
            .unwrap_or_default()
            .iter()
            .map(|shorthand| parse_route_shorthand(shorthand))
            .collect::<Result<Vec<_>, _>>()?;
        services.push(DeployServiceSpec {
            service_id,
            image,
            replicas,
            routes,
        });
    }
    if services.is_empty() {
        return Err(cli_error("compose file must define at least one service"));
    }
    Ok(ParsedComposeDeploy {
        namespace_id,
        services,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedComposeDeploy {
    pub namespace_id: NamespaceId,
    pub services: Vec<DeployServiceSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposeFile {
    name: Option<String>,
    services: BTreeMap<String, ComposeService>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposeService {
    image: String,
    deploy: Option<ComposeDeploy>,
    #[serde(rename = "x-route")]
    x_route: Option<ComposeRoutes>,
}

/// `x-route` accepts one shorthand or a list of shorthands, so one service
/// can bind multiple hostnames (AE5).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ComposeRoutes {
    One(String),
    Many(Vec<String>),
}

impl ComposeRoutes {
    fn into_shorthands(self) -> Vec<String> {
        match self {
            Self::One(shorthand) => vec![shorthand],
            Self::Many(shorthands) => shorthands,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposeDeploy {
    replicas: Option<u16>,
}
