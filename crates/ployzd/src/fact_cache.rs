//! Compatibility exports for the daemon-local role testimony cache.

pub use crate::role_testimony::{
    RoleTestimonyCache as FactCache, RoleTestimonyCacheError as FactCacheError,
    RunningRoleTestimonyCache as RunningFactCache, start_role_testimony_cache as start_fact_cache,
};
