//! Where a Volume lives on the Machine Pool: its ZFS dataset names and mountpoints, and the
//! reverse reading of a dataset name.
//!
//! A Volume has a writer root at `<pool>/ployz/<name>`, or a mirror slot at
//! `<pool>/ployz-mirror/<name>` whose received copy is the `fs` child. Every route that
//! names or reads one of these goes through here.

use std::fmt::Display;

use ployz_core::{DATASET_ROOT, MIRROR_ROOT};

use crate::machine_pool::MachinePool;

/// Where Docker finds a writer root's data. The root parent dataset mounts here.
pub const MOUNT_ROOT: &str = "/var/lib/ployz-volumes";

/// The mountpoint every slot inherits from the mirror root dataset. Equal to
/// `/var/lib/{MIRROR_ROOT}` by coincidence of two unrelated names, so each stays its own
/// constant.
pub const MIRROR_MOUNT_ROOT: &str = "/var/lib/ployz-mirror";

const SLOT_FS: &str = "fs";

/// The dataset names of one Machine Pool.
#[derive(Clone, Copy, Debug)]
pub struct DatasetLayout<'pool> {
    pool: &'pool str,
}

impl<'pool> DatasetLayout<'pool> {
    /// The layout of the Pool named `pool`.
    #[must_use]
    pub fn new(pool: &'pool str) -> Self {
        Self { pool }
    }

    /// The layout of an imported Machine Pool.
    #[must_use]
    pub fn of(pool: &'pool MachinePool) -> Self {
        Self::new(pool.name())
    }

    /// The imported Pool's name.
    #[must_use]
    pub fn pool(&self) -> &'pool str {
        self.pool
    }

    /// `<pool>/ployz`, the parent of every writer root.
    #[must_use]
    pub fn root_parent(&self) -> String {
        format!("{}/{DATASET_ROOT}", self.pool)
    }

    /// `<pool>/ployz-mirror`, the read-only parent every slot inherits from.
    #[must_use]
    pub fn mirror_root(&self) -> String {
        format!("{}/{MIRROR_ROOT}", self.pool)
    }

    /// `<pool>/ployz/<name>`, the Volume's writer root.
    #[must_use]
    pub fn root(&self, name: &(impl Display + ?Sized)) -> String {
        format!("{}/{name}", self.root_parent())
    }

    /// `<pool>/ployz-mirror/<name>`, the slot parent that carries the commitment.
    #[must_use]
    pub fn slot_parent(&self, name: &(impl Display + ?Sized)) -> String {
        format!("{}/{name}", self.mirror_root())
    }

    /// `<pool>/ployz-mirror/<name>/fs`, the copy a receive or a cut lands.
    #[must_use]
    pub fn slot_fs(&self, name: &(impl Display + ?Sized)) -> String {
        format!("{}/{SLOT_FS}", self.slot_parent(name))
    }

    /// Where `dataset` sits under this Pool.
    #[must_use]
    pub fn place<'name>(&self, dataset: &'name str) -> Place<'name> {
        dataset
            .strip_prefix(self.pool)
            .and_then(|rest| rest.strip_prefix('/'))
            .map_or(Place::Outside, Place::under_pool)
    }
}

/// Where Docker mounts the writer root of `name`.
#[must_use]
pub fn root_mountpoint(name: &(impl Display + ?Sized)) -> String {
    format!("{MOUNT_ROOT}/{name}")
}

/// Where the received copy of the slot `name` mounts.
#[must_use]
pub fn slot_fs_mountpoint(name: &(impl Display + ?Sized)) -> String {
    format!("{MIRROR_MOUNT_ROOT}/{name}/{SLOT_FS}")
}

/// The Pool whose writer root parent `dataset` is, when `dataset` is `<pool>/ployz` under a
/// Pool that is itself a top-level dataset.
#[must_use]
pub fn pool_of_root_parent(dataset: &str) -> Option<&str> {
    dataset
        .strip_suffix(DATASET_ROOT)
        .and_then(|pool| pool.strip_suffix('/'))
        .filter(|pool| !pool.contains('/'))
}

/// What a dataset is to the Pool, from its path under the Pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Place<'name> {
    /// Under `<pool>/ployz/`: the writer root of the named Volume, or a descendant of it
    /// (the name then contains a `/`).
    Root(&'name str),
    /// Under `<pool>/ployz-mirror/`: the slot of the named Volume.
    Slot {
        name: &'name str,
        at: SlotAt,
    },
    Outside,
}

/// Which dataset of a slot a path is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotAt {
    /// `<pool>/ployz-mirror/<name>`
    Parent,
    /// `<pool>/ployz-mirror/<name>/fs`
    Fs,
    /// Anything else below the slot name.
    Below,
}

impl<'name> Place<'name> {
    /// Where `dataset` sits under whichever Pool it names, the first path segment.
    #[must_use]
    pub fn in_any_pool(dataset: &'name str) -> Self {
        dataset
            .split_once('/')
            .map_or(Self::Outside, |(_pool, rest)| Self::under_pool(rest))
    }

