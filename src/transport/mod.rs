pub mod listener;
pub mod local_socket;

pub use local_socket::{DaemonRequest, DaemonResponse, UnixSocketTransport};

use std::future::Future;

pub trait Transport: Send + Sync {
    fn request(
        &self,
        req: DaemonRequest,
    ) -> impl Future<Output = std::io::Result<DaemonResponse>> + Send + '_;
}
