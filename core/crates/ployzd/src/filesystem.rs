use std::{
    fs, io,
    os::unix::fs::{MetadataExt, chown},
    path::{Path, PathBuf},
};

use ployz_core::private_fs::{Durability, Replace, TempName, replace_file};

/// Mode of the Machine API socket, whether systemd or ployzd binds it.
pub(crate) const MACHINE_API_SOCKET_MODE: u32 = 0o660;
/// Mode of the Ployz runtime and data directories.
pub(crate) const PLOYZ_DIR_MODE: u32 = 0o750;

/// The file [`atomic_write`] writes before it renames it over `path`.
pub(crate) fn temporary_path(path: &Path) -> PathBuf {
    TempName::Sibling.path(path)
}

pub fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    replace_file(
        path,
        contents,
        Replace {
            mode,
            durability: Durability::FileAndDirectory,
            temp: TempName::Sibling,
        },
    )
    .map_err(io::Error::from)
}

pub fn set_ployz_group(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    let gid = if fs::metadata("/proc/self")?.uid() == 0 {
        group_gid("ployz").unwrap_or(0)
    } else {
        metadata.gid()
    };
    chown(path, None, Some(gid))
}

fn group_gid(name: &str) -> Option<u32> {
    fs::read_to_string("/etc/group")
        .ok()?
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            (fields.next()? == name)
                .then(|| fields.nth(1)?.parse().ok())
                .flatten()
        })
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::MetadataExt as _};

    use super::{atomic_write, temporary_path};

    #[test]
    fn an_atomic_write_replaces_the_file_at_its_mode_and_reuses_a_stale_sibling_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("spec.json");
        assert_eq!(temporary_path(&path), directory.path().join("spec.tmp"));
        fs::write(&path, b"old").unwrap();
        fs::write(
            temporary_path(&path),
            b"stale and longer than the new bytes",
        )
        .unwrap();
        atomic_write(&path, b"new", 0o640).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
        assert!(!temporary_path(&path).exists());
    }

    #[test]
    fn an_atomic_write_that_fails_returns_the_io_error() {
        let directory = tempfile::tempdir().unwrap();
        let error =
            atomic_write(&directory.path().join("absent").join("file"), b"x", 0o600).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
