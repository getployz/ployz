#![forbid(unsafe_code)]

//! Local command-line shell for `ployz`.

pub mod commands;
pub mod diagnostics;
pub mod init;
pub mod join_client;
pub mod machine;
pub mod mesh;
pub mod remote;
pub mod removal;
pub mod token;

pub use join_client::{JoinDoorClient, JoinDoorClientError};
