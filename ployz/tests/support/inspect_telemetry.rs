//! Shared telemetry observations for Machine RPC integration fakes.

use ployz_core::{
    BridgeEndpointCapacity, ByteCapacity, InspectTelemetry, MachineTelemetry, TelemetryObservation,
};

pub(super) fn observation(scope: InspectTelemetry) -> Option<TelemetryObservation> {
    let bridge = BridgeEndpointCapacity::new(1_024, 0);
    match scope {
        InspectTelemetry::None => None,
        InspectTelemetry::BridgeCapacity => Some(TelemetryObservation::BridgeCapacity { bridge }),
        InspectTelemetry::Full => Some(TelemetryObservation::Full {
            host: MachineTelemetry::new(
                0,
                0,
                1,
                0,
                ByteCapacity::default(),
                ByteCapacity::default(),
            ),
            bridge,
        }),
    }
}
