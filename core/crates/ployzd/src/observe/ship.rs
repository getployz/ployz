//! What leaves the Machine for Cloud: the crash stream and the requests stream, each queued
//! in an outbox on disk and pushed by one [`Shipper`].

mod crash;
mod push;
mod requests;
mod store;

#[cfg(test)]
pub(super) use crash::CAP;
pub(super) use crash::Outbox;
pub use push::{Shipper, push_target};
pub(super) use push::{Stream, unix_seconds};
pub(super) use requests::RequestOutbox;
#[cfg(test)]
pub(super) use requests::dir_has;

#[cfg(test)]
mod fixtures {
    use std::fs;

    use ployz_core::{
        ContainerId, MachineId, QualifiedService, ServiceId,
        metrics::{MarkerKind, MetricMarker},
    };

    use super::crash::Outbox;
    use crate::{observe::layout::StoreRoot, test_dir::TestDir};

    pub(super) fn store() -> (TestDir, StoreRoot) {
        let root = TestDir::new("ployzd-observe-ship");
        fs::create_dir_all(&root.0).unwrap();
        let store = StoreRoot::under(&root.0);
        store.prepare().unwrap();
        (root, store)
    }

    pub(super) fn crash(ts: i64) -> MetricMarker {
        MetricMarker {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            container_id: ContainerId::parse("b".repeat(64)).unwrap(),
            service_id: ServiceId::parse("c".repeat(32)).unwrap(),
            service: QualifiedService::parse("prod/web").unwrap(),
            deployment_id: Some("d1".into()),
            replica: "web-1".into(),
            ts,
            kind: MarkerKind::Exit {
                exit_code: Some(137),
                oom: true,
                stopped: false,
            },
        }
    }

    pub(super) fn held(outbox: &Outbox) -> Vec<u64> {
        outbox
            .oldest(super::CAP)
            .unwrap()
            .into_iter()
            .map(|entry| entry.seq)
            .collect()
    }
}
