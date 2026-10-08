//! Deletes stored log files to keep the Log Store under its limits.
//!
//! Cleanup deletes only what the store itself created: a `<seq>-<ino>.log`
//! file whose name parses, inside a dir whose name parses as a container id,
//! reached without following a symlink. Every delete is an `unlinkat`
//! relative to a dir it opened with `O_NOFOLLOW`, so nothing in the store
//! can point a delete elsewhere.
//!
//! A file Docker still links costs no disk the store adds, and unlinking the
//! store's link would free nothing, so only files the store alone holds
//! (`nlink == 1`) count toward the cap and are deleted.

use std::{
    ffi::CString,
    io,
    os::fd::{AsRawFd, RawFd},
    time::{Duration, SystemTime},
};

use nix::{
    dir::{Dir, Type},
    errno::Errno,
    fcntl::{AtFlags, OFlag},
    sys::stat::{Mode, SFlag, fstatat},
    unistd::{UnlinkatFlags, unlinkat},
};
use ployz_core::ContainerId;

use super::layout::{
    CONTAINERS_DIR, GAPS_FILE, LINKING_FILE, LogFileName, META_FILE, META_TEMP_FILE, StoreRoot,
    VERSION_DIR,
};

const GIB: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Bytes the store may hold alone.
    pub cap_bytes: u64,
    /// Free bytes the filesystem must keep; below it the store gives way.
    pub free_floor_bytes: u64,
    pub max_age: Duration,
}

