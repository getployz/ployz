use bollard::models::{Ipam, IpamConfig, NetworkCreateRequest};
use std::collections::HashMap;

use crate::adapters::docker::labels::MANAGED_LABEL;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
