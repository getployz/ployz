pub mod listener;
pub mod local_socket;
pub mod protocol;

pub use local_socket::UnixSocketTransport;
pub use protocol::{DaemonRequest, DaemonResponse};

use std::future::Future;

pub trait Transport: Send + Sync {
    fn request(
        &self,
        req: DaemonRequest,
    ) -> impl Future<Output = std::io::Result<DaemonResponse>> + Send + '_;
}
