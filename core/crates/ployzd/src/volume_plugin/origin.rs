//! The origin each copy carries, and its two marks.
//!
//! - `ployz:origin` on a root or a slot's `fs`, set locally. A copy without it is Legacy:
//!   no value is ever written for Legacy.
//! - `ployz:pull-origin`, the record, on a slot parent beside `ployz:pull`. A pull writes
//!   it once the stream's header proved what the bytes are, and [`VolumeStorage::complete`]
//!   retires it once `fs` carries the same value. Destroying the slot drops it with the
//!   bytes it named.
//!
//! Every reader goes through [`VolumeStorage::origin_of`]. `None` is Legacy throughout.

use ployz_core::{SnapshotGuid, VolumeOrigin};

use super::{PluginError, Result, VolumeStorage, marks::Mark};

/// The origin a copy expects or carries; `None` is Legacy.
pub(super) type Origin = Option<VolumeOrigin>;

/// What one `zfs get` of a snapshot and its filesystem shows: the snapshot's GUID and the
/// origin it carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Point {
    pub(super) guid: SnapshotGuid,
    pub(super) origin: Origin,
}

impl Point {
    /// Reads `zfs get -Hp -o name,property,value,source guid,ployz:origin <fs>@<t> <fs>`.
    ///
    /// A born point's snapshot shows the value as `inherited from <fs>` and `fs` as
    /// `local`. ZFS shows a value `zfs receive -p` wrote as `received` on `fs` only, never
    /// on its snapshots, so the source is read off `fs`. Any other source refuses: the
    /// value was not stamped here.
    pub(super) fn parse(output: &str, fs: &str, snapshot: &str) -> Result<Self> {
        let invalid =
            || PluginError::from(format!("unexpected ZFS output for {snapshot}: {output:?}"));
        let mut guid = None;
        let mut on_snapshot = None;
        let mut on_fs = None;
        for line in output.lines() {
            let mut fields = line.split('\t');
            let (Some(name), Some(property), Some(value), Some(source), None) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                return Err(invalid());
            };
            let slot = match (name, property) {
                (name, "guid") if name == snapshot => &mut guid,
                (name, "guid") if name == fs => continue,
                (name, property) if name == snapshot && property == Mark::Origin.name() => {
                    &mut on_snapshot
                }
                (name, property) if name == fs && property == Mark::Origin.name() => &mut on_fs,
                _ => return Err(invalid()),
            };
            if slot.replace((value, source)).is_some() {
                return Err(invalid());
            }
        }
        let (Some((guid, _)), Some(on_snapshot), Some(on_fs)) = (guid, on_snapshot, on_fs) else {
            return Err(invalid());
        };
        let guid = guid.parse().map(SnapshotGuid::new).map_err(|_| invalid())?;
        let inherited = format!("inherited from {fs}");
        let origin = match (on_snapshot, on_fs) {
            (("-", "-"), ("-", "-")) => None,
            ((value, source), (stamped, "local")) if source == inherited && value == stamped => {
                Some(parse_origin(value, fs)?)
            }
            ((_, source), (_, fs_source)) => {
                return Err(PluginError::from(format!(
                    "{snapshot} carries an origin this Machine did not stamp ({source}; {fs} {fs_source})"
                )));
            }
        };
        Ok(Self { guid, origin })
    }
}

fn parse_origin(value: &str, dataset: &str) -> Result<VolumeOrigin> {
    VolumeOrigin::parse(value).map_err(|error| {
        PluginError::from(format!(
            "{dataset} carries an unreadable origin {value:?}: {error}"
        ))
    })
}

impl VolumeStorage {
    /// The origin `mark` holds on `dataset`, set locally, or `None` when it is unset.
    async fn local_origin(&self, dataset: &str, mark: Mark) -> Result<Origin> {
        let output = self
            .zfs(&["get", "-Hp", "-o", "value,source", mark.name(), dataset])
            .await?;
        match output.trim_end_matches('\n').split_once('\t') {
            Some(("-", "-")) => Ok(None),
            Some((value, "local")) => parse_origin(value, dataset).map(Some),
            _ => Err(PluginError::from(format!(
                "{dataset} carries {mark} that this Machine did not set: {output:?}"
            ))),
        }
    }

