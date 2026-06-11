use std::fmt;
use std::path::{Path, PathBuf};

use ployz_core::nats_config::NatsUserSeed;

/// Writes the first node's `node.seed` (`0600`). The named writer is ployzd
/// control, which runs on the same machine; this is a local file write.
pub fn write_node_seed_file(
    path: &Path,
    credentials: &NatsUserSeed,
) -> Result<(), NodeSeedWriteError> {
    let write_error = |message: String| NodeSeedWriteError::Write {
        path: path.to_path_buf(),
        message,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| write_error(error.to_string()))?;
    }
    std::fs::write(path, format!("{}\n", credentials.secret()))
        .map_err(|error| write_error(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| write_error(error.to_string()))?;
    }
    Ok(())
}

#[derive(Debug)]
pub enum NodeSeedWriteError {
    Write { path: PathBuf, message: String },
}

impl fmt::Display for NodeSeedWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write { path, message } => write!(
                formatter,
                "failed to write node seed file {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for NodeSeedWriteError {}
