use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "docker-runtime")]
use mvp_node::load_node;
use mvp_node::{NodeError, NodeResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DeployRuntimeArgs {
    Process,
    Docker {
        image: String,
        service_port: u16,
        command: Option<Vec<String>>,
    },
}

pub(crate) fn container_shell_command(value: &str) -> Vec<String> {
    vec!["sh".to_string(), "-c".to_string(), value.to_string()]
}

#[cfg(feature = "docker-runtime")]
pub(crate) fn docker_runtime_backend(
    state_dir: Option<&PathBuf>,
    image: &str,
    service_port: u16,
    command: Option<&[String]>,
) -> NodeResult<Option<Arc<dyn mvp_runtime::RuntimeBackend>>> {
    let state_dir = state_dir.ok_or(NodeError::MissingFlagValue { flag: "--state" })?;
    let state = load_node(state_dir)?;
    let mut config = mvp_runtime::DockerRuntimeConfig::new(
        state.node_id(),
        state.paths().runtime_dir.clone(),
        image,
    )
    .with_service_port(service_port)
    .with_dns_server(state.container_subnet().docker_gateway_ip().to_string());
    if let Some(command) = command {
        config = config.with_command(command.iter().cloned());
    }
    let network =
        mvp_runtime::DockerBridgeNetwork::connect(mvp_runtime::DockerBridgeNetworkConfig::new(
            format!("ployz-mvp-{}", state.node_id_str()),
            state.container_subnet(),
        ))
        .map_err(|source| NodeError::RuntimeBackend { source })?;
    let runtime =
        mvp_runtime::DockerRuntime::connect_with_container_network(config, Arc::new(network))
            .map_err(|source| NodeError::RuntimeBackend { source })?;
    Ok(Some(Arc::new(runtime)))
}

#[cfg(not(feature = "docker-runtime"))]
pub(crate) fn docker_runtime_backend(
    _state_dir: Option<&PathBuf>,
    _image: &str,
    _service_port: u16,
    _command: Option<&[String]>,
) -> NodeResult<Option<Arc<dyn mvp_runtime::RuntimeBackend>>> {
    Err(NodeError::CommandNotWired {
        command: "deploy --runtime docker requires the docker-runtime feature".to_string(),
    })
}
