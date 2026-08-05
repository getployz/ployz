#![forbid(unsafe_code)]

//! Local command-line shell for `ployz`.

pub mod commands;
pub mod deploy;
pub mod init;
pub mod join_client;
pub mod logs;
pub mod machine;
pub mod mesh;
pub mod namespace;
pub mod ops;
pub mod remote;
pub mod token;

pub use join_client::{JoinDoorClient, JoinDoorClientError};
