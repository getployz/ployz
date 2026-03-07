use crate::spec::ManagedVolumeSpec;
use crate::error::Result;

pub struct VolumeContext {
    pub namespace: String,
    pub service_name: String,
    pub machine_id: String,
}

pub struct ProvisionResult {
    pub host_path: String,
}

pub trait VolumeDriver: Send + Sync {
    fn provision(&self, spec: &ManagedVolumeSpec, ctx: &VolumeContext) -> Result<ProvisionResult>;
    fn on_update(&self, spec: &ManagedVolumeSpec, ctx: &VolumeContext) -> Result<()>;
    fn on_stop(&self, spec: &ManagedVolumeSpec, ctx: &VolumeContext) -> Result<()>;
    fn on_destroy(&self, spec: &ManagedVolumeSpec, ctx: &VolumeContext) -> Result<()>;
}
