#![forbid(unsafe_code)]

//! Ployz daemon roles and transport-free runtime mechanics.
//!
//! Role selection is explicit and startup failures remain typed. Cluster
//! storage and transport adapters stay behind their owning daemon roles.

mod adapters {
    pub(crate) mod atomic_file;
}
pub mod certificate {
    mod issuer;
    pub use issuer::{
        AcmeAccountStore, AcmeIssuerError, AcmeTimeoutPhase, DEFAULT_ACME_CLEANUP_TIMEOUT,
        DEFAULT_ACME_DIRECTORY_URL, DEFAULT_ACME_ISSUE_TIMEOUT, Http01ChallengePublisher,
        InstantAcmeIssuer, IssuedCertificate,
    };
}
pub mod corrosion;
pub(crate) mod lease;
pub mod roles {
    /// Advertised internal-DNS record TTL. The DNS role serves it on every
    /// answer, and the deploy drain wait must cover at least this long.
    pub const DNS_TTL_SECONDS: u32 = 5;

    pub mod dns {
        mod config;
        pub mod internal;
        mod runtime;
        mod source;
        pub use internal::InternalResolverHealth;
        pub use runtime::{DnsRoleRuntimeError, run_from_environment};
    }
    pub mod gateway {
        mod config;
        mod observation;
        pub(crate) mod pingora;
        pub(crate) mod projection;
        mod runtime;
        mod source;
        pub use runtime::{GatewayRoleRuntimeError, run_from_environment};
    }
    pub mod api;
    pub(crate) mod handshake_control;
    pub mod keeper;
    pub(crate) mod system_observation;
    pub(crate) mod upgrade;
}
pub mod dispatch;
pub mod logging;
mod network_mtu;
pub use network_mtu::WireGuardMtuPolicy;
pub mod role_cli;
