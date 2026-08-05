//! Mesh-aware CLI transport.

pub mod context;
pub mod http;
mod netstack;
mod wireguard;

pub use wireguard::{
    BuiltinWireguardDial, BuiltinWireguardPeer, MeshConnectError, MeshConnector, MeshDialTimeouts,
    MeshStream, UDP_BLOCKED_DOCS_ANCHOR, render_ssh_fallback_command,
};
