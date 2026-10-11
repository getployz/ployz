//! Cumulative read and write bytes per ZFS dataset, from the objset kstats
//! OpenZFS keeps under `/proc/spl/kstat/zfs/<pool>/objset-<id>`.
//!
//! Each objset file names its dataset and counts bytes moved through the
//! ZPL for it since import. Reading them is a handful of small proc files,
//! so the walk is safe on the 10 second loop.

use std::{collections::BTreeMap, fs, path::Path};

pub(crate) const KSTAT_ROOT: &str = "/proc/spl/kstat/zfs";
const OBJSET_PREFIX: &str = "objset-";

/// Bytes read from and written to one dataset since its pool was imported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DatasetIo {
    pub(crate) read_bytes: u64,
    pub(crate) write_bytes: u64,
}

/// Reads every pool's objset kstats under `root`, keyed by dataset name.
///
/// A missing root, an unreadable pool dir, or a file that does not parse
/// leaves those datasets out; it never reports them as zero.
pub(crate) fn read(root: &Path) -> BTreeMap<String, DatasetIo> {
    let Ok(pools) = fs::read_dir(root) else {
        return BTreeMap::new();
    };
    pools
        .flatten()
        .filter_map(|pool| fs::read_dir(pool.path()).ok())
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(OBJSET_PREFIX.as_bytes())
        })
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .filter_map(|text| parse_objset(&text))
        .collect()
}

/// Parses one objset kstat file into its dataset name and byte counters.
///
/// Returns `None` when the name or either byte counter is missing.
fn parse_objset(text: &str) -> Option<(String, DatasetIo)> {
    let mut name = None;
    let mut read_bytes = None;
    let mut write_bytes = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(key), Some(_type), Some(value)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        match key {
            "dataset_name" => name = Some(value.to_owned()),
            "nread" => read_bytes = value.parse().ok(),
            "nwritten" => write_bytes = value.parse().ok(),
            _ => {}
        }
    }
    Some((
        name?,
        DatasetIo {
            read_bytes: read_bytes?,
            write_bytes: write_bytes?,
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{DatasetIo, parse_objset, read};
    use crate::test_dir::TestDir;

    const CAPTURED: &str = include_str!("fixtures/objset-captured");
    const CAPTURED_DATASET: &str =
        "ployz/ployz/verify-production_vol-503b7412-8873-43da-ab49-049c9c5d94cb";
    const CAPTURED_IO: DatasetIo = DatasetIo {
        read_bytes: 0,
        write_bytes: 104_857_600,
    };

    #[test]
    fn a_captured_objset_file_yields_its_dataset_and_bytes() {
        let (name, io) = parse_objset(CAPTURED).expect("captured file parses");
        assert_eq!(name, CAPTURED_DATASET);
        assert_eq!(io, CAPTURED_IO);
    }

    #[test]
    fn a_file_missing_a_counter_is_skipped_not_zeroed() {
        let without_nread: String = CAPTURED
            .lines()
            .filter(|line| !line.starts_with("nread"))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(parse_objset(&without_nread), None);
        assert_eq!(
            parse_objset("dataset_name 7 ployz/x\nnread 4 x\nnwritten 4 1\n"),
            None
        );
    }

    #[test]
    fn read_walks_every_pool_and_only_objset_files() {
        let root = TestDir::new("zfs-io");
        let pool = root.0.join("ployz");
        let other = root.0.join("backup");
        fs::create_dir_all(&pool).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(root.0.join("arcstats"), "not a pool").unwrap();
        fs::write(pool.join("objset-0x36"), CAPTURED).unwrap();
        fs::write(
            pool.join("txgs"),
            "dataset_name 7 decoy\nnread 4 1\nnwritten 4 1\n",
        )
        .unwrap();
        fs::write(pool.join("objset-0x99"), "truncated").unwrap();
        fs::write(
            other.join("objset-0x10"),
            "dataset_name 7 backup/data\nnread 4 5\nnwritten 4 6\n",
        )
        .unwrap();

        let datasets = read(&root.0);

        assert_eq!(
            datasets.into_iter().collect::<Vec<_>>(),
            vec![
                (
                    "backup/data".to_owned(),
                    DatasetIo {
                        read_bytes: 5,
                        write_bytes: 6
                    }
                ),
                (CAPTURED_DATASET.to_owned(), CAPTURED_IO),
            ]
        );
    }

    #[test]
    fn a_missing_kstat_root_reads_as_no_datasets() {
        let root = TestDir::new("zfs-io-missing");
        assert!(read(&root.0).is_empty());
    }
}
