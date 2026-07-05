//! Small helpers for JSON evidence files owned by one local process.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) enum EvidenceFileError {
    Read { message: String },
    Decode { message: String },
    Encode { message: String },
    Write { message: String },
}

pub(crate) fn read_json_or_default<T>(path: &Path) -> Result<T, EvidenceFileError>
where
    T: DeserializeOwned + Default,
{
    let payload = match std::fs::read(path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => {
            return Err(EvidenceFileError::Read {
                message: error.to_string(),
            });
        }
    };
    serde_json::from_slice(&payload).map_err(|error| EvidenceFileError::Decode {
        message: error.to_string(),
    })
}

pub(crate) fn write_json<T>(path: &Path, evidence: &T) -> Result<(), EvidenceFileError>
where
    T: Serialize,
{
    let payload =
        serde_json::to_vec_pretty(evidence).map_err(|error| EvidenceFileError::Encode {
            message: error.to_string(),
        })?;
    write_file_atomically(path, &payload).map_err(|error| EvidenceFileError::Write {
        message: error.to_string(),
    })
}

#[derive(Debug)]
pub(crate) struct AtomicFileWriteError {
    path: PathBuf,
    message: String,
}

impl std::fmt::Display for AtomicFileWriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for AtomicFileWriteError {}

pub(crate) fn write_file_atomically(
    path: &Path,
    contents: &[u8],
) -> Result<(), AtomicFileWriteError> {
    let write_error = |message: String| AtomicFileWriteError {
        path: path.to_path_buf(),
        message,
    };
    let Some(parent) = path.parent() else {
        return Err(write_error("path has no parent directory".to_owned()));
    };
    std::fs::create_dir_all(parent).map_err(|error| write_error(error.to_string()))?;
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| write_error(error.to_string()))?;
    file.write_all(contents)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| write_error(error.to_string()))?;
    file.persist(path)
        .map_err(|error| write_error(error.error.to_string()))?;
    sync_parent_directory(parent).map_err(|error| write_error(error.to_string()))
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), std::io::Error> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{EvidenceFileError, read_json_or_default, write_json};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct ExampleEvidence {
        values: Vec<String>,
    }

    #[test]
    fn missing_file_loads_default_and_written_json_round_trips() {
        let temp = tempfile::tempdir().expect("tempdir is created");
        let path = temp.path().join("nested").join("evidence.json");

        assert_eq!(
            read_json_or_default::<ExampleEvidence>(&path).expect("missing file defaults"),
            ExampleEvidence::default()
        );

        let evidence = ExampleEvidence {
            values: vec!["alpha".to_owned(), "beta".to_owned()],
        };
        write_json(&path, &evidence).expect("evidence is written atomically");

        assert_eq!(
            read_json_or_default::<ExampleEvidence>(&path).expect("written evidence reads"),
            evidence
        );
    }

    #[test]
    fn invalid_json_reports_decode_error() {
        let temp = tempfile::tempdir().expect("tempdir is created");
        let path = temp.path().join("evidence.json");
        std::fs::write(&path, b"{").expect("invalid json is written");

        let error = read_json_or_default::<ExampleEvidence>(&path)
            .expect_err("invalid json fails to decode");
        assert!(matches!(error, EvidenceFileError::Decode { .. }));
    }
}
