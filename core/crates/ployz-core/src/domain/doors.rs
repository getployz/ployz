//! The Cluster's Doors: the one reader every Engine caller asks which Doors the
//! Cluster's ingress Servers should hold.

use crate::{
    Ciphertext, IngressDoors, IngressPorts, IngressWiring, QualifiedService, ServiceId,
    ServiceObservation, SetIngressDoorsRequest, tunnel_sealed_token,
    validate_ingress_service_spec,
};

/// The Doors the Cluster's Ingress Proxies should hold, with the sealed tunnel token
/// the Tunnel Connector carries. Every Server a command sets gets these, so a joiner
/// opens the same Doors as the rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterDoors {
    pub doors: IngressDoors,
    /// From the newest Tunnel Connector with a readable token. `None` when none runs,
    /// so a declared Tunnel Door without one is refused where it would open.
    pub sealed_token: Option<Ciphertext>,
}

impl ClusterDoors {
    #[must_use]
    pub fn closed() -> Self {
        Self {
            doors: IngressDoors::closed(),
            sealed_token: None,
        }
    }

    #[must_use]
    pub fn public() -> Self {
        Self {
            doors: IngressDoors::public(),
            sealed_token: None,
        }
    }

    /// The SetIngressDoors that opens these Doors for the Ingress Proxy Service `proxy`.
    #[must_use]
    pub fn request(&self, proxy: ServiceId) -> SetIngressDoorsRequest {
        SetIngressDoorsRequest {
            doors: self.doors.clone(),
            proxy_service: proxy,
            sealed_token: self.sealed_token.clone(),
        }
    }
}

/// The Doors the Cluster should hold. A `declared` set every Door of which this release
/// knows wins. Otherwise the Doors are read from the newest holder any Server runs and
/// the newest Tunnel Connector with a token, as before the declaration existed: a
/// Cluster with no holder and no connector holds the Public Door, and one whose newest
/// holder publishes no port and runs no connector holds none.
///
/// The token always comes from the newest connector, since the declaration carries no
/// secret.
#[must_use]
pub fn cluster_doors(
    declared: Option<IngressDoors>,
    services: &[ServiceObservation],
) -> ClusterDoors {
    let newest = |identity: QualifiedService| {
        services
            .iter()
            .filter(|service| service.identity == identity)
            .flat_map(|service| service.containers.iter().map(|copy| copy.as_observation()))
            .max_by_key(|copy| (copy.created_at_unix_nanos, copy.container_id))
    };
    let sealed_token = newest(QualifiedService::system_tunnel())
        .and_then(|tunnel| tunnel_sealed_token(&tunnel.resolved_spec.container.labels));
    if let Some(doors) = declared.filter(IngressDoors::is_known) {
        return ClusterDoors {
            doors,
            sealed_token,
        };
    }
    let ports = newest(QualifiedService::system_ingress_holder())
        .and_then(|holder| validate_ingress_service_spec(&holder.resolved_spec).ok())
        .map_or(IngressPorts::Public, IngressWiring::ports);
    let doors = match (ports, sealed_token.is_some()) {
        (IngressPorts::Public, false) => IngressDoors::public(),
        (IngressPorts::Closed, false) => IngressDoors::closed(),
        (IngressPorts::Public, true) => IngressDoors::both(),
        (IngressPorts::Closed, true) => IngressDoors::tunnel(),
    };
    ClusterDoors {
        doors,
        sealed_token,
    }
}

/// The declared Doors in `stored`, the JSON text of the replicated `doors` key. `None`
/// when it is not a set of Doors, or names a Door this release doesn't know: a reader
/// then treats the Cluster as undeclared, and never as Public.
#[must_use]
pub fn declared_doors(stored: &str) -> Option<IngressDoors> {
    serde_json::from_str::<IngressDoors>(stored)
        .ok()
        .filter(IngressDoors::is_known)
}

/// Decode an optional declared set as [`declared_doors`] does: an unreadable value
/// reads as `None`, never as an error.
pub(crate) fn optional_declared<'de, D>(decoder: D) -> Result<Option<IngressDoors>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<serde_json::Value> as serde::Deserialize>::deserialize(decoder)?;
    Ok(value
        .and_then(|value| serde_json::from_value::<IngressDoors>(value).ok())
        .filter(IngressDoors::is_known))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_set_of_known_doors_is_a_declaration() {
        assert_eq!(declared_doors("[]"), Some(IngressDoors::closed()));
        assert_eq!(declared_doors(r#"["public"]"#), Some(IngressDoors::public()));
        assert_eq!(
            declared_doors(r#"["tunnel","public"]"#),
            Some(IngressDoors::both())
        );
        for unreadable in [r#"["tunnel","warp"]"#, r#""x""#, "{}", "null", "not json"] {
            assert_eq!(declared_doors(unreadable), None, "{unreadable}");
        }
    }
}
