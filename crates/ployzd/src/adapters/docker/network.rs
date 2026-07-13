use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::models::{Ipam, IpamConfig, NetworkCreateRequest, NetworkInspect};
use bollard::query_parameters::InspectNetworkOptions;
use ployz_core::dataplane::{EndpointBridgeStatus, MachineEndpointSubnet};
use std::collections::HashMap;

use crate::adapters::docker::labels::MANAGED_LABEL;
use crate::roles::machine::runner::MachineContainerRunnerError;

pub(super) const ENDPOINT_NETWORK_NAME: &str = "ployz";
pub(super) const DRIVER_MTU_OPTION: &str = "com.docker.network.driver.mtu";

pub(super) fn endpoint_network_create_request(
    endpoint_network_subnet: &str,
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
                subnet: Some(endpoint_network_subnet.to_owned()),
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
    endpoint_network_subnet: &str,
    endpoint_bridge_ifname: &str,
    endpoint_mtu: u32,
) -> Result<(), MachineContainerRunnerError> {
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
            return Err(MachineContainerRunnerError::EnsureEndpointNetwork {
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
            return Err(MachineContainerRunnerError::EnsureEndpointNetwork {
                message: format!("create Docker network {ENDPOINT_NETWORK_NAME}: {error}"),
            });
        }
    }

    let network = docker
        .inspect_network(ENDPOINT_NETWORK_NAME, None::<InspectNetworkOptions>)
        .await
        .map_err(|error| MachineContainerRunnerError::EnsureEndpointNetwork {
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

pub(super) async fn require_endpoint_network(
    docker: &Docker,
    endpoint_network_subnet: &str,
    endpoint_bridge_ifname: &str,
    endpoint_mtu: u32,
) -> Result<(), MachineContainerRunnerError> {
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
}

fn required_endpoint_network_from_inspect(
    result: Result<NetworkInspect, BollardError>,
) -> Result<NetworkInspect, MachineContainerRunnerError> {
    result.map_err(|error| MachineContainerRunnerError::Create {
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
    endpoint_network_subnet: &str,
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
    match validate_endpoint_network(
        &network,
        endpoint_network_subnet,
        endpoint_bridge_ifname,
        endpoint_mtu,
    ) {
        Ok(()) => EndpointBridgeStatus::Ready { subnet: expected },
        Err(MachineContainerRunnerError::EndpointNetworkSubnetMismatch { expected, observed }) => {
            EndpointBridgeStatus::SubnetMismatch { expected, observed }
        }
        Err(error) => EndpointBridgeStatus::Unavailable {
            message: endpoint_status_failure(format!("{error:?}")),
        },
    }
}

fn endpoint_status_failure(message: String) -> ployz_core::ops::FailureMessage {
    ployz_core::ops::FailureMessage::try_new(message)
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

fn endpoint_network_bridge(network: &NetworkInspect) -> Option<&str> {
    network
        .options
        .as_ref()
        .and_then(|options| options.get("com.docker.network.bridge.name"))
        .map(String::as_str)
}

fn validate_endpoint_network(
    network: &NetworkInspect,
    expected_subnet: &str,
    expected_bridge: &str,
    expected_mtu: u32,
) -> Result<(), MachineContainerRunnerError> {
    if endpoint_network_subnet(network) != Some(expected_subnet) {
        if let (Ok(expected), Some(Ok(observed))) = (
            MachineEndpointSubnet::try_new(expected_subnet),
            endpoint_network_subnet(network).map(MachineEndpointSubnet::try_new),
        ) {
            return Err(MachineContainerRunnerError::EndpointNetworkSubnetMismatch {
                expected,
                observed,
            });
        }
        return Err(MachineContainerRunnerError::EnsureEndpointNetwork {
            message: format!(
                "Docker network {ENDPOINT_NETWORK_NAME} has subnet {}, expected {expected_subnet}",
                endpoint_network_subnet(network).unwrap_or("unset")
            ),
        });
    }
    if network.driver.as_deref() != Some("bridge")
        || endpoint_network_bridge(network) != Some(expected_bridge)
        || !endpoint_network_mtu_matches(network, expected_mtu)
    {
        return Err(MachineContainerRunnerError::EnsureEndpointNetwork {
            message: format!(
                "Docker network {ENDPOINT_NETWORK_NAME} does not match bridge {expected_bridge} and MTU {expected_mtu}"
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

    #[test]
    fn endpoint_network_create_request_sets_machine_subnet() {
        let request = endpoint_network_create_request("10.42.7.0/24", "br-ployz", 1420);

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
        assert_eq!(
            request
                .ipam
                .and_then(|ipam| ipam.config)
                .and_then(|configs| configs.into_iter().next().and_then(|config| config.subnet)),
            Some("10.42.7.0/24".to_owned())
        );
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
            Err(MachineContainerRunnerError::Create { message })
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
    fn endpoint_network_validation_requires_exact_subnet_bridge_and_mtu() {
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
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert!(validate_endpoint_network(&exact, "10.198.1.0/24", "br-ployz", 1420).is_ok());
        assert!(matches!(
            validate_endpoint_network(&exact, "10.198.2.0/24", "br-ployz", 1420),
            Err(MachineContainerRunnerError::EndpointNetworkSubnetMismatch { .. })
        ));
        assert!(validate_endpoint_network(&exact, "10.198.1.0/24", "other-bridge", 1420).is_err());
        assert!(validate_endpoint_network(&exact, "10.198.1.0/24", "br-ployz", 1400).is_err());
    }
}
