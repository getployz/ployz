//! The Machine RPC catalog read as data: one [`RouteRow`] per route, in catalog order.

use std::collections::BTreeSet;

use super::{CapabilityAdvertisement, ManagementScope};
use crate::CapabilityName;

/// How a route moves messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteShape {
    Unary,
    ServerStreaming,
    BidiStreaming,
}

/// What the catalog names about one route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteRow {
    /// The fully qualified gRPC path the route is dispatched on.
    pub path: &'static str,
    /// The JSON `command` of the route's request. A bidi stream carries none.
    pub command: &'static str,
    pub capability: &'static str,
    pub advertisement: CapabilityAdvertisement,
    pub scope: ManagementScope,
    pub shape: RouteShape,
}

macro_rules! define_capabilities {
    (
        package $package:literal
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_request:ty, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident $(, $($unary_more:tt)*)?),)+ }
        server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_request:ty, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident $(, $($stream_more:tt)*)?),)+ }
        bidi_streaming { $($bidi_variant:ident: ($bidi_method:ident, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident $(, $($bidi_more:tt)*)?),)+ }
        capabilities { $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+ }
    ) => {
        $(pub const $unary_capability: &str = $unary_capability_name;)+
        $(pub const $stream_capability: &str = $stream_capability_name;)+
        $(pub const $bidi_capability: &str = $bidi_capability_name;)+
        $($(#[$extra_meta])* pub const $extra_capability: &str = $extra_capability_name;)+

        /// Every route, unary then server-streaming then bidi, in catalog order.
        pub const ROUTES: &[RouteRow] = &[
            $(RouteRow {
                path: concat!("/", $package, ".MachineRpc/", stringify!($unary_variant)),
                command: stringify!($unary_method),
                capability: $unary_capability,
                advertisement: CapabilityAdvertisement::$unary_advertisement,
                scope: ManagementScope::$unary_scope,
                shape: RouteShape::Unary,
            },)+
            $(RouteRow {
                path: concat!("/", $package, ".MachineRpc/", stringify!($stream_variant)),
                command: stringify!($stream_method),
                capability: $stream_capability,
                advertisement: CapabilityAdvertisement::$stream_advertisement,
                scope: ManagementScope::$stream_scope,
                shape: RouteShape::ServerStreaming,
            },)+
            $(RouteRow {
                path: concat!("/", $package, ".MachineRpc/", stringify!($bidi_variant)),
                command: stringify!($bidi_method),
                capability: $bidi_capability,
                advertisement: CapabilityAdvertisement::$bidi_advertisement,
                scope: ManagementScope::$bidi_scope,
                shape: RouteShape::BidiStreaming,
            },)+
        ];

        /// The capabilities that name no route, in catalog order.
        const BEHAVIOUR_CAPABILITIES: &[(&str, CapabilityAdvertisement)] = &[
            $(($extra_capability, CapabilityAdvertisement::$extra_advertisement),)+
        ];
    };
}

crate::rpc_catalog!(define_capabilities);

fn route_at(path: &str) -> Option<&'static RouteRow> {
    ROUTES.iter().find(|route| route.path == path)
}

/// Whether this exact RPC path accepts one buffered request for fan-out.
/// Client-streaming methods (Exec and Build) require one selected Machine.
#[must_use]
pub fn supports_fanout(path: &str) -> bool {
    route_at(path).is_some_and(|route| route.shape != RouteShape::BidiStreaming)
}

/// The capability that names the route at `path`, or `None` for an uncatalogued path.
#[must_use]
pub fn capability_of(path: &str) -> Option<&'static str> {
    route_at(path).map(|route| route.capability)
}

/// The least scope that may call the route at `path`, or `None` for an uncatalogued path.
#[must_use]
pub fn scope_of(path: &str) -> Option<ManagementScope> {
    route_at(path).map(|route| route.scope)
}

/// Whether `command` names a request a Machine decodes. A bidi stream is opened on its
/// route and carries no request envelope.
pub(super) fn is_request_command(command: &str) -> bool {
    ROUTES
        .iter()
        .any(|route| route.shape != RouteShape::BidiStreaming && route.command == command)
}

fn catalogued_capabilities() -> impl Iterator<Item = (&'static str, CapabilityAdvertisement)> {
    ROUTES
        .iter()
        .map(|route| (route.capability, route.advertisement))
        .chain(BEHAVIOUR_CAPABILITIES.iter().copied())
}

impl CapabilityAdvertisement {
    /// Capability names this class advertises, in catalog order.
    pub fn capabilities(self) -> impl Iterator<Item = CapabilityName> {
        catalogued_capabilities()
            .filter(move |(_, class)| *class == self)
            .map(|(name, _)| CapabilityName::parse(name).expect("static capability name is valid"))
    }
}

/// Every capability name this binary can serve, whether or not a unit is on now.
#[must_use]
pub fn served_capabilities() -> BTreeSet<String> {
    catalogued_capabilities()
        .map(|(name, _)| name.to_owned())
        .collect()
}
