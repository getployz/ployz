//! The preview Namespaces' sets, read from Docker: each preview Container's static address on
//! the ployz bridge, which it keeps while stopped, so a Docker restart of one changes nothing.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    net::Ipv4Addr,
};

use bollard::query_parameters::ListContainersOptionsBuilder;
use ployz_core::{AddressPart, Namespace, PreviewNamespace, labels};

use super::{Error, LocalDocker};
use crate::network::{DOCKER_NETWORK_NAME, bridge::address::pinned};

/// One read of Docker and its write at a time, so an older read never lands after a newer one.
static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl LocalDocker {
    /// Write each preview Namespace's set from the Containers Docker lists on the bridge now.
    ///
    /// # Errors
    /// Returns why Docker could not list the Containers, or the sets could not be written.
    pub(crate) async fn sync_preview_fence(&self) -> Result<(), Error> {
        let _turn = TURN.lock().await;
        let listed = self
            .client
            .list_containers(Some(
                ListContainersOptionsBuilder::default()
                    .all(true)
                    .filters(&HashMap::from([("network", vec![DOCKER_NETWORK_NAME])]))
                    .build(),
            ))
            .await?;
        let mut sets: BTreeMap<Namespace, BTreeSet<Ipv4Addr>> = BTreeMap::new();
        for container in listed {
            let namespace = container
                .labels
                .as_ref()
                .and_then(|labels| labels.get(labels::NAMESPACE))
                .and_then(|namespace| Namespace::parse(namespace).ok())
                .filter(|namespace| PreviewNamespace::of(namespace).is_some());
            let endpoint = container
                .network_settings
                .as_ref()
                .and_then(|settings| settings.networks.as_ref()?.get(DOCKER_NETWORK_NAME));
            let address = endpoint.and_then(|endpoint| {
                pinned(endpoint).or_else(|| endpoint.ip_address.as_deref()?.parse().ok())
            });
            if let (Some(namespace), Some(address)) = (namespace, address)
                && AddressPart::of_last_octet(address.octets()[3]) == AddressPart::Preview
            {
                sets.entry(namespace).or_default().insert(address);
            }
        }
        tokio::task::spawn_blocking(move || crate::network::write_preview_sets(sets))
            .await
            .map_err(|error| Error::Network(std::io::Error::other(error).into()))??;
        Ok(())
    }
}
