//! Core-local ingress intent and private projection evidence stores.

mod active_certificate;
mod configuration;
mod ployz_dns_target;
mod projection;

pub use active_certificate::ActiveCertificateMetadataStore;
pub(crate) use active_certificate::upsert_active_certificate_metadata;
pub use configuration::{
    IngressConfiguration, IngressConfigurationWrite, IngressIntentStore, IngressIntentStoreError,
};
pub use ployz_dns_target::{
    ManagedDnsCheckpoint, ManagedDnsEndpointSet, PloyzDnsTargetAllocation, PloyzDnsTargetStore,
    PloyzDnsTargetWrite,
};
pub use projection::{IngressProjectionStore, IngressProjectionWrite};
