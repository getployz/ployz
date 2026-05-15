mod add;
mod bootstrap;
mod coordination;
mod init;
mod lifecycle;
pub(in crate::daemon::handlers::machine) mod remote;
pub(super) mod rollback;
mod target;

#[cfg(test)]
pub(in crate::daemon::handlers::machine) use self::coordination::release_reserved_subnet;
