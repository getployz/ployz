use std::fs;
use std::io::Write as _;
use std::path::Path;

use defguard_wireguard_rs::key::Key;

use crate::{FileMode, StagedFile};

use super::{BuiltinWireguardHostError, host_error};

pub(super) fn provision_private_key(path: &Path) -> Result<String, BuiltinWireguardHostError> {
    match fs::read_to_string(path) {
        Ok(value) => return validate_private_key(path, value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(host_error(format!(
                "read WireGuard private key {}: {error}",
                path.display()
            )));
        }
    }
    let Some(directory) = path.parent() else {
        return Err(host_error(format!(
            "WireGuard private key path has no parent: {}",
            path.display()
        )));
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(directory).map_err(|error| {
        host_error(format!(
            "create WireGuard key directory {}: {error}",
            directory.display()
        ))
    })?;
    let private_key = Key::generate().to_string();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| host_error(format!("invalid private key file name: {}", path.display())))?;
    let mut staged = StagedFile::create(directory, file_name, "key", FileMode::Secret0600)
        .map_err(|error| host_error(format!("stage WireGuard private key: {error:?}")))?;
    staged
        .file()
        .write_all(format!("{private_key}\n").as_bytes())
        .and_then(|()| staged.file().sync_all())
        .map_err(|error| host_error(format!("write WireGuard private key: {error}")))?;
    match commit_new_key(&mut staged, path) {
        Ok(()) => Ok(private_key),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => fs::read_to_string(path)
            .map_err(|read_error| {
                host_error(format!(
                    "read concurrently-created WireGuard private key {}: {read_error}",
                    path.display()
                ))
            })
            .and_then(|value| validate_private_key(path, value)),
        Err(error) => Err(host_error(format!(
            "commit WireGuard private key {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(target_os = "linux")]
fn commit_new_key(staged: &mut StagedFile, path: &Path) -> std::io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    renameat_with(CWD, staged.path(), CWD, path, RenameFlags::NOREPLACE)
        .map_err(std::io::Error::from)?;
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(target_os = "linux"))]
fn commit_new_key(staged: &mut StagedFile, path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "WireGuard private key already exists",
        ));
    }
    staged.commit_to(path)
}

fn validate_private_key(path: &Path, value: String) -> Result<String, BuiltinWireguardHostError> {
    ensure_private_key_permissions(path)?;
    let value = value.trim().to_owned();
    Key::try_from(value.as_str()).map_err(|error| {
        host_error(format!(
            "parse WireGuard private key {}: {error}",
            path.display()
        ))
    })?;
    Ok(value)
}

fn ensure_private_key_permissions(path: &Path) -> Result<(), BuiltinWireguardHostError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            host_error(format!(
                "make WireGuard private key private {}: {error}",
                path.display()
            ))
        })?;
    }
    Ok(())
}
