mod stdio;
mod unix;

use ployz_api::{DaemonRequest, DaemonResponse};
use std::future::Future;

pub use stdio::StdioTransport;
pub use unix::UnixSocketTransport;

pub trait Transport: Send + Sync {
    fn request(
        &self,
        request: DaemonRequest,
    ) -> impl Future<Output = std::io::Result<DaemonResponse>> + Send + '_;
}
