//! The files an outbox keeps: entry names, the last seq handed out and the dropped count.

use std::{fs, io, path::Path};

use crate::written::staging;

const LAST_SEQ_FILE: &str = "last_seq";
const DROPPED_FILE: &str = "dropped";

/// The lowest seq [`next_seq`] hands out, since `last` starts at 0.
const FIRST_SEQ: u64 = 1;

fn next_seq(clock: u64, last: u64) -> u64 {
    clock.max(last.saturating_add(1))
}

#[derive(Debug)]
pub(super) struct Ledger {
    pub(super) last_seq: u64,
    pub(super) dropped: u64,
}

impl Ledger {
    pub(super) fn open(dir: &Path, newest: u64) -> Self {
        Self {
            last_seq: read_counter(&dir.join(LAST_SEQ_FILE)).max(newest),
            dropped: read_counter(&dir.join(DROPPED_FILE)),
        }
    }

    pub(super) fn take_seq(&mut self, dir: &Path, clock: u64) -> io::Result<u64> {
        let seq = next_seq(clock, self.last_seq);
        write_counter(&dir.join(LAST_SEQ_FILE), seq)?;
        self.last_seq = seq;
        Ok(seq)
    }

    pub(super) fn count_dropped(&mut self, dir: &Path, count: usize) -> io::Result<()> {
        self.dropped = self
            .dropped
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        write_counter(&dir.join(DROPPED_FILE), self.dropped)
    }
}

const ENTRY_NAME_LEN: usize = 20;

pub(super) fn entry_name(seq: u64, suffix: &str) -> String {
    format!("{seq:0ENTRY_NAME_LEN$}{suffix}")
}

/// A file an outbox keeps.
pub(super) enum File {
    /// `last_seq`, which [`Ledger::take_seq`] writes, or `dropped`, which
    /// [`Ledger::count_dropped`] writes.
    Counter,
    /// An entry as [`entry_name`] spells it, with one of the outbox's suffixes.
    Entry { seq: u64, suffix: &'static str },
}

/// The file `name` is in an outbox whose entries carry one of `suffixes`, and whether it is
/// that file staged by `atomic_write`.
pub(super) fn file(name: &str, suffixes: &[&'static str]) -> Option<(File, bool)> {
    let seq = name
        .get(..ENTRY_NAME_LEN)
        .and_then(|digits| digits.parse::<u64>().ok())
        .filter(|seq| *seq >= FIRST_SEQ);
    let entries = seq.into_iter().flat_map(|seq| {
        suffixes
            .iter()
            .map(move |suffix| (entry_name(seq, suffix), File::Entry { seq, suffix }))
    });
    [
        (LAST_SEQ_FILE.to_owned(), File::Counter),
        (DROPPED_FILE.to_owned(), File::Counter),
    ]
    .into_iter()
    .chain(entries)
    .find_map(|(target, file)| Some((file, staging(name, &target)?)))
}

pub(super) fn seqs(dir: &Path, suffix: &str) -> io::Result<Vec<u64>> {
    let mut seqs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str().and_then(|name| name.strip_suffix(suffix)) else {
            continue;
        };
        if name.len() == ENTRY_NAME_LEN
            && let Ok(seq) = name.parse()
        {
            seqs.push(seq);
        }
    }
    seqs.sort_unstable();
    Ok(seqs)
}

pub(super) fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_counter(path: &Path) -> u64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

fn write_counter(path: &Path, value: u64) -> io::Result<()> {
    crate::filesystem::atomic_write(path, value.to_string().as_bytes(), 0o600)
}
