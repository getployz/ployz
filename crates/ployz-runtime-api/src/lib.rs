mod container;
mod identity;
mod image;
pub mod ipam;
pub mod mesh;

use async_trait::async_trait;

pub use container::{
    Observation, ObservedContainer, PortBinding, PortMap, RestartPolicy, RuntimeContainerSpec,
};
pub use identity::{Identity, IdentityError};
pub use image::{
    ImageArchiveReader, ImageDiskPreflight, ImageReceivePreflightRequest, RuntimeImage,
    RuntimeImageBackend, RuntimeImageError, RuntimeImageImportResult,
};

#[async_trait]
pub trait RuntimeHandle: Send + Sync {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String>;

    async fn detach(self: Box<Self>) -> std::result::Result<(), String> {
        Ok(())
    }
}

pub struct NoopRuntimeHandle;

#[async_trait]
impl RuntimeHandle for NoopRuntimeHandle {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String> {
        Ok(())
    }
}
