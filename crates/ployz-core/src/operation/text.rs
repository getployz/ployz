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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NonEmptyTextError {
    #[error("text must not be empty")]
    Empty,
}
