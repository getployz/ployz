#![forbid(unsafe_code)]

//! Ployz daemon roles and transport-free runtime mechanics.
//!
//! Role selection is explicit and unavailable roles return a typed startup
//! error. Cluster storage and transport are outside the mechanics retained
//! here.

mod adapters {
    pub(crate) mod atomic_file;
}
pub mod certificate {
    mod issuer;
    pub(crate) mod material;
    pub use issuer::{
        AcmeAccountStore, AcmeIssuerError, AcmeTimeoutPhase, DEFAULT_ACME_CLEANUP_TIMEOUT,
        DEFAULT_ACME_DIRECTORY_URL, DEFAULT_ACME_ISSUE_TIMEOUT, Http01ChallengePublisher,
        InstantAcmeIssuer, IssuedCertificate,
    };
    pub use material::{CertificateMaterialError, prepare_custom_certificate};
}
pub mod corrosion;
pub mod roles {
    pub mod dns {
        pub mod internal;
        pub use internal::InternalResolverHealth;
    }
    pub mod gateway {
        #[path = "source/certificate_store.rs"]
        pub mod certificate_store;
        pub mod pingora;
        pub mod projection;
        pub mod route_table;
    }
    pub mod api;
    pub mod keeper;
    pub(crate) mod upgrade;
}
pub mod dispatch;
pub mod logging;
mod network_mtu;
pub use network_mtu::WireGuardMtuPolicy;
pub mod role_cli;
