#[path = "../daemon/handlers/image/archive.rs"]
mod archive;
#[path = "../daemon/handlers/image/inspect.rs"]
mod inspect;
#[path = "../daemon/handlers/image/operations.rs"]
pub(crate) mod operations;
#[path = "../daemon/handlers/image/push.rs"]
mod push;
#[path = "../daemon/handlers/image/registry.rs"]
pub(crate) mod registry;
#[path = "../daemon/handlers/image/status.rs"]
mod status;
