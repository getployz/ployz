pub mod protocol;
pub mod stdio;
pub mod unix;

pub use protocol::{
    DaemonPayload, DaemonRequest, DaemonResponse, DebugTickTask, DeployFrame, DeployOptions,
    InstallSource, MachineAddOptions, MachineAddPayload, MachineAwaitingSelfPublication,
    MachineInstallOptions, MachineListPayload, MachineListRow, MachineOperationInfo,
    MachineOperationListPayload, MachineOperationPayload, MachineRemovePayload, MeshReadyPayload,
    MeshSelfRecordPayload,
};
pub use stdio::StdioTransport;
pub use unix::UnixSocketTransport;

use std::future::Future;

pub trait Transport: Send + Sync {
    fn request(
        &self,
        req: DaemonRequest,
    ) -> impl Future<Output = std::io::Result<DaemonResponse>> + Send + '_;
}
