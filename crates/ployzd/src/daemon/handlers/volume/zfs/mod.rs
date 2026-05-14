mod handlers;
mod local;
mod machine_lookup;
mod responses;
mod rpc;
mod send;
mod store;
#[cfg(test)]
mod tests;
mod transfer_execution;

pub(in crate::daemon::handlers::volume::zfs) use store::volume_dataset;