impl Limits {
    /// The limits for a filesystem of `total_bytes`: a cap of 5% held between
    /// 512 MiB and 5 GiB, a floor of 10% free but never under 2 GiB, and 30
    /// days of age.
    #[must_use]
    pub fn for_filesystem(total_bytes: u64) -> Self {
        Self {
            cap_bytes: (total_bytes / 20).clamp(GIB / 2, 5 * GIB),
            free_floor_bytes: (total_bytes / 10).max(2 * GIB),
            max_age: Duration::from_secs(30 * 24 * 60 * 60),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Disk {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl Disk {
    /// Measures the filesystem holding `store`.
    ///
    /// # Errors
    ///
    /// Returns the `statvfs` error.
    pub fn measure(store: &StoreRoot) -> io::Result<Self> {
        let stat = nix::sys::statvfs::statvfs(store.path())?;
        let fragment = stat.fragment_size();
        Ok(Self {
            total_bytes: stat.blocks().saturating_mul(fragment),
            free_bytes: stat.blocks_available().saturating_mul(fragment),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Report {
    pub files_deleted: usize,
    pub bytes_freed: u64,
    pub symlinks_removed: usize,
    pub containers_removed: usize,
}

struct Candidate {
    container: CString,
    name: CString,
    order: (i64, String, u64),
    size: u64,
    allocated_bytes: u64,
    dev: u64,
    ino: u64,
}

struct ContainerDir {
    name: CString,
    id: ContainerId,
    log_files: usize,
    foreign: usize,
}

const DIR_FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_DIRECTORY)
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_CLOEXEC);

/// Runs one cleanup pass. `container_exists` says whether Docker still has a
/// container; a store dir is removed once its container is gone and it holds
/// no log files.
///
/// # Errors
///
/// Returns an error when the store's containers dir cannot be read. Errors on
/// one container or file are logged and skipped.
pub fn run(
    store: &StoreRoot,
    limits: &Limits,
    disk: Disk,
    now: SystemTime,
    container_exists: impl Fn(&ContainerId) -> bool,
) -> io::Result<Report> {
    let mut containers = open_containers(store)?;
    let containers_fd = containers.as_raw_fd();
    let mut report = Report::default();
    let mut candidates = Vec::new();
    let mut dirs = Vec::new();
    let names: Vec<CString> = containers
        .iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type() != Some(Type::Symlink))
        .map(|entry| entry.file_name().to_owned())
        .collect();
    for name in names {
        let Some(id) = name
            .to_str()
            .ok()
            .and_then(|id| ContainerId::parse(id).ok())
        else {
            continue;
        };
        match scan_container(containers_fd, name, &id, &mut candidates, &mut report) {
            Ok(dir) => dirs.push(dir),
            Err(error) => tracing::warn!(container = %id, %error, "skipping a Log Store dir"),
        }
    }

    let age_limit = now
        .checked_sub(limits.max_age)
        .and_then(|limit| limit.duration_since(SystemTime::UNIX_EPOCH).ok())
        .and_then(|limit| i64::try_from(limit.as_secs()).ok())
        .unwrap_or(i64::MIN);
    candidates.sort_by(|left, right| left.order.cmp(&right.order));
    let mut held: u64 = candidates.iter().map(|candidate| candidate.size).sum();
    let mut released: u64 = 0;
    for candidate in &candidates {
        let too_old = candidate.order.0 < age_limit;
        let over_cap = held > limits.cap_bytes;
        let under_floor = disk.free_bytes.saturating_add(released) < limits.free_floor_bytes;
        if !(too_old || over_cap || under_floor) {
            continue;
        }
        match delete_file(containers_fd, candidate) {
            Ok(()) => {
                held -= candidate.size;
                released += candidate.allocated_bytes;
                report.files_deleted += 1;
                report.bytes_freed += candidate.size;
                if let Some(dir) = dirs.iter_mut().find(|dir| dir.name == candidate.container) {
                    dir.log_files -= 1;
                }
            }
            Err(error) => {
                tracing::warn!(?candidate.name, %error, "could not delete a stored log file")
            }
        }
    }

    for dir in dirs
        .iter()
        .filter(|dir| dir.log_files == 0 && dir.foreign == 0 && !container_exists(&dir.id))
    {
        match remove_container_dir(containers_fd, &dir.name) {
            Ok(true) => report.containers_removed += 1,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(container = %dir.id, %error, "could not remove a Log Store dir")
            }
        }
    }
    Ok(report)
}

/// Opens the containers dir one component at a time from
/// `<DockerRootDir>/ployz-observe`, so a symlink anywhere in the store's own
/// path stops cleanup instead of redirecting it.
fn open_containers(store: &StoreRoot) -> io::Result<Dir> {
    let base = Dir::open(store.base(), DIR_FLAGS, Mode::empty())?;
    let version = Dir::openat(
        Some(base.as_raw_fd()),
        VERSION_DIR,
        DIR_FLAGS,
        Mode::empty(),
    )?;
    Ok(Dir::openat(
        Some(version.as_raw_fd()),
        CONTAINERS_DIR,
        DIR_FLAGS,
        Mode::empty(),
    )?)
}

fn scan_container(
    containers_fd: RawFd,
    name: CString,
    id: &ContainerId,
    candidates: &mut Vec<Candidate>,
    report: &mut Report,
) -> nix::Result<ContainerDir> {
    let mut dir = Dir::openat(
        Some(containers_fd),
        name.as_c_str(),
        DIR_FLAGS,
        Mode::empty(),
    )?;
    let dir_fd = dir.as_raw_fd();
    let mut log_files = 0;
    let mut foreign = 0;
    let entries: Vec<CString> = dir
        .iter()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_owned())
        .collect();
    for file in entries {
        let Some(parsed) = LogFileName::parse(file.to_bytes()) else {
            if !is_store_file(&file) {
                foreign += 1;
            }
            continue;
        };
        let stat = fstatat(Some(dir_fd), file.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW)?;
        let kind = SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT;
        if kind == SFlag::S_IFLNK {
            unlinkat(Some(dir_fd), file.as_c_str(), UnlinkatFlags::NoRemoveDir)?;
            report.symlinks_removed += 1;
            continue;
        }
        if kind != SFlag::S_IFREG {
            foreign += 1;
            continue;
        }
        log_files += 1;
        if stat.st_nlink == 1 {
            candidates.push(Candidate {
                container: name.clone(),
                name: file,
                order: (stat.st_mtime, id.as_str().to_owned(), parsed.seq),
                size: u64::try_from(stat.st_size).unwrap_or(0),
                allocated_bytes: u64::try_from(stat.st_blocks)
                    .unwrap_or(0)
                    .saturating_mul(512),
                dev: stat.st_dev,
                ino: stat.st_ino,
            });
        }
    }
    Ok(ContainerDir {
        name,
        id: *id,
        log_files,
        foreign,
    })
}

const STORE_FILES: [&str; 4] = [META_FILE, META_TEMP_FILE, GAPS_FILE, LINKING_FILE];

fn is_store_file(name: &CString) -> bool {
    let name = name.to_bytes();
    name == b"." || name == b".." || STORE_FILES.iter().any(|file| file.as_bytes() == name)
}

fn delete_file(containers_fd: RawFd, candidate: &Candidate) -> nix::Result<()> {
    let dir = Dir::openat(
        Some(containers_fd),
        candidate.container.as_c_str(),
        DIR_FLAGS,
        Mode::empty(),
    )?;
    let stat = fstatat(
        Some(dir.as_raw_fd()),
        candidate.name.as_c_str(),
        AtFlags::AT_SYMLINK_NOFOLLOW,
    )?;
    let kind = SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT;
    if kind != SFlag::S_IFREG
        || stat.st_dev != candidate.dev
        || stat.st_ino != candidate.ino
        || stat.st_nlink != 1
    {
        return Err(Errno::ESTALE);
    }
    unlinkat(
        Some(dir.as_raw_fd()),
        candidate.name.as_c_str(),
        UnlinkatFlags::NoRemoveDir,
    )
}

fn remove_container_dir(containers_fd: RawFd, name: &CString) -> nix::Result<bool> {
    let dir = Dir::openat(
        Some(containers_fd),
        name.as_c_str(),
        DIR_FLAGS,
        Mode::empty(),
    )?;
    for file in STORE_FILES {
        match unlinkat(Some(dir.as_raw_fd()), file, UnlinkatFlags::NoRemoveDir) {
            Ok(()) | Err(Errno::ENOENT) => {}
            Err(error) => return Err(error),
        }
    }
    drop(dir);
    match unlinkat(
        Some(containers_fd),
        name.as_c_str(),
        UnlinkatFlags::RemoveDir,
    ) {
        Ok(()) => Ok(true),
        Err(Errno::ENOTEMPTY | Errno::EEXIST | Errno::ENOENT) => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{MetadataExt as _, symlink},
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    use ployz_core::ContainerId;

    use super::{Disk, Limits, Report, run};
    use crate::{observe::layout::StoreRoot, test_dir::TestDir};

    const T0: u64 = 1_760_000_000;

    struct Store {
        _dir: TestDir,
        root: StoreRoot,
        docker: PathBuf,
    }

    fn store() -> Store {
        let dir = TestDir::new("ployzd-observe-cleanup");
        fs::create_dir_all(&dir.0).unwrap();
        let root = StoreRoot::under(&dir.0);
        root.prepare().unwrap();
        let docker = dir.0.join("docker-files");
        fs::create_dir_all(&docker).unwrap();
        Store {
            _dir: dir,
            root,
            docker,
        }
    }

    fn cid(n: u8) -> ContainerId {
        ContainerId::parse(format!("{n:02x}").repeat(32)).unwrap()
    }

    impl Store {
        fn file(&self, container: &ContainerId, name: &str, size: usize, age_secs: u64) -> PathBuf {
            let dir = self.root.container(container);
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            fs::write(&path, vec![b'x'; size]).unwrap();
            let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(T0 - age_secs);
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
            path
        }

        fn linked_by_docker(&self, path: &PathBuf) {
            let name = path.file_name().unwrap();
            fs::hard_link(path, self.docker.join(name)).unwrap();
        }

        fn names(&self, container: &ContainerId) -> Vec<String> {
            let Ok(entries) = fs::read_dir(self.root.container(container)) else {
                return Vec::new();
            };
            let mut names: Vec<String> = entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }

        fn clean(&self, cap_bytes: u64, free_bytes: u64, gone: &[ContainerId]) -> Report {
            let limits = Limits {
                cap_bytes,
                free_floor_bytes: 0,
                max_age: Duration::from_secs(30 * 86_400),
            };
            let disk = Disk {
                total_bytes: u64::MAX,
                free_bytes,
            };
            let now = SystemTime::UNIX_EPOCH + Duration::from_secs(T0);
            run(&self.root, &limits, disk, now, |id| !gone.contains(id)).unwrap()
        }
    }

    #[test]
    fn limits_scale_with_the_filesystem_within_bounds() {
        let gib = 1_u64 << 30;
        let small = Limits::for_filesystem(4 * gib);
        assert_eq!(
            (small.cap_bytes, small.free_floor_bytes),
            (gib / 2, 2 * gib)
        );
        let mid = Limits::for_filesystem(40 * gib);
        assert_eq!((mid.cap_bytes, mid.free_floor_bytes), (2 * gib, 4 * gib));
        let large = Limits::for_filesystem(4000 * gib);
        assert_eq!(
            (large.cap_bytes, large.free_floor_bytes),
            (5 * gib, 400 * gib)
        );
    }

    #[test]
    fn under_the_cap_nothing_is_deleted() {
        let store = store();
        store.file(&cid(1), "1-10.log", 100, 60);
        store.file(&cid(1), "2-11.log", 100, 30);
        assert_eq!(store.clean(200, u64::MAX, &[]), Report::default());
        assert_eq!(store.names(&cid(1)), ["1-10.log", "2-11.log"]);
    }

    #[test]
    fn over_the_cap_the_oldest_files_go_first_and_files_docker_links_are_kept() {
        let store = store();
        store.file(&cid(1), "1-10.log", 100, 50);
        store.file(&cid(2), "1-20.log", 100, 40);
        store.file(&cid(1), "2-11.log", 100, 30);
        let live = store.file(&cid(1), "3-12.log", 1000, 90);
        store.linked_by_docker(&live);

        let report = store.clean(150, u64::MAX, &[]);
        assert_eq!((report.files_deleted, report.bytes_freed), (2, 200));
        assert_eq!(store.names(&cid(1)), ["2-11.log", "3-12.log"]);
        assert_eq!(store.names(&cid(2)), Vec::<String>::new());

        assert_eq!(store.clean(150, u64::MAX, &[]), Report::default());
    }

    #[test]
    fn files_past_the_age_limit_go_and_the_free_floor_takes_more() {
        let store = store();
        let old = store.file(&cid(1), "1-10.log", 100, 31 * 86_400);
        store.file(&cid(1), "2-11.log", 100, 60);
        store.file(&cid(1), "3-12.log", 100, 30);
        let allocated_bytes = fs::metadata(old).unwrap().blocks() * 512;
        let limits = Limits {
            cap_bytes: u64::MAX,
            free_floor_bytes: 1_000 + allocated_bytes + 1,
            max_age: Duration::from_secs(30 * 86_400),
        };
        let disk = Disk {
            total_bytes: u64::MAX,
            free_bytes: 1_000,
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(T0);
        let report = run(&store.root, &limits, disk, now, |_| true).unwrap();
        assert_eq!(report.files_deleted, 2);
        assert_eq!(store.names(&cid(1)), ["3-12.log"]);
    }

    #[test]
    fn symlinks_are_removed_without_touching_their_targets_and_foreign_files_stay() {
        let store = store();
        let outside = store.docker.join("precious");
        fs::write(&outside, b"keep").unwrap();
        let dir = store.root.container(&cid(1));
        fs::create_dir_all(&dir).unwrap();
        symlink(&outside, dir.join("1-10.log")).unwrap();
        fs::write(dir.join("notes.txt"), b"keep").unwrap();
        fs::write(dir.join("01-10.log"), b"keep").unwrap();
        symlink(&store.docker, store.root.containers().join(cid(2).as_str())).unwrap();
        fs::write(store.docker.join("1-1.log"), b"keep").unwrap();

        let report = store.clean(0, u64::MAX, &[cid(1), cid(2)]);
        assert_eq!(report.symlinks_removed, 1);
        assert_eq!(report.files_deleted, 0);
        assert_eq!(fs::read(&outside).unwrap(), b"keep");
        assert_eq!(store.names(&cid(1)), ["01-10.log", "notes.txt"]);
        assert_eq!(fs::read(store.docker.join("1-1.log")).unwrap(), b"keep");
    }

    #[test]
    fn a_symlink_in_the_store_path_stops_cleanup() {
        let store = store();
        let file = store.file(&cid(1), "1-10.log", 100, 31 * 86_400);
        let real = store.root.base().join("real");
        fs::rename(store.root.path(), &real).unwrap();
        symlink(&real, store.root.path()).unwrap();
        let limits = Limits::for_filesystem(u64::MAX);
        let disk = Disk {
            total_bytes: u64::MAX,
            free_bytes: u64::MAX,
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(T0);
        assert!(run(&store.root, &limits, disk, now, |_| false).is_err());
        assert!(fs::exists(&file).unwrap());
    }

    #[test]
    fn a_dir_is_removed_once_its_container_is_gone_and_its_files_are() {
        let store = store();
        store.file(&cid(1), "meta.json", 10, 0);
        store.file(&cid(1), "gaps.jsonl", 10, 0);
        store.file(&cid(2), "meta.json", 10, 0);
        store.file(&cid(3), "meta.json", 10, 0);
        store.file(&cid(3), "1-30.log", 10, 0);

        let report = store.clean(u64::MAX, u64::MAX, &[cid(1), cid(3)]);
        assert_eq!(report.containers_removed, 1);
        assert!(!store.root.container(&cid(1)).exists());
        assert!(store.root.container(&cid(2)).exists());
        assert!(store.root.container(&cid(3)).exists());

        let report = store.clean(0, u64::MAX, &[cid(1), cid(3)]);
        assert_eq!((report.files_deleted, report.containers_removed), (1, 1));
        assert!(!store.root.container(&cid(3)).exists());
    }

    #[test]
    fn a_dir_holding_anything_the_store_did_not_write_keeps_its_metadata() {
        let store = store();
        store.file(&cid(1), "meta.json", 10, 0);
        store.file(&cid(1), "gaps.jsonl", 10, 0);
        store.file(&cid(1), "notes.txt", 10, 0);

        let report = store.clean(u64::MAX, u64::MAX, &[cid(1)]);
        assert_eq!(report.containers_removed, 0);
        assert_eq!(
            store.names(&cid(1)),
            ["gaps.jsonl", "meta.json", "notes.txt"]
        );
    }
}
