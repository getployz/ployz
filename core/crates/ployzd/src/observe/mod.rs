//! The Log Store: Docker's `local` log files for Ployz containers, kept
//! after Docker would delete them.

pub mod cleanup;
mod client;
pub mod frame;
mod harvest;
#[cfg(test)]
mod integration_tests;
pub mod layout;
mod query;
mod serve;

use std::{
    fs,
    fs::File,
    io,
    io::Write,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
};

use bollard::Docker;
pub use client::ObserveClient;
use frame::{Event, Frames, Stream};
use fs2::FileExt;
use layout::{LogFileName, StoreRoot};
use tokio::net::UnixListener;

const DOCKER_LOG_FILE: &str = "container.log";
pub const SOCKET_FILE: &str = "observe.sock";
const LOCK_FILE: &str = "harvester.lock";

/// Runs the harvester: holds the log files of every Ployz container in the
/// Log Store until SIGTERM.
///
/// # Errors
///
/// Returns an error when Docker cannot be reached, another harvester holds
/// the store, or the store or socket cannot be created.
pub async fn run(run_dir: &Path) -> io::Result<()> {
    crate::faults::check_env()?;
    crate::faults::leak_memory();
    let docker = Docker::connect_with_defaults().map_err(io::Error::other)?;
    let docker_root = docker
        .info()
        .await
        .map_err(io::Error::other)?
        .docker_root_dir
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("Docker did not report its root dir"))?;
    let store = StoreRoot::under(&docker_root);
    let _lock = lock_store(&store)?;
    store.prepare()?;
    let listener = bind_socket(&run_dir.join(SOCKET_FILE))?;
    tracing::info!(store = %store.path().display(), "holding Ployz container logs");
    tokio::spawn(serve::serve(listener, store.clone()));
    harvest::Harvester::run(docker, &docker_root, store).await
}

fn lock_store(store: &StoreRoot) -> io::Result<File> {
    layout::create_store_dir(store.base())?;
    let lock = File::create(store.base().join(LOCK_FILE))?;
    lock.try_lock_exclusive().map_err(|error| {
        io::Error::new(
            error.kind(),
            "another ployzd observe is already holding this Log Store",
        )
    })?;
    Ok(lock)
}

fn bind_socket(path: &Path) -> io::Result<UnixListener> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Prints log files as `docker logs --timestamps` prints them, stdout lines
/// to stdout and stderr lines to stderr. A dir prints its files oldest first,
/// whether it is a store container dir or Docker's `local-logs` dir.
///
/// # Errors
///
/// Returns an error when a path cannot be read or output cannot be written.
pub fn decode(paths: &[PathBuf]) -> io::Result<()> {
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut err = io::BufWriter::new(stderr.lock());
    for path in paths {
        for file in files_oldest_first(path)? {
            let bytes = fs::read(&file)?;
            let mut frames = Frames::new(&bytes);
            for event in frames.by_ref() {
                match event {
                    Event::Entry(entry) => match entry.stream {
                        Stream::Stdout => frame::write_docker_style(&entry, &mut out)?,
                        Stream::Stderr => {
                            out.flush()?;
                            frame::write_docker_style(&entry, &mut err)?;
                            err.flush()?;
                        }
                    },
                    Event::Corrupt { offset, skipped } => {
                        out.flush()?;
                        writeln!(
                            err,
                            "ployzd: {}: skipped {skipped} unreadable bytes at offset {offset}",
                            file.display()
                        )?;
                        err.flush()?;
                    }
                }
            }
        }
    }
    out.flush()?;
    err.flush()
}

fn files_oldest_first(path: &Path) -> io::Result<Vec<PathBuf>> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut stored = Vec::new();
    let mut docker = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        if let Some(stored_name) = LogFileName::parse(name.as_encoded_bytes()) {
            stored.push((stored_name.seq, entry.path()));
        } else if let Some(age) = docker_file_age(&name.to_string_lossy()) {
            docker.push((age, entry.path()));
        }
    }
    stored.sort();
    docker.sort_by_key(|(age, _)| std::cmp::Reverse(*age));
    Ok(stored
        .into_iter()
        .chain(docker)
        .map(|(_, path)| path)
        .collect())
}

pub(crate) fn docker_file_age(name: &str) -> Option<u64> {
    let rest = name.strip_prefix(DOCKER_LOG_FILE)?;
    if rest.is_empty() {
        return Some(0);
    }
    let digits = rest.strip_prefix('.')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}
