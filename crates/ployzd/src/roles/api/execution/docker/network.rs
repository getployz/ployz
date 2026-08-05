use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::models::{
    ContainerSummaryStateEnum, Ipam, IpamConfig, NetworkConnectRequest, NetworkCreateRequest,
    NetworkDisconnectRequest, NetworkInspect,
};
use bollard::query_parameters::{
    InspectNetworkOptions, ListContainersOptionsBuilder, RestartContainerOptions,
};
use ployz_core::network::{EndpointBridgeStatus, MachineEndpointSubnet};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::labels::MANAGED_LABEL;
use crate::roles::api::runner::{
    MachineContainerCreateError, MachineEndpointNetworkConvergenceError,
    MachineEndpointNetworkConvergenceOutcome, MachineEndpointNetworkError,
};

pub(super) const ENDPOINT_NETWORK_NAME: &str = "ployz";
pub(super) const DRIVER_MTU_OPTION: &str = "com.docker.network.driver.mtu";

pub(super) fn endpoint_network_create_request(
    endpoint_network_subnet: &MachineEndpointSubnet,
    endpoint_bridge_ifname: &str,
    endpoint_mtu: u32,
) -> NetworkCreateRequest {
    NetworkCreateRequest {
        name: ENDPOINT_NETWORK_NAME.to_owned(),
        driver: Some("bridge".to_owned()),
        options: Some(HashMap::from([
            (
                "com.docker.network.bridge.name".to_owned(),
                endpoint_bridge_ifname.to_owned(),
            ),
            (DRIVER_MTU_OPTION.to_owned(), endpoint_mtu.to_string()),
        ])),
        ipam: Some(Ipam {
            driver: Some("default".to_owned()),
            config: Some(vec![IpamConfig {
                subnet: Some(endpoint_network_subnet.as_string()),
                gateway: Some(endpoint_network_subnet.bridge_gateway_ipv4().to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        labels: Some(HashMap::from([(
            MANAGED_LABEL.to_owned(),
            "true".to_owned(),
        )])),
        ..Default::default()
    }
}

pub(super) async fn ensure_endpoint_network(
    docker: &Docker,
    endpoint_network_subnet: &MachineEndpointSubnet,
    endpoint_bridge_ifname: &str,
    endpoint_mtu: u32,
) -> Result<(), MachineEndpointNetworkError> {
    match docker
        .inspect_network(ENDPOINT_NETWORK_NAME, None::<InspectNetworkOptions>)
        .await
    {
        Ok(network) => {
            return validate_endpoint_network(
                &network,
                endpoint_network_subnet,
                endpoint_bridge_ifname,
                endpoint_mtu,
            );
        }
        Err(error) if is_docker_object_missing(&error) => {}
        Err(error) => {
            return Err(MachineEndpointNetworkError::EnsureEndpointNetwork {
                message: format!("inspect Docker network {ENDPOINT_NETWORK_NAME}: {error}"),
            });
        }
    }

    let request = endpoint_network_create_request(
        endpoint_network_subnet,
        endpoint_bridge_ifname,
        endpoint_mtu,
    );
    match docker.create_network(request).await {
        Ok(_) => {}
        Err(error) if is_network_already_exists(&error) => {}
        Err(error) => {
            return Err(MachineEndpointNetworkError::EnsureEndpointNetwork {
                message: format!("create Docker network {ENDPOINT_NETWORK_NAME}: {error}"),
            });
        }
    }

    let network = docker
        .inspect_network(ENDPOINT_NETWORK_NAME, None::<InspectNetworkOptions>)
        .await
        .map_err(|error| MachineEndpointNetworkError::EnsureEndpointNetwork {
            message: format!(
                "inspect Docker network {ENDPOINT_NETWORK_NAME} after create: {error}"
            ),
        })?;
    validate_endpoint_network(
        &network,
        endpoint_network_subnet,
        endpoint_bridge_ifname,
        endpoint_mtu,
    )
}

pub(super) async fn converge_endpoint_network(
    docker: &Docker,
    endpoint_network_subnet: &MachineEndpointSubnet,
    endpoint_bridge_ifname: &str,
    endpoint_mtu: u32,
) -> Result<MachineEndpointNetworkConvergenceOutcome, MachineEndpointNetworkConvergenceError> {
    let observed = match docker
        .inspect_network(ENDPOINT_NETWORK_NAME, None::<InspectNetworkOptions>)
        .await
    {
        Ok(network) => Some(network),
        Err(error) if is_docker_object_missing(&error) => None,
        Err(error) => {
            return Err(MachineEndpointNetworkConvergenceError::Inspect {
                message: error.to_string(),
            });
        }
    };
    if observed.as_ref().is_some_and(|network| {
        network
            .labels
            .as_ref()
            .and_then(|labels| labels.get(MANAGED_LABEL))
            .map(String::as_str)
            != Some("true")
    }) {
        return Err(MachineEndpointNetworkConvergenceError::UnmanagedNetwork);
    }

    let managed = managed_endpoint_containers(docker).await?;
    let attached = observed
        .as_ref()
        .and_then(|network| network.containers.as_ref())
        .map(|containers| containers.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let managed_ids = managed.keys().cloned().collect::<BTreeSet<_>>();
    let unmanaged = attached
        .difference(&managed_ids)
        .cloned()
        .collect::<Vec<_>>();
    if !unmanaged.is_empty() {
        return Err(
            MachineEndpointNetworkConvergenceError::UnmanagedAttachments {
                container_ids: unmanaged,
            },
        );
    }

    let network_matches = observed.as_ref().is_some_and(|network| {
        validate_endpoint_network(
            network,
            endpoint_network_subnet,
            endpoint_bridge_ifname,
            endpoint_mtu,
        )
        .is_ok()
    });
    let missing_before = managed_ids
        .difference(&attached)
        .cloned()
        .collect::<BTreeSet<_>>();
    if network_matches && missing_before.is_empty() {
        return Ok(MachineEndpointNetworkConvergenceOutcome::Unchanged);
    }

    let recreated = observed.is_some() && !network_matches;
    let created = observed.is_none();
    if recreated {
        for container_id in &attached {
            docker
                .disconnect_network(
                    ENDPOINT_NETWORK_NAME,
                    NetworkDisconnectRequest {
                        container: container_id.clone(),
                        force: Some(true),
                    },
                )
                .await
                .map_err(|error| MachineEndpointNetworkConvergenceError::Disconnect {
                    container_id: container_id.clone(),
                    message: error.to_string(),
                })?;
        }
        docker
            .remove_network(ENDPOINT_NETWORK_NAME)
            .await
            .map_err(|error| MachineEndpointNetworkConvergenceError::Remove {
                message: error.to_string(),
            })?;
    }
    if recreated || created {
        match docker
            .create_network(endpoint_network_create_request(
                endpoint_network_subnet,
                endpoint_bridge_ifname,
                endpoint_mtu,
            ))
            .await
        {
            Ok(_) => {}
            Err(error) if is_network_already_exists(&error) => {}
            Err(error) => {
                return Err(MachineEndpointNetworkConvergenceError::Create {
                    message: error.to_string(),
                });
            }
        }
    }

    let repair_ids = if recreated || created {
        managed_ids.clone()
    } else {
        missing_before
    };
    let running = repair_ids
        .iter()
        .filter(|container_id| managed.get(*container_id) == Some(&ManagedContainerState::Running))
        .cloned()
        .collect::<Vec<_>>();
    for container_id in &running {
        docker
            .restart_container(container_id, None::<RestartContainerOptions>)
            .await
            .map_err(|error| MachineEndpointNetworkConvergenceError::Restart {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
    }

    let after_restarts = inspect_converged_network(
        docker,
        endpoint_network_subnet,
        endpoint_bridge_ifname,
        endpoint_mtu,
    )
    .await?;
    let attached_after_restarts = after_restarts
        .containers
        .as_ref()
        .map(|containers| containers.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let still_missing = repair_ids
        .difference(&attached_after_restarts)
        .cloned()
        .collect::<Vec<_>>();
    for container_id in &still_missing {
        docker
            .connect_network(
                ENDPOINT_NETWORK_NAME,
                NetworkConnectRequest {
                    container: container_id.clone(),
                    endpoint_config: None,
                },
            )
            .await
            .map_err(|error| MachineEndpointNetworkConvergenceError::Connect {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
    }

    let verified = inspect_converged_network(
        docker,
        endpoint_network_subnet,
        endpoint_bridge_ifname,
        endpoint_mtu,
    )
    .await?;
    let verified_ids = verified
        .containers
        .as_ref()
        .map(|containers| containers.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let missing = managed_ids
        .difference(&verified_ids)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(MachineEndpointNetworkConvergenceError::MissingAttachments {
            container_ids: missing,
        });
    }

    if recreated {
        Ok(MachineEndpointNetworkConvergenceOutcome::Recreated {
            reattached_containers: repair_ids.len(),
            restarted_containers: running.len(),
        })
    } else if created {
        Ok(MachineEndpointNetworkConvergenceOutcome::Created)
    } else {
        Ok(
            MachineEndpointNetworkConvergenceOutcome::RecoveredAttachments {
                reattached_containers: repair_ids.len(),
                restarted_containers: running.len(),
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedContainerState {
    Running,
    Stopped,
}

async fn managed_endpoint_containers(
    docker: &Docker,
) -> Result<BTreeMap<String, ManagedContainerState>, MachineEndpointNetworkConvergenceError> {
    let filters = HashMap::from([("label".to_owned(), vec![format!("{MANAGED_LABEL}=true")])]);
    let options = ListContainersOptionsBuilder::new()
        .all(true)
        .filters(&filters)
        .build();
    let summaries = docker
        .list_containers(Some(options))
        .await
        .map_err(
            |error| MachineEndpointNetworkConvergenceError::ListManagedContainers {
                message: error.to_string(),
            },
        )?;
    summaries
        .into_iter()
        .map(|summary| {
            let Some(id) = summary.id else {
                return Err(MachineEndpointNetworkConvergenceError::ManagedContainerMissingId);
            };
            let state = if summary.state == Some(ContainerSummaryStateEnum::RUNNING) {
                ManagedContainerState::Running
            } else {
                ManagedContainerState::Stopped
            };
            Ok((id, state))
        })
        .collect()
}

async fn inspect_converged_network(
    docker: &Docker,
    endpoint_network_subnet: &MachineEndpointSubnet,
    endpoint_bridge_ifname: &str,
    endpoint_mtu: u32,
) -> Result<NetworkInspect, MachineEndpointNetworkConvergenceError> {
    let network = docker
        .inspect_network(ENDPOINT_NETWORK_NAME, None::<InspectNetworkOptions>)
        .await
        .map_err(|error| MachineEndpointNetworkConvergenceError::Verify {
            message: error.to_string(),
        })?;
    validate_endpoint_network(
        &network,
        endpoint_network_subnet,
        endpoint_bridge_ifname,
        endpoint_mtu,
    )
    .map_err(|error| MachineEndpointNetworkConvergenceError::Verify {
        message: format!("{error:?}"),
    })?;
    Ok(network)
}

pub(super) async fn require_endpoint_network(
    docker: &Docker,
    endpoint_network_subnet: &MachineEndpointSubnet,
    endpoint_bridge_ifname: &str,
    endpoint_mtu: u32,
) -> Result<(), MachineContainerCreateError> {
    let network = required_endpoint_network_from_inspect(
        docker
            .inspect_network(ENDPOINT_NETWORK_NAME, None::<InspectNetworkOptions>)
            .await,
    )?;
    validate_endpoint_network(
        &network,
        endpoint_network_subnet,
        endpoint_bridge_ifname,
        endpoint_mtu,
    )
    .map_err(|error| match error {
        MachineEndpointNetworkError::EnsureEndpointNetwork { message } => {
            MachineContainerCreateError::EnsureEndpointNetwork { message }
        }
        MachineEndpointNetworkError::EndpointNetworkSubnetMismatch { expected, observed } => {
            MachineContainerCreateError::EndpointNetworkSubnetMismatch { expected, observed }
        }
    })
}

fn required_endpoint_network_from_inspect(
    result: Result<NetworkInspect, BollardError>,
) -> Result<NetworkInspect, MachineContainerCreateError> {
    result.map_err(|error| MachineContainerCreateError::Create {
        message: if is_docker_object_missing(&error) {
            format!("required Docker network {ENDPOINT_NETWORK_NAME} is missing")
        } else {
            format!("inspect required Docker network {ENDPOINT_NETWORK_NAME}: {error}")
        },
    })
}

pub(super) async fn read_endpoint_network_status(
    docker: &Docker,
    expected: MachineEndpointSubnet,
    endpoint_bridge_ifname: &str,
    endpoint_mtu: u32,
) -> EndpointBridgeStatus {
    let network = match docker
        .inspect_network(ENDPOINT_NETWORK_NAME, None::<InspectNetworkOptions>)
        .await
    {
        Ok(network) => network,
        Err(error) if is_docker_object_missing(&error) => return EndpointBridgeStatus::Missing,
        Err(error) => {
            return EndpointBridgeStatus::Unavailable {
                message: endpoint_status_failure(format!(
                    "inspect Docker network {ENDPOINT_NETWORK_NAME}: {error}"
                )),
            };
        }
    };
    match validate_endpoint_network(&network, &expected, endpoint_bridge_ifname, endpoint_mtu) {
        Ok(()) => EndpointBridgeStatus::Ready { subnet: expected },
        Err(MachineEndpointNetworkError::EndpointNetworkSubnetMismatch { expected, observed }) => {
            EndpointBridgeStatus::SubnetMismatch { expected, observed }
        }
        Err(error) => EndpointBridgeStatus::Unavailable {
            message: endpoint_status_failure(format!("{error:?}")),
        },
    }
}

fn endpoint_status_failure(message: String) -> ployz_core::operation::FailureMessage {
    ployz_core::operation::FailureMessage::try_new(message)
        .expect("Docker endpoint status failure is non-empty")
}

fn endpoint_network_mtu_matches(network: &NetworkInspect, endpoint_mtu: u32) -> bool {
    endpoint_network_mtu(network).as_deref() == Some(&endpoint_mtu.to_string())
}

fn endpoint_network_mtu(network: &NetworkInspect) -> Option<String> {
    network
        .options
        .as_ref()
        .and_then(|options| options.get(DRIVER_MTU_OPTION).cloned())
}

fn endpoint_network_subnet(network: &NetworkInspect) -> Option<&str> {
    network
        .ipam
        .as_ref()
        .and_then(|ipam| ipam.config.as_ref())
        .and_then(|configs| configs.first())
        .and_then(|config| config.subnet.as_deref())
}

fn endpoint_network_gateway(network: &NetworkInspect) -> Option<&str> {
    network
        .ipam
        .as_ref()
        .and_then(|ipam| ipam.config.as_ref())
        .and_then(|configs| configs.first())
        .and_then(|config| config.gateway.as_deref())
}

fn endpoint_network_bridge(network: &NetworkInspect) -> Option<&str> {
    network
        .options
        .as_ref()
        .and_then(|options| options.get("com.docker.network.bridge.name"))
        .map(String::as_str)
}

fn validate_endpoint_network(
    network: &NetworkInspect,
    expected_subnet: &MachineEndpointSubnet,
    expected_bridge: &str,
    expected_mtu: u32,
) -> Result<(), MachineEndpointNetworkError> {
    let expected_subnet_text = expected_subnet.as_string();
    if endpoint_network_subnet(network) != Some(expected_subnet_text.as_str()) {
        if let Some(Ok(observed)) =
            endpoint_network_subnet(network).map(MachineEndpointSubnet::try_new)
        {
            return Err(MachineEndpointNetworkError::EndpointNetworkSubnetMismatch {
                expected: expected_subnet.clone(),
                observed,
            });
        }
        return Err(MachineEndpointNetworkError::EnsureEndpointNetwork {
            message: format!(
                "Docker network {ENDPOINT_NETWORK_NAME} has subnet {}, expected {}",
                endpoint_network_subnet(network).unwrap_or("unset"),
                expected_subnet_text
            ),
        });
    }
    let expected_gateway = expected_subnet.bridge_gateway_ipv4().to_string();
    if network.driver.as_deref() != Some("bridge")
        || endpoint_network_bridge(network) != Some(expected_bridge)
        || !endpoint_network_mtu_matches(network, expected_mtu)
        || endpoint_network_gateway(network) != Some(expected_gateway.as_str())
    {
        return Err(MachineEndpointNetworkError::EnsureEndpointNetwork {
            message: format!(
                "Docker network {ENDPOINT_NETWORK_NAME} does not match bridge {expected_bridge}, MTU {expected_mtu}, and gateway {expected_gateway}"
            ),
        });
    }
    Ok(())
}

fn is_network_already_exists(error: &BollardError) -> bool {
    matches!(
        error,
        BollardError::DockerResponseServerError {
            status_code: 409,
            message
        } if message.contains("already exists")
    )
}

pub(super) fn is_docker_object_missing(error: &BollardError) -> bool {
    matches!(
        error,
        BollardError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::errors::Error as BollardError;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;
    use tokio::sync::Mutex;

    fn subnet(value: &str) -> MachineEndpointSubnet {
        MachineEndpointSubnet::try_new(value).expect("machine endpoint subnet")
    }

    async fn docker_with_responses(
        responses: Vec<(u16, &'static str)>,
    ) -> (Docker, Arc<Mutex<Vec<String>>>, tempfile::TempDir) {
        let socket_dir = tempfile::TempDir::new().expect("Docker API stub directory");
        let socket_path = socket_dir.path().join("docker.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind Docker API stub");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server_requests = Arc::clone(&requests);
        tokio::spawn(async move {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.expect("accept Docker API request");
                let mut request = Vec::new();
                let mut buffer = [0; 512];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).await.expect("read Docker request");
                    assert_ne!(read, 0, "Docker request ended before headers");
                    let (read_bytes, _) = buffer.split_at(read);
                    request.extend_from_slice(read_bytes);
                }
                server_requests
                    .lock()
                    .await
                    .push(String::from_utf8(request).expect("Docker request is UTF-8"));
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write Docker response");
            }
        });
        let docker = Docker::connect_with_socket(
            socket_path.to_str().expect("UTF-8 Docker socket path"),
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("connect Docker API stub");
        (docker, requests, socket_dir)
    }

    #[tokio::test]
    async fn exact_managed_endpoint_network_is_an_idempotent_noop() {
        let exact = r#"{"Driver":"bridge","Labels":{"plz.managed":"true"},"Options":{"com.docker.network.bridge.name":"br-ployz","com.docker.network.driver.mtu":"1420"},"IPAM":{"Config":[{"Subnet":"10.198.1.0/24","Gateway":"10.198.1.1"}]},"Containers":{}}"#;
        let (docker, requests, _socket_dir) =
            docker_with_responses(vec![(200, exact), (200, "[]")]).await;

        let outcome =
            converge_endpoint_network(&docker, &subnet("10.198.1.0/24"), "br-ployz", 1_420)
                .await
                .expect("exact endpoint network converges");

        assert_eq!(outcome, MachineEndpointNetworkConvergenceOutcome::Unchanged);
        let requests = requests.lock().await;
        let [inspect, containers] = requests.as_slice() else {
            panic!("expected only read-only Docker requests")
        };
        assert!(inspect.starts_with("GET /networks/ployz "), "{inspect}");
        assert!(
            containers.starts_with("GET /containers/json?"),
            "{containers}"
        );
    }

    #[tokio::test]
    async fn missing_endpoint_network_is_created_for_the_accepted_subnet() {
        let exact = r#"{"Driver":"bridge","Labels":{"plz.managed":"true"},"Options":{"com.docker.network.bridge.name":"br-ployz","com.docker.network.driver.mtu":"1420"},"IPAM":{"Config":[{"Subnet":"10.198.1.0/24","Gateway":"10.198.1.1"}]},"Containers":{}}"#;
        let (docker, requests, _socket_dir) = docker_with_responses(vec![
            (404, r#"{"message":"network ployz not found"}"#),
            (200, "[]"),
            (201, r#"{"Id":"new","Warning":""}"#),
            (200, exact),
            (200, exact),
        ])
        .await;

        let outcome =
            converge_endpoint_network(&docker, &subnet("10.198.1.0/24"), "br-ployz", 1_420)
                .await
                .expect("missing endpoint network converges");

        assert_eq!(outcome, MachineEndpointNetworkConvergenceOutcome::Created);
        let methods = requests
            .lock()
            .await
            .iter()
            .map(|request| request.lines().next().expect("request line").to_owned())
            .collect::<Vec<_>>();
        let [_, _, create, _, _] = methods.as_slice() else {
            panic!("expected Docker inspect, list, create, and verification requests")
        };
        assert!(create.starts_with("POST /networks/create "));
    }

    #[tokio::test]
    async fn concurrent_endpoint_network_create_is_verified_and_accepted() {
        let exact = r#"{"Driver":"bridge","Labels":{"plz.managed":"true"},"Options":{"com.docker.network.bridge.name":"br-ployz","com.docker.network.driver.mtu":"1420"},"IPAM":{"Config":[{"Subnet":"10.198.1.0/24","Gateway":"10.198.1.1"}]},"Containers":{}}"#;
        let (docker, requests, _socket_dir) = docker_with_responses(vec![
            (404, r#"{"message":"network ployz not found"}"#),
            (200, "[]"),
            (
                409,
                r#"{"message":"network with name ployz already exists"}"#,
            ),
            (200, exact),
            (200, exact),
        ])
        .await;

        let outcome =
            converge_endpoint_network(&docker, &subnet("10.198.1.0/24"), "br-ployz", 1_420)
                .await
                .expect("concurrent endpoint network creation converges");

        assert_eq!(outcome, MachineEndpointNetworkConvergenceOutcome::Created);
        assert_eq!(requests.lock().await.len(), 5);
    }

    #[tokio::test]
    async fn subnet_drift_recreates_network_and_restores_managed_containers() {
        let old = r#"{"Driver":"bridge","Labels":{"plz.managed":"true"},"Options":{"com.docker.network.bridge.name":"br-ployz","com.docker.network.driver.mtu":"1420"},"IPAM":{"Config":[{"Subnet":"10.198.1.0/24","Gateway":"10.198.1.1"}]},"Containers":{"running":{},"stopped":{}}}"#;
        let managed = r#"[{"Id":"running","Labels":{"plz.managed":"true"},"State":"running"},{"Id":"stopped","Labels":{"plz.managed":"true"},"State":"exited"}]"#;
        let after_restart = r#"{"Driver":"bridge","Labels":{"plz.managed":"true"},"Options":{"com.docker.network.bridge.name":"br-ployz","com.docker.network.driver.mtu":"1420"},"IPAM":{"Config":[{"Subnet":"10.198.2.0/24","Gateway":"10.198.2.1"}]},"Containers":{"running":{}}}"#;
        let complete = r#"{"Driver":"bridge","Labels":{"plz.managed":"true"},"Options":{"com.docker.network.bridge.name":"br-ployz","com.docker.network.driver.mtu":"1420"},"IPAM":{"Config":[{"Subnet":"10.198.2.0/24","Gateway":"10.198.2.1"}]},"Containers":{"running":{},"stopped":{}}}"#;
        let (docker, requests, _socket_dir) = docker_with_responses(vec![
            (200, old),
            (200, managed),
            (200, ""),
            (200, ""),
            (200, ""),
            (201, r#"{"Id":"new","Warning":""}"#),
            (200, ""),
            (200, after_restart),
            (200, ""),
            (200, complete),
        ])
        .await;

        let outcome =
            converge_endpoint_network(&docker, &subnet("10.198.2.0/24"), "br-ployz", 1_420)
                .await
                .expect("subnet drift converges");

        assert_eq!(
            outcome,
            MachineEndpointNetworkConvergenceOutcome::Recreated {
                reattached_containers: 2,
                restarted_containers: 1,
            }
        );
        let methods = requests
            .lock()
            .await
            .iter()
            .map(|request| request.lines().next().expect("request line").to_owned())
            .collect::<Vec<_>>();
        let [
            _,
            _,
            disconnect_one,
            disconnect_two,
            remove,
            create,
            restart,
            _,
            connect,
            _,
        ] = methods.as_slice()
        else {
            panic!("expected ten Docker convergence requests")
        };
        assert!(disconnect_one.starts_with("POST /networks/ployz/disconnect "));
        assert!(disconnect_two.starts_with("POST /networks/ployz/disconnect "));
        assert!(remove.starts_with("DELETE /networks/ployz "));
        assert!(create.starts_with("POST /networks/create "));
        assert!(restart.starts_with("POST /containers/running/restart"));
        assert!(connect.starts_with("POST /networks/ployz/connect "));
    }

    #[tokio::test]
    async fn unmanaged_endpoint_attachment_blocks_network_recreation() {
        let old = r#"{"Driver":"bridge","Labels":{"plz.managed":"true"},"Options":{"com.docker.network.bridge.name":"br-ployz","com.docker.network.driver.mtu":"1420"},"IPAM":{"Config":[{"Subnet":"10.198.1.0/24","Gateway":"10.198.1.1"}]},"Containers":{"foreign":{}}}"#;
        let (docker, requests, _socket_dir) =
            docker_with_responses(vec![(200, old), (200, "[]")]).await;

        let error = converge_endpoint_network(&docker, &subnet("10.198.2.0/24"), "br-ployz", 1_420)
            .await
            .expect_err("unmanaged endpoint blocks repair");

        assert_eq!(
            error,
            MachineEndpointNetworkConvergenceError::UnmanagedAttachments {
                container_ids: vec!["foreign".to_owned()],
            }
        );
        assert_eq!(requests.lock().await.len(), 2);
    }

    #[test]
    fn endpoint_network_create_request_sets_machine_subnet() {
        let request = endpoint_network_create_request(&subnet("10.42.7.0/24"), "br-ployz", 1420);

        assert_eq!(request.name, ENDPOINT_NETWORK_NAME);
        assert_eq!(request.driver, Some("bridge".to_owned()));
        assert_eq!(
            request
                .options
                .as_ref()
                .and_then(|options| { options.get("com.docker.network.bridge.name").cloned() }),
            Some("br-ployz".to_owned())
        );
        assert_eq!(
            request
                .options
                .as_ref()
                .and_then(|options| { options.get(DRIVER_MTU_OPTION).cloned() }),
            Some("1420".to_owned())
        );
        let config = request
            .ipam
            .and_then(|ipam| ipam.config)
            .and_then(|configs| configs.into_iter().next())
            .expect("machine endpoint IPAM config");
        assert_eq!(config.subnet, Some("10.42.7.0/24".to_owned()));
        assert_eq!(config.gateway, Some("10.42.7.1".to_owned()));
    }

    #[test]
    fn missing_required_endpoint_network_is_a_container_creation_failure() {
        let result =
            required_endpoint_network_from_inspect(Err(BollardError::DockerResponseServerError {
                status_code: 404,
                message: "network ployz not found".to_owned(),
            }));

        assert!(matches!(
            result,
            Err(MachineContainerCreateError::Create { message })
                if message.contains("required Docker network ployz is missing")
        ));
    }

    #[test]
    fn endpoint_network_create_conflict_is_idempotent() {
        assert!(is_network_already_exists(
            &BollardError::DockerResponseServerError {
                status_code: 409,
                message: "network with name ployz already exists".to_owned(),
            }
        ));
        assert!(!is_network_already_exists(
            &BollardError::DockerResponseServerError {
                status_code: 409,
                message: "different conflict".to_owned(),
            }
        ));
    }

    #[test]
    fn endpoint_network_mtu_matches_driver_option() {
        let network = NetworkInspect {
            options: Some(HashMap::from([(
                DRIVER_MTU_OPTION.to_owned(),
                "1420".to_owned(),
            )])),
            ..Default::default()
        };

        assert!(endpoint_network_mtu_matches(&network, 1420));
        assert!(!endpoint_network_mtu_matches(&network, 1412));
    }

    #[test]
    fn endpoint_network_validation_requires_exact_subnet_gateway_bridge_and_mtu() {
        let exact = NetworkInspect {
            driver: Some("bridge".to_owned()),
            options: Some(HashMap::from([
                (
                    "com.docker.network.bridge.name".to_owned(),
                    "br-ployz".to_owned(),
                ),
                (DRIVER_MTU_OPTION.to_owned(), "1420".to_owned()),
            ])),
            ipam: Some(Ipam {
                config: Some(vec![IpamConfig {
                    subnet: Some("10.198.1.0/24".to_owned()),
                    gateway: Some("10.198.1.1".to_owned()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let expected = subnet("10.198.1.0/24");
        assert!(validate_endpoint_network(&exact, &expected, "br-ployz", 1420).is_ok());
        assert!(matches!(
            validate_endpoint_network(&exact, &subnet("10.198.2.0/24"), "br-ployz", 1420),
            Err(MachineEndpointNetworkError::EndpointNetworkSubnetMismatch { .. })
        ));
        assert!(validate_endpoint_network(&exact, &expected, "other-bridge", 1420).is_err());
        assert!(validate_endpoint_network(&exact, &expected, "br-ployz", 1400).is_err());

        let mut wrong_gateway = exact;
        let Some(ipam) = wrong_gateway.ipam.as_mut() else {
            panic!("exact network has IPAM")
        };
        let Some(configs) = ipam.config.as_mut() else {
            panic!("exact network has IPAM config")
        };
        let [config] = configs.as_mut_slice() else {
            panic!("exact network has one IPAM config")
        };
        config.gateway = Some("10.198.1.2".to_owned());
        assert!(validate_endpoint_network(&wrong_gateway, &expected, "br-ployz", 1420).is_err());
    }
}
