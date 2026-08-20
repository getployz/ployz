//! Fresh operator telemetry from one Machine.

use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

/// Consistent usable, attached, and free endpoint counts for the Ployz bridge.
///
/// Attached endpoints may exceed the currently usable IPAM range when live
/// Docker state is overcommitted. In that state free capacity is zero.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct BridgeEndpointCapacity {
    #[serde(rename = "bridge_usable_endpoints")]
    usable_endpoints: u64,
    #[serde(rename = "bridge_attached_endpoints")]
    attached_endpoints: u64,
    #[serde(rename = "bridge_free_endpoints")]
    free_endpoints: u64,
}

impl BridgeEndpointCapacity {
    /// Build counts from live usable and attached endpoints.
    #[must_use]
    pub fn new(usable_endpoints: u64, attached_endpoints: u64) -> Self {
        Self {
            usable_endpoints,
            attached_endpoints,
            free_endpoints: usable_endpoints.saturating_sub(attached_endpoints),
        }
    }

    /// IPAM-derived bridge endpoints available to Containers.
    #[must_use]
    pub fn usable_endpoints(&self) -> u64 {
        self.usable_endpoints
    }

    /// Endpoints currently attached to the bridge.
    #[must_use]
    pub fn attached_endpoints(&self) -> u64 {
        self.attached_endpoints
    }

    /// Usable bridge endpoints not currently attached.
    #[must_use]
    pub fn free_endpoints(&self) -> u64 {
        self.free_endpoints
    }
}

/// Fresh telemetry requested from a Machine inspection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectTelemetry {
    /// Do not probe telemetry.
    #[default]
    None,
    /// Probe only Ployz bridge endpoint capacity for admission.
    BridgeCapacity,
    /// Probe host telemetry and Ployz bridge endpoint capacity.
    Full,
}

impl<'de> Deserialize<'de> for BridgeEndpointCapacity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            bridge_usable_endpoints: u64,
            bridge_attached_endpoints: u64,
            bridge_free_endpoints: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        let capacity = Self::new(wire.bridge_usable_endpoints, wire.bridge_attached_endpoints);
        if capacity.free_endpoints != wire.bridge_free_endpoints {
            return Err(de::Error::custom("bridge endpoint counts are inconsistent"));
        }
        Ok(capacity)
    }
}

/// Fresh operator telemetry from one Machine. It is not placement scoring data.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct MachineTelemetry {
    /// Unix timestamp when this observation began.
    pub observed_at_unix_seconds: u64,
    /// Ployz-managed Docker Container count.
    pub managed_containers: u64,
    /// Host logical CPU count.
    pub cpu_count: u64,
    /// One-minute host load average multiplied by 1,000.
    pub load_average_milli: u64,
    /// Host memory total in bytes.
    memory_total_bytes: u64,
    /// Host memory available in bytes.
    memory_available_bytes: u64,
    /// Docker-root filesystem size in bytes.
    docker_root_total_bytes: u64,
    /// Docker-root filesystem free bytes.
    docker_root_free_bytes: u64,
}

/// A total byte count and a usable subset that cannot exceed it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ByteCapacity {
    total: u64,
    available: u64,
}

/// An available byte count exceeded its resource total.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("available bytes exceed total bytes")]
pub struct ByteCapacityError;

/// Fresh telemetry returned by a requested Machine inspection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
pub enum TelemetryObservation {
    /// Only bridge endpoint capacity was requested.
    BridgeCapacity {
        /// Fresh Ployz bridge endpoint capacity.
        bridge: BridgeEndpointCapacity,
    },
    /// Host telemetry and bridge endpoint capacity were requested together.
    Full {
        /// Fresh host telemetry.
        host: MachineTelemetry,
        /// Fresh Ployz bridge endpoint capacity.
        bridge: BridgeEndpointCapacity,
    },
}

