pub mod protocol;
pub mod unix;

pub use protocol::{
    DaemonRequest, DaemonResponse, DebugTickTask, DeployFrame, DeployOptions, InstallMode,
    InstallSource, MachineAddOptions, MachineInstallOptions,
};
pub use unix::UnixSocketTransport;

use std::future::Future;

pub trait Transport: Send + Sync {
    fn request(
        &self,
        req: DaemonRequest,
    ) -> impl Future<Output = std::io::Result<DaemonResponse>> + Send + '_;
}
