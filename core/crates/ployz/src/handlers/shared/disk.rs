use std::fmt;

use ployz_core::{
    ByteCapacity, DOCKER_ROOT_NEARLY_FULL_BYTES, MachineId, MachineName, MachineObservation,
};

/// Bytes in the decimal GB storage limits are written in (MB under one GB); --json keeps the bytes.
pub(in crate::handlers) fn size(bytes: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "display rounding only")]
    let bytes = bytes as f64;
    if bytes >= 1e9 {
        format!("{:.1} GB", bytes / 1e9)
    } else {
        format!("{:.0} MB", bytes / 1e6)
    }
}

/// A Server with little space left under its Docker root.
pub(in crate::handlers) struct FullDisk<'a> {
    pub(in crate::handlers) machine: &'a MachineName,
    pub(in crate::handlers) free_bytes: u64,
}

impl FullDisk<'_> {
    /// What is wrong, said of the Server named beside it.
    pub(in crate::handlers) fn reason(&self) -> String {
        format!("Its {self:#}")
    }
}

impl fmt::Display for FullDisk<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !f.alternate() {
            write!(f, "Server {}'s ", self.machine)?;
        }
        write!(
            f,
            "disk is nearly full: {} free for Docker. Pulling an image onto it can fail until you free space.",
            size(self.free_bytes)
        )
    }
}

/// Each listed Server whose Docker root has under [`DOCKER_ROOT_NEARLY_FULL_BYTES`] free.
pub(in crate::handlers) fn full_disks<'a>(
    machines: &'a [MachineObservation],
    disks: &[(MachineId, ByteCapacity)],
) -> Vec<FullDisk<'a>> {
    machines
        .iter()
        .filter_map(|observed| {
            let (_, disk) = disks
                .iter()
                .find(|(machine, _)| *machine == observed.machine.id)?;
            (disk.available() < DOCKER_ROOT_NEARLY_FULL_BYTES).then_some(FullDisk {
                machine: &observed.machine.name,
                free_bytes: disk.available(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ployz_core::{MachineObservation, MembershipObservation};

    use super::full_disks;

    #[test]
    fn a_server_whose_docker_disk_is_nearly_full_is_named() {
        let [full, roomy, unread] = [
            crate::global_fixtures::machine('a', "machine-1"),
            crate::global_fixtures::machine('b', "machine-2"),
            crate::global_fixtures::machine('c', "machine-3"),
        ];
        let disks = [
            (
                full.id,
                ployz_core::ByteCapacity::parse(24_000_000_000, 30_000_000).unwrap(),
            ),
            (
                roomy.id,
                ployz_core::ByteCapacity::parse(24_000_000_000, 1_000_000_000).unwrap(),
            ),
        ];
        let machines = [full, roomy, unread]
            .map(|machine| MachineObservation::new(machine, MembershipObservation::Up));
        let named: Vec<String> = full_disks(&machines, &disks)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            named,
            [
                "Server machine-1's disk is nearly full: 30 MB free for Docker. \
              Pulling an image onto it can fail until you free space."
            ]
        );
    }

    #[test]
    fn the_boundary_is_one_billion_free_bytes() {
        let [edge, under] = [
            crate::global_fixtures::machine('a', "machine-1"),
            crate::global_fixtures::machine('b', "machine-2"),
        ];
        let disks = [
            (
                edge.id,
                ployz_core::ByteCapacity::parse(2_000_000_000, 1_000_000_000).unwrap(),
            ),
            (
                under.id,
                ployz_core::ByteCapacity::parse(2_000_000_000, 999_999_999).unwrap(),
            ),
        ];
        let machines = [edge, under]
            .map(|machine| MachineObservation::new(machine, MembershipObservation::Up));
        let named: Vec<&str> = full_disks(&machines, &disks)
            .iter()
            .map(|full| full.machine.as_str())
            .collect();
        assert_eq!(named, ["machine-2"]);
        assert_eq!(ployz_core::DOCKER_ROOT_NEARLY_FULL_BYTES, 1_000_000_000);
    }
}
