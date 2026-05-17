use thiserror::Error;

#[derive(Debug, Error)]
pub enum IrohFactError {
    #[error("iroh fact operation unavailable: {operation}")]
    Unavailable { operation: &'static str },
}

pub type IrohFactResult<T> = std::result::Result<T, IrohFactError>;
