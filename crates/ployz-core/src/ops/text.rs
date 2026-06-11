use std::fmt;

use crate::wire::nonempty_text_newtype;

nonempty_text_newtype! {
    pub struct FailureMessage;
    ts_brand: "Brand<string, \"FailureMessage\">";
    error: NonEmptyTextError;
}

nonempty_text_newtype! {
    pub struct CancellationReason;
    ts_brand: "Brand<string, \"CancellationReason\">";
    error: NonEmptyTextError;
}

nonempty_text_newtype! {
    pub struct OperatorHint;
    ts_brand: "Brand<string, \"OperatorHint\">";
    error: NonEmptyTextError;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NonEmptyTextError {
    Empty,
}

impl fmt::Display for NonEmptyTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("text must not be empty"),
        }
    }
}
