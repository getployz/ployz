//! The Log Store: Docker's `local` log files for Ployz containers, kept
//! after Docker would delete them.

pub mod cleanup;
pub mod frame;
pub mod layout;

use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};

use frame::{Event, Frames, Stream};
use layout::LogFileName;

const DOCKER_LOG_FILE: &str = "container.log";

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

/// How many rotations ago Docker wrote a `local-logs` file: 0 for
/// `container.log`, N for `container.log.N`.
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
