pub(crate) use ployz_image::registry;

#[path = "../daemon/handlers/image/inspect.rs"]
mod inspect;
#[path = "../daemon/handlers/image/operations.rs"]
pub(crate) mod operations;
#[path = "../daemon/handlers/image/push.rs"]
mod push;
#[path = "../daemon/handlers/image/status.rs"]
mod status;