    fn under_pool(rest: &'name str) -> Self {
        if let Some(name) = rest
            .strip_prefix(DATASET_ROOT)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            return Self::Root(name);
        }
        let Some(slot) = rest
            .strip_prefix(MIRROR_ROOT)
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            return Self::Outside;
        };
        let name = slot.split('/').next().unwrap_or(slot);
        let at = match &slot[name.len()..] {
            "" => SlotAt::Parent,
            below if below.strip_prefix('/') == Some(SLOT_FS) => SlotAt::Fs,
            _ => SlotAt::Below,
        };
        Self::Slot { name, at }
    }

    /// The Volume a dataset belongs to, whichever copy of it this is.
    #[must_use]
    pub fn volume(self) -> Option<&'name str> {
        match self {
            Self::Root(name) | Self::Slot { name, .. } => Some(name),
            Self::Outside => None,
        }
    }

    /// The name of a Volume whose writer root this is, and not a descendant of it.
    #[must_use]
    pub fn writer_root(self) -> Option<&'name str> {
        match self {
            Self::Root(name) if !name.contains('/') => Some(name),
            Self::Root(_) | Self::Slot { .. } | Self::Outside => None,
        }
    }

    /// Whether the Pool's commitment counts this dataset's occupancy.
    #[must_use]
    pub fn commits(self) -> bool {
        self.volume().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_mountpoints_are_today_s_strings() {
        let layout = DatasetLayout::new("tank");
        assert_eq!(layout.root_parent(), "tank/ployz");
        assert_eq!(layout.mirror_root(), "tank/ployz-mirror");
        assert_eq!(layout.root("data"), "tank/ployz/data");
        assert_eq!(layout.slot_parent("data"), "tank/ployz-mirror/data");
        assert_eq!(layout.slot_fs("data"), "tank/ployz-mirror/data/fs");
        assert_eq!(root_mountpoint("data"), "/var/lib/ployz-volumes/data");
        assert_eq!(slot_fs_mountpoint("data"), "/var/lib/ployz-mirror/data/fs");
        assert_eq!(MOUNT_ROOT, "/var/lib/ployz-volumes");
        assert_eq!(MIRROR_MOUNT_ROOT, "/var/lib/ployz-mirror");
    }

    #[test]
    fn the_mirror_mount_root_is_the_mirror_dataset_name_under_var_lib() {
        assert_eq!(MIRROR_MOUNT_ROOT, format!("/var/lib/{MIRROR_ROOT}"));
        assert_eq!(
            slot_fs_mountpoint("data"),
            format!("/var/lib/{MIRROR_ROOT}/data/fs")
        );
    }

    #[test]
    fn a_pool_places_its_datasets() {
        let layout = DatasetLayout::new("tank");
        for (dataset, place) in [
            ("tank", Place::Outside),
            ("tank/ployz", Place::Outside),
            ("tank/ployz/data", Place::Root("data")),
            ("tank/ployz/data/child", Place::Root("data/child")),
            ("tank/ployz/", Place::Root("")),
            ("tank/ployz-mirror", Place::Outside),
            (
                "tank/ployz-mirror/data",
                Place::Slot {
                    name: "data",
                    at: SlotAt::Parent,
                },
            ),
            (
                "tank/ployz-mirror/data/fs",
                Place::Slot {
                    name: "data",
                    at: SlotAt::Fs,
                },
            ),
            (
                "tank/ployz-mirror/data/fs/deeper",
                Place::Slot {
                    name: "data",
                    at: SlotAt::Below,
                },
            ),
            (
                "tank/ployz-mirror/data/other",
                Place::Slot {
                    name: "data",
                    at: SlotAt::Below,
                },
            ),
            (
                "tank/ployz-mirror/",
                Place::Slot {
                    name: "",
                    at: SlotAt::Parent,
                },
            ),
            ("tank/other/data", Place::Outside),
            ("tankard/ployz/data", Place::Outside),
            ("other/ployz/data", Place::Outside),
        ] {
            assert_eq!(layout.place(dataset), place, "{dataset}");
        }
    }

    #[test]
    fn any_pool_reads_the_same_paths_under_whichever_pool_names_them() {
        for (dataset, place) in [
            ("tank", Place::Outside),
            ("tank/ployz", Place::Outside),
            ("other/ployz/data", Place::Root("data")),
            ("other/ployz/data/child", Place::Root("data/child")),
            (
                "other/ployz-mirror/data/fs",
                Place::Slot {
                    name: "data",
                    at: SlotAt::Fs,
                },
            ),
            ("other/ployz-mirror", Place::Outside),
            ("other/elsewhere/data", Place::Outside),
        ] {
            assert_eq!(Place::in_any_pool(dataset), place, "{dataset}");
        }
    }

    #[test]
    fn a_writer_root_is_a_root_without_descendant_segments() {
        assert_eq!(Place::Root("data").writer_root(), Some("data"));
        assert_eq!(Place::Root("data/child").writer_root(), None);
        assert_eq!(
            Place::Slot {
                name: "data",
                at: SlotAt::Parent
            }
            .writer_root(),
            None
        );
        assert_eq!(Place::Outside.writer_root(), None);
    }

    #[test]
    fn the_pool_of_a_root_parent_is_a_top_level_dataset() {
        assert_eq!(pool_of_root_parent("tank/ployz"), Some("tank"));
        assert_eq!(pool_of_root_parent("tank/sub/ployz"), None);
        assert_eq!(pool_of_root_parent("ployz"), None);
        assert_eq!(pool_of_root_parent("tank/ployz/data"), None);
    }
}
