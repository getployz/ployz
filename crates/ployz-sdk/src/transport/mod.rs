mod stdio;
mod unix;

use crate::{ControlRequest, ControlResponse};
use std::future::Future;

pub use stdio::StdioTransport;
pub use unix::UnixSocketTransport;

pub trait Transport: Send + Sync {
    fn request(
        &self,
        request: ControlRequest,
    ) -> impl Future<Output = std::io::Result<ControlResponse>> + Send + '_;
}
