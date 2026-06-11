//! NATS subject permission profiles.
//!
//! The security role model lives in `ployz-core` (`ployz_core::permissions`);
//! this module re-exports it for existing ployz-nats callers.

pub use ployz_core::permissions::{
    NatsPermissionProfile, ResponsePermission, SubjectPermissions,
    active_route_state_kv_write_scope, active_service_state_kv_write_scope, inbox_prefix,
    inbox_subscribe_scope, kv_read_js_api_subjects, lock_kv_write_scope,
    node_observation_kv_write_subjects, operation_status_kv_write_scope,
};