impl TelemetryObservation {
    /// Take the fresh Ployz bridge endpoint capacity from either observation.
    #[must_use]
    pub fn into_bridge(self) -> BridgeEndpointCapacity {
        match self {
            Self::BridgeCapacity { bridge } | Self::Full { bridge, .. } => bridge,
        }
    }
}

impl ByteCapacity {
    /// Parse a total byte count and its available subset.
    ///
    /// # Errors
    ///
    /// Returns [`ByteCapacityError`] when `available` exceeds `total`.
    pub fn parse(total: u64, available: u64) -> Result<Self, ByteCapacityError> {
        (available <= total)
            .then_some(Self { total, available })
            .ok_or(ByteCapacityError)
    }

    /// Total bytes in the resource.
    #[must_use]
    pub fn total(self) -> u64 {
        self.total
    }

    /// Available bytes in the resource.
    #[must_use]
    pub fn available(self) -> u64 {
        self.available
    }
}

impl MachineTelemetry {
    /// Build telemetry from validated memory and Docker-root capacities.
    #[must_use]
    pub fn new(
        observed_at_unix_seconds: u64,
        managed_containers: u64,
        cpu_count: u64,
        load_average_milli: u64,
        memory: ByteCapacity,
        docker_root: ByteCapacity,
    ) -> Self {
        Self {
            observed_at_unix_seconds,
            managed_containers,
            cpu_count,
            load_average_milli,
            memory_total_bytes: memory.total(),
            memory_available_bytes: memory.available(),
            docker_root_total_bytes: docker_root.total(),
            docker_root_free_bytes: docker_root.available(),
        }
    }

    /// Host memory total in bytes.
    #[must_use]
    pub fn memory_total_bytes(&self) -> u64 {
        self.memory_total_bytes
    }

    /// Host memory available in bytes.
    #[must_use]
    pub fn memory_available_bytes(&self) -> u64 {
        self.memory_available_bytes
    }

    /// Docker-root filesystem size in bytes.
    #[must_use]
    pub fn docker_root_total_bytes(&self) -> u64 {
        self.docker_root_total_bytes
    }

    /// Docker-root filesystem free bytes.
    #[must_use]
    pub fn docker_root_free_bytes(&self) -> u64 {
        self.docker_root_free_bytes
    }
}

impl<'de> Deserialize<'de> for MachineTelemetry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            observed_at_unix_seconds: u64,
            managed_containers: u64,
            cpu_count: u64,
            load_average_milli: u64,
            memory_total_bytes: u64,
            memory_available_bytes: u64,
            docker_root_total_bytes: u64,
            docker_root_free_bytes: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        let memory = ByteCapacity::parse(wire.memory_total_bytes, wire.memory_available_bytes)
            .map_err(|_| de::Error::custom("Machine memory counts are inconsistent"))?;
        let docker_root =
            ByteCapacity::parse(wire.docker_root_total_bytes, wire.docker_root_free_bytes)
                .map_err(|_| de::Error::custom("Docker-root byte counts are inconsistent"))?;
        Ok(Self::new(
            wire.observed_at_unix_seconds,
            wire.managed_containers,
            wire.cpu_count,
            wire.load_average_milli,
            memory,
            docker_root,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inconsistent_endpoint_counts_cannot_deserialize() {
        assert!(
            serde_json::from_str::<BridgeEndpointCapacity>(
                r#"{"bridge_usable_endpoints":5,"bridge_attached_endpoints":2,"bridge_free_endpoints":4}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn inconsistent_machine_byte_counts_cannot_deserialize() {
        assert!(
            serde_json::from_str::<MachineTelemetry>(
                r#"{"observed_at_unix_seconds":0,"managed_containers":0,"cpu_count":1,"load_average_milli":0,"memory_total_bytes":1,"memory_available_bytes":2,"docker_root_total_bytes":1,"docker_root_free_bytes":0}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn byte_capacity_rejects_available_above_total() {
        assert_eq!(ByteCapacity::parse(1, 2), Err(ByteCapacityError));
    }
}
