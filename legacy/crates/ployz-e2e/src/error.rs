use thiserror::Error;

pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("io: {0}")]
    Io(String),
    #[error("command failed: {command}\nstdout:\n{stdout}\nstderr:\n{stderr}")]
    CommandFailed {
        command: String,
        stdout: String,
        stderr: String,
    },
    #[error("{0}")]
    Message(String),
}
