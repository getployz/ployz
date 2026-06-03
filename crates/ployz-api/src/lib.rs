//! External Ployz control protocol DTOs.
//!
//! These types are the wire contract for CLI, cloud, and SDK consumers. Product
//! logic should convert at process boundaries and keep using core domain types.

pub mod error;
pub mod frame;
pub mod operation;
pub mod status;

pub use error::{ProtocolError, ProtocolErrorCode, ProtocolResult};
pub use frame::{ErrorFrame, OutputFrame, RequestFrame, RequestId, ResponseFrame, ResponsePayload};
