mod config;
mod resolve;
mod server;
mod snapshot;
mod sync;

pub use config::{DnsConfig, DnsError};
pub use resolve::{DnsQuery, ResolveResult, parse_query, resolve};
pub use server::{run_dns_process_with_store, run_dns_server};
pub use snapshot::{DnsSnapshot, SharedDnsSnapshot, project_dns};
pub use sync::{run_sync_loop, spawn_sync_thread_with_store};
