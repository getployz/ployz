//! Black-box Machine role RPC, service, process, and observation behavior.

mod support;

#[path = "machine/rpc.rs"]
mod rpc;
#[path = "machine/runtime.rs"]
mod runtime;
#[path = "machine/service_runtime.rs"]
mod service_runtime;
