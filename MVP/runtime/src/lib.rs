#[cfg(feature = "docker")]
mod docker;
mod error;
mod model;
mod process;

#[cfg(feature = "docker")]
pub use docker::{DockerRuntime, DockerRuntimeConfig};
pub use error::{RuntimeError, RuntimeResult};
pub use model::{
    ManagedHttpCommand, ProcessInstance, ProcessInstanceSpec, ProcessRuntimeConfig,
    RuntimeInstance, RuntimeInstanceSpec, RuntimeInstanceState, RuntimeState,
};
pub use process::{ProcessRuntime, run_static_http_server};

pub trait RuntimeBackend: Send + Sync {
    fn prepare(&self, spec: &RuntimeInstanceSpec) -> RuntimeResult<RuntimeInstance>;
    fn start(&self, spec: &RuntimeInstanceSpec) -> RuntimeResult<RuntimeInstance>;
    fn drain(&self, instance_id: &mvp_deploy::InstanceId)
    -> RuntimeResult<Option<RuntimeInstance>>;
    fn stop(&self, instance_id: &mvp_deploy::InstanceId) -> RuntimeResult<Option<RuntimeInstance>>;
    fn list(&self) -> RuntimeResult<Vec<RuntimeInstance>>;

    fn adopt(&self) -> RuntimeResult<Vec<RuntimeInstance>> {
        self.list()
    }
}
