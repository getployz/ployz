use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, chown},
    path::Path,
};

pub fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(mode)
        .open(&temporary)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    File::open(path.parent().expect("written file has a parent"))?.sync_all()
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
