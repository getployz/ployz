pub mod protocol;
pub mod unix;

pub use protocol::{DaemonRequest, DaemonResponse, DeployFrame, DeployOptions};
pub use unix::UnixSocketTransport;

use std::future::Future;

pub trait Transport: Send + Sync {
    fn request(
        &self,
        req: DaemonRequest,
    ) -> impl Future<Output = std::io::Result<DaemonResponse>> + Send + '_;
}
