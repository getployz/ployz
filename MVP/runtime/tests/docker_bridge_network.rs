#![cfg(feature = "docker")]

use std::time::{SystemTime, UNIX_EPOCH};

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder};
use futures_util::StreamExt;
use mvp_mesh::ContainerSubnet;
use mvp_runtime::{
    ContainerNetworkBackend, DockerBridgeNetwork, DockerBridgeNetworkConfig, RuntimeError,
};

#[test]
fn docker_bridge_creates_inspects_and_removes_network() {
    let network = match docker_bridge("lifecycle", "10.210.81.0/24") {
        Ok(network) => network,
        Err(error) => {
            eprintln!("skipping Docker bridge integration test: {error}");
            return;
        }
    };

    let state = network.ensure().expect("ensure bridge network");

    assert_eq!(state.subnet.to_string(), "10.210.81.0/24");
    assert_eq!(state.gateway_ip.to_string(), "10.210.81.1");
    assert_eq!(state.first_service_ip.to_string(), "10.210.81.2");
    assert!(state.bridge_ifname.as_deref().is_some_and(|name| name.starts_with("br-")));

    let second = network.ensure().expect("ensure bridge network again");
    assert_eq!(second.name, state.name);

    network.remove().expect("remove bridge network");
}

#[test]
fn docker_bridge_connects_container_at_static_ip() {
    let image = "busybox:latest";
    let network = match docker_bridge("attach", "10.210.82.0/24") {
        Ok(network) => network,
        Err(error) => {
            eprintln!("skipping Docker bridge integration test: {error}");
            return;
        }
    };
    let docker = Docker::connect_with_socket_defaults().expect("docker client");
    let executor = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("docker test runtime");
    let container_name = unique_name("ployz-mvp-bridge-test-container");

    let result = executor.block_on(async {
        ensure_image(&docker, image).await?;
        network.ensure()?;
        create_sleeping_container(&docker, &container_name, image).await?;
        let attachment = network.connect(&container_name, Some(state_ip(&network)?))?;
        assert_eq!(attachment.network_name, network.state()?.name);
        assert_eq!(
            attachment.ipv4.map(|ip| ip.to_string()),
            Some("10.210.82.2".to_string())
        );
        network.disconnect(&container_name, true)?;
        Ok::<(), RuntimeError>(())
    });
    let _ = executor.block_on(remove_container(&docker, &container_name));
    let _ = network.remove();
    result.expect("connect static container IP");
}

fn docker_bridge(label: &str, subnet: &str) -> Result<DockerBridgeNetwork, RuntimeError> {
    let subnet =
        ContainerSubnet::new(subnet.parse().expect("valid test subnet")).expect("test subnet");
    let network = DockerBridgeNetwork::connect(DockerBridgeNetworkConfig::new(
        unique_name(&format!("ployz-mvp-bridge-test-{label}")),
        subnet,
    ))?;
    Ok(network)
}

fn state_ip(network: &DockerBridgeNetwork) -> Result<mvp_mesh::ContainerIp, RuntimeError> {
    Ok(network.state()?.first_service_ip)
}

async fn ensure_image(docker: &Docker, image: &str) -> Result<(), RuntimeError> {
    if docker.inspect_image(image).await.is_ok() {
        return Ok(());
    }
    let options = bollard::query_parameters::CreateImageOptionsBuilder::default()
        .from_image(image)
        .build();
    let mut stream = docker.create_image(Some(options), None, None);
    while let Some(result) = stream.next().await {
        result.map_err(|source| RuntimeError::DockerOperation {
            operation: "docker pull",
            message: source.to_string(),
        })?;
    }
    Ok(())
}

async fn create_sleeping_container(
    docker: &Docker,
    name: &str,
    image: &str,
) -> Result<(), RuntimeError> {
    let body = ContainerCreateBody {
        image: Some(image.to_string()),
        cmd: Some(vec!["sleep".to_string(), "60".to_string()]),
        host_config: Some(HostConfig::default()),
        ..Default::default()
    };
    let options = CreateContainerOptionsBuilder::default().name(name).build();
    docker
        .create_container(Some(options), body)
        .await
        .map_err(|source| RuntimeError::DockerOperation {
            operation: "docker create test container",
            message: source.to_string(),
        })?;
    docker
        .start_container(name, None)
        .await
        .map_err(|source| RuntimeError::DockerOperation {
            operation: "docker start test container",
            message: source.to_string(),
        })
}

async fn remove_container(docker: &Docker, name: &str) -> Result<(), RuntimeError> {
    let options = RemoveContainerOptionsBuilder::default().force(true).build();
    match docker.remove_container(name, Some(options)).await {
        Ok(()) => Ok(()),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(()),
        Err(source) => Err(RuntimeError::DockerOperation {
            operation: "docker remove test container",
            message: source.to_string(),
        }),
    }
}

fn unique_name(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    format!("{prefix}-{}-{now}", std::process::id())
}
