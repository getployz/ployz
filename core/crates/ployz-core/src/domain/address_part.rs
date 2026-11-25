//! Each Machine Subnet is split into parts by the last octet, so a Server's firewall can
//! tell a Container's standing from its address alone, on this Server or across the mesh.
//! The split is a frozen format: every Server fences peers' Containers by it.

use std::ops::RangeInclusive;

use crate::{Admission, PreviewNamespace, QualifiedService, RawVolumeSource, ServiceVolumeGraph};

/// Where in a Machine Subnet a Container's address lies.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum AddressPart {
    /// Signed workloads with no data Volume mounted, and the reserved infrastructure
    /// Containers whatever they mount.
    Open,
    /// Signed workloads in a preview Namespace, whatever they mount.
    Preview,
    /// Signed workloads, hooks included, with a data Volume mounted.
    SignedData,
    /// Everything else: unsigned workloads, and any Container with no readable admission.
    Free,
}

impl AddressPart {
    /// Every part, in address order.
    pub const ALL: [Self; 4] = [Self::Open, Self::Preview, Self::SignedData, Self::Free];

    /// The last octets of the part's addresses in every Machine Subnet.
    #[must_use]
    pub const fn hosts(self) -> RangeInclusive<u8> {
        match self {
            Self::Open => 2..=127,
            Self::Preview => 128..=159,
            Self::SignedData => 160..=191,
            Self::Free => 192..=254,
        }
    }

    /// The last octet's value and mask every address in the part matches, with the
    /// network address and gateway in Open and the broadcast address in Free.
    #[must_use]
    pub const fn last_octet_mask(self) -> (u8, u8) {
        match self {
            Self::Open => (0, 128),
            Self::Preview => (128, 224),
            Self::SignedData => (160, 224),
            Self::Free => (192, 192),
        }
    }

    /// The part `last_octet` lies in, the network address and gateway in Open and the
    /// broadcast address in Free.
    #[must_use]
    pub fn of_last_octet(last_octet: u8) -> Self {
        Self::ALL
            .into_iter()
            .find(|part| {
                let (value, mask) = part.last_octet_mask();
                last_octet & mask == value
            })
            .expect("the parts' masks cover every octet")
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::SignedData => "signed data",
            Self::Preview => "preview",
            Self::Free => "free",
        }
    }
}

impl std::fmt::Display for AddressPart {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The part a Container takes a new address in. `identity` is the Service as this Server
/// verified it at admission, `admission` what it kept, `None` when it kept nothing
/// readable, and `volumes` the Container's own resolved mounts.
#[must_use]
pub fn part_of(
    identity: &QualifiedService,
    admission: Option<Admission>,
    volumes: &ServiceVolumeGraph,
) -> AddressPart {
    match admission {
        None | Some(Admission::Free) => AddressPart::Free,
        Some(Admission::Signed) if identity.reserved().is_some() => AddressPart::Open,
        Some(Admission::Signed) if PreviewNamespace::of(&identity.namespace).is_some() => {
            AddressPart::Preview
        }
        Some(Admission::Signed) => {
            let holds_data = volumes
                .mounted_volumes()
                .any(|volume| !matches!(volume.source.kind(), RawVolumeSource::Tmpfs { .. }));
            if holds_data {
                AddressPart::SignedData
            } else {
                AddressPart::Open
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContainerPath, Namespace, ServiceMount, ServiceName, ServiceVolume, ServiceVolumeReference,
    };

    /// A graph declaring an ordinary Volume and a tmpfs, mounting those `mounted` names.
    fn volumes(mounted: &[&str]) -> ServiceVolumeGraph {
        let volume = |reference: &str, source| ServiceVolume {
            reference: ServiceVolumeReference::parse(reference).unwrap(),
            source: serde_json::from_value(source).unwrap(),
        };
        ServiceVolumeGraph::parse(
            vec![
                volume(
                    "data",
                    serde_json::json!({
                        "kind": "ordinary", "name": "data", "driver": { "name": "local" },
                    }),
                ),
                volume("scratch", serde_json::json!({ "kind": "tmpfs" })),
            ],
            mounted
                .iter()
                .map(|reference| ServiceMount {
                    volume: ServiceVolumeReference::parse(*reference).unwrap(),
                    target: ContainerPath::parse(format!("/{reference}")).unwrap(),
                    read_only: false,
                    no_copy: false,
                    subpath: None,
                })
                .collect(),
        )
        .unwrap()
    }

    fn service(namespace: &str, name: &str) -> QualifiedService {
        QualifiedService::new(
            Namespace::parse(namespace).unwrap(),
            ServiceName::parse(name).unwrap(),
        )
    }

    #[test]
    fn only_a_signed_workload_mounting_data_takes_a_signed_data_address() {
        let app = service("shop-production", "postgres");
        let signed = Some(Admission::Signed);
        assert_eq!(
            part_of(&app, signed, &volumes(&["data"])),
            AddressPart::SignedData
        );
        assert_eq!(
            part_of(&app, signed, &volumes(&["scratch"])),
            AddressPart::Open
        );
        assert_eq!(part_of(&app, signed, &volumes(&[])), AddressPart::Open);
        for unsigned in [None, Some(Admission::Free)] {
            assert_eq!(
                part_of(&app, unsigned, &volumes(&["data"])),
                AddressPart::Free
            );
        }
    }

    #[test]
    fn a_signed_preview_container_takes_a_preview_address_whatever_it_mounts() {
        let home = crate::PreviewHome::of(&crate::MachineId::parse("a".repeat(32)).unwrap());
        let preview = service(&format!("pv-0123456789abcdef-{home}-00c0ffee"), "postgres");
        for mounted in [&["data"][..], &[]] {
            assert_eq!(
                part_of(&preview, Some(Admission::Signed), &volumes(mounted)),
                AddressPart::Preview
            );
        }
        assert_eq!(
            part_of(&preview, Some(Admission::Free), &volumes(&[])),
            AddressPart::Free
        );
        let lookalike = service("pv-production", "postgres");
        assert_eq!(
            part_of(&lookalike, Some(Admission::Signed), &volumes(&["data"])),
            AddressPart::SignedData
        );
    }

    #[test]
    fn reserved_infrastructure_is_open_whatever_it_mounts_and_a_system_name_alone_is_not() {
        let holder = QualifiedService::system_ingress_holder();
        assert_eq!(
            part_of(&holder, Some(Admission::Signed), &volumes(&["data"])),
            AddressPart::Open
        );
        assert_eq!(part_of(&holder, None, &volumes(&[])), AddressPart::Free);
        let squatter = service(Namespace::SYSTEM, "ingress-cache");
        assert_eq!(
            part_of(&squatter, Some(Admission::Free), &volumes(&[])),
            AddressPart::Free
        );
    }

    #[test]
    fn each_part_is_its_mask_and_the_parts_tile_the_subnet() {
        let mut last = 1;
        for part in AddressPart::ALL {
            assert_eq!(*part.hosts().start(), last + 1, "{part}");
            last = *part.hosts().end();
            let (value, mask) = part.last_octet_mask();
            for octet in 0..=u8::MAX {
                let outside_hosts = match octet {
                    0 | 1 => part == AddressPart::Open,
                    u8::MAX => part == AddressPart::Free,
                    _ => false,
                };
                let expected = part.hosts().contains(&octet) || outside_hosts;
                assert_eq!(octet & mask == value, expected, "{part} {octet}");
                assert_eq!(
                    AddressPart::of_last_octet(octet) == part,
                    expected,
                    "{part} {octet}"
                );
            }
        }
        assert_eq!(last, 254);
    }
}
