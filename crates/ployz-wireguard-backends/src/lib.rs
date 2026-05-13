#[cfg(any(feature = "docker", feature = "userspace-wg"))]
pub(crate) use ployz_error as error;
#[cfg(any(feature = "docker", feature = "userspace-wg"))]
pub(crate) use ployz_model as model;

pub mod mesh;
pub use mesh::{driver, wireguard};
