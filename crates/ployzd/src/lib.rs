#![forbid(unsafe_code)]

//! Ployz daemon roles and transport-free runtime mechanics.
//!
//! Role selection is explicit and unavailable roles return a typed startup
//! error. Cluster storage and transport are outside the mechanics retained
//! here.

mod adapters {
    pub(crate) mod atomic_file;
}
mod certificate {
    pub(crate) mod material;
}
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
}
pub mod dispatch;
pub mod logging;
pub mod role_cli;