    /// The pending record on a slot parent.
    pub(super) async fn record(&self, parent: &str) -> Result<Origin> {
        self.local_origin(parent, Mark::PullOrigin).await
    }

    /// The origin of a copy: the slot's pending record if it has one, else what `dataset`
    /// carries. `parent` is the slot parent for a slot's copy, `None` for a root.
    /// `dataset` is `None` for a slot that has no `fs` yet.
    pub(super) async fn origin_of(
        &self,
        parent: Option<&str>,
        dataset: Option<&str>,
    ) -> Result<Origin> {
        if let Some(parent) = parent
            && let Some(record) = self.record(parent).await?
        {
            return Ok(Some(record));
        }
        match dataset {
            Some(dataset) => self.local_origin(dataset, Mark::Origin).await,
            None => Ok(None),
        }
    }

    /// Sets or clears the record to exactly `origin`.
    pub(super) async fn write_record(&self, parent: &str, origin: Origin) -> Result<()> {
        match origin {
            Some(origin) => {
                self.set_verified(parent, Mark::PullOrigin.name(), &origin.to_string())
                    .await
            }
            None => self.clear_record(parent).await,
        }
    }

    async fn clear_record(&self, parent: &str) -> Result<()> {
        self.zfs(&["inherit", Mark::PullOrigin.name(), parent])
            .await
            .map(drop)
    }

    /// Completion: stamps the record onto `fs`, reads it back, and only then retires the
    /// record. It never touches `ployz:pull`. A failure leaves the record for the next call,
    /// and a copy with no record keeps the origin it has.
    pub(super) async fn complete(&self, parent: &str, fs: &str) -> Result<()> {
        let Some(record) = self.record(parent).await? else {
            return Ok(());
        };
        self.set_verified(fs, Mark::Origin.name(), &record.to_string())
            .await?;
        ployzd::faults::kill_inside("Volume.Origin.Stamped", None);
        self.clear_record(parent).await
    }

    /// The snapshot `<fs>@<snapshot>` and the origin it carries, in one observation.
    pub(super) async fn point(&self, fs: &str, snapshot: &str) -> Result<Point> {
        let at = format!("{fs}@{snapshot}");
        let output = self
            .zfs(&[
                "get",
                "-Hp",
                "-o",
                "name,property,value,source",
                &format!("guid,{}", Mark::Origin.name()),
                &at,
                fs,
            ])
            .await?;
        Point::parse(&output, fs, &at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: &str = "tank/ployz/data";
    const AT: &str = "tank/ployz/data@w-1-2";
    const ORIGIN: &str = "81dabf1205b4218f99ea90cd1fef0a73ab0bbe0c0d3cb8e7d6fb5ed8bb0d1e07";

    fn output(snapshot: (&str, &str), fs: (&str, &str)) -> String {
        format!(
            "{AT}\tguid\t12\t-\n{AT}\tployz:origin\t{}\t{}\n{FS}\tguid\t7\t-\n{FS}\tployz:origin\t{}\t{}\n",
            snapshot.0, snapshot.1, fs.0, fs.1
        )
    }

    #[test]
    fn a_point_carries_only_an_origin_stamped_on_its_own_filesystem() {
        let inherited = format!("inherited from {FS}");
        let born = Point::parse(&output((ORIGIN, &inherited), (ORIGIN, "local")), FS, AT).unwrap();
        assert_eq!(born.guid, SnapshotGuid::new(12));
        assert_eq!(
            born.origin.map(|origin| origin.to_string()).as_deref(),
            Some(ORIGIN)
        );
        let legacy = Point::parse(&output(("-", "-"), ("-", "-")), FS, AT).unwrap();
        assert_eq!(legacy.origin, None);
        for refused in [
            // `zfs receive -p` wrote it: ZFS says so only on the filesystem.
            output((ORIGIN, &inherited), (ORIGIN, "received")),
            output(
                (ORIGIN, "inherited from tank/ployz"),
                (ORIGIN, "inherited from tank/ployz"),
            ),
            output((ORIGIN, "local"), ("-", "-")),
            output(("NOT-HEX", &inherited), ("NOT-HEX", "local")),
            output((ORIGIN, &inherited), ("-", "-")),
        ] {
            assert!(Point::parse(&refused, FS, AT).is_err(), "{refused}");
        }
    }
}
