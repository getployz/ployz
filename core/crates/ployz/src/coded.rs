//! What `--json` says about an error: its code, the evidence it carries and the hints
//! it gives. Each error of this crate implements [`Coded`] beside its type; the errors
//! of other crates implement it here.

use std::{error::Error, io};

use ployz_core::{
    CodecError, ContainerSelectorError, Hint, MachineSelectorError, MachineUpdateError, RpcError,
    RpcErrorCode, ServiceSelectorError, StreamProtocolError, UnconfirmedDataLoss, ValueError,
};
use serde_json::Value;

/// An error with a code in the RPC error vocabulary.
pub(crate) trait Coded: Error + Send + Sync + 'static {
    /// The `--json` error code.
    fn code(&self) -> RpcErrorCode;

    /// Machine-readable evidence for `details`, beside the keys `hints` write.
    fn evidence(&self) -> Option<Value> {
        None
    }

    /// What the reader can do about it, carried in `details` as hint keys.
    fn hints(&self) -> Vec<Hint> {
        Vec::new()
    }
}

impl Coded for MachineSelectorError {
    fn code(&self) -> RpcErrorCode {
        match self {
            MachineSelectorError::NoTargets => RpcErrorCode::InvalidArgument,
            MachineSelectorError::NoVisibleMachines | MachineSelectorError::NotFound(_) => {
                RpcErrorCode::NotFound
            }
            MachineSelectorError::Ambiguous { .. } => RpcErrorCode::Ambiguous,
        }
    }

    fn hints(&self) -> Vec<Hint> {
        match self {
            Self::NotFound(_) => vec![Hint::Inspect("ployz server ls".into())],
            Self::NoTargets | Self::NoVisibleMachines | Self::Ambiguous { .. } => Vec::new(),
        }
    }
}

impl Coded for CodecError {
    fn code(&self) -> RpcErrorCode {
        match self {
            CodecError::UnsupportedCommand(_) | CodecError::UnsupportedProtocolMajor { .. } => {
                RpcErrorCode::Unsupported
            }
            CodecError::EncodeJson(_)
            | CodecError::DecodeJson(_)
            | CodecError::UnexpectedResponse { .. }
            | CodecError::UnexpectedRequest { .. } => RpcErrorCode::Internal,
        }
    }
}

impl Coded for RpcError {
    fn code(&self) -> RpcErrorCode {
        self.code.clone()
    }

    fn evidence(&self) -> Option<Value> {
        Some(self.details.clone())
    }
}

impl Coded for io::Error {
    fn code(&self) -> RpcErrorCode {
        use io::ErrorKind;
        let kind = self.kind();
        if kind == ErrorKind::NotFound {
            RpcErrorCode::NotFound
        } else if kind == ErrorKind::InvalidInput {
            RpcErrorCode::InvalidArgument
        } else if matches!(
            kind,
            ErrorKind::ConnectionRefused
                | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::NotConnected
                | ErrorKind::TimedOut
        ) {
            RpcErrorCode::Unavailable
        } else {
            RpcErrorCode::Internal
        }
    }
}

impl Coded for MachineUpdateError {
    fn code(&self) -> RpcErrorCode {
        match self {
            Self::DuplicateName => RpcErrorCode::Conflict,
            Self::MissingEndpoints => RpcErrorCode::InvalidArgument,
        }
    }
}

impl Coded for ServiceSelectorError {
    fn code(&self) -> RpcErrorCode {
        match self {
            Self::NotFound { .. } => RpcErrorCode::NotFound,
            Self::NameAmbiguity { .. } => RpcErrorCode::Ambiguous,
        }
    }
}

impl Coded for ContainerSelectorError {
    fn code(&self) -> RpcErrorCode {
        match self {
            Self::NotFound { .. } => RpcErrorCode::NotFound,
            Self::Ambiguous { .. } => RpcErrorCode::Ambiguous,
        }
    }

    fn hints(&self) -> Vec<Hint> {
        match self {
            Self::NotFound { valid, .. } => vec![Hint::valid(valid)],
            Self::Ambiguous { .. } => Vec::new(),
        }
    }
}

/// Bad input, whatever its wording.
macro_rules! invalid_argument {
    ($($t:ty),+ $(,)?) => {
        $(impl Coded for $t {
            fn code(&self) -> RpcErrorCode {
                RpcErrorCode::InvalidArgument
            }
        })+
    };
}

invalid_argument!(
    ValueError,
    std::num::ParseIntError,
    shell_words::ParseError,
    UnconfirmedDataLoss,
);

impl Coded for StreamProtocolError {
    fn code(&self) -> RpcErrorCode {
        RpcErrorCode::Internal
    }
}

impl Coded for serde_json::Error {
    fn code(&self) -> RpcErrorCode {
        RpcErrorCode::Internal
    }
}
