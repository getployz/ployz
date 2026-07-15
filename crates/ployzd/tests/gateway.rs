//! Black-box Gateway role RPC, process, source, and serving behavior.

mod support;

#[path = "gateway/certificate_rpc.rs"]
mod certificate_rpc;
#[path = "gateway/process.rs"]
mod process;
#[path = "gateway/source.rs"]
mod source;
