//! What the daemon's planes share.

use crate::{
    corrosion::ClusterLink,
    docker::ContainerRuntime,
    machine::{LocalMachine, RecordOwner},
};

/// The Machine every serving plane is cloned from: Docker, the volume plugin and the
/// Cluster link applied once, so its clones share what a Machine keeps in memory, such as
/// the creations that settled.
pub(super) fn serving_local(
    owner: RecordOwner,
    containers: Option<ContainerRuntime>,
    plugin: crate::storage::Plugin,
    link: Option<&ClusterLink>,
) -> LocalMachine {
    LocalMachine::new(owner)
        .with_containers(containers)
        .with_plugin(plugin)
        .with_cluster_context(link.map(|link| link.context.clone()))
}

#[cfg(test)]
mod tests {
    use super::serving_local;
    use crate::{machine_api::MachineApi, test_dir::TestDir};

    #[tokio::test]
    async fn every_plane_serves_through_one_machine_that_keeps_settled_creations_once() {
        let root = TestDir::new("ployzd-serving-local");
        let owner = crate::machine::RecordOwner::spawn(
            crate::machine::LocalMachineStore::open(root.0.join("data")).unwrap(),
        )
        .unwrap();
        let serving = serving_local(owner.clone(), None, crate::storage::Plugin::default(), None);

        let management = serving.clone();
        let boot_pass = serving.clone();
        let machine_api = MachineApi::builder(owner)
            .with_local(serving.clone())
            .build();

        assert!(management.shares_settled_creations_with(&boot_pass));
        assert!(management.shares_settled_creations_with(&machine_api.local_machine()));
        assert!(serving.shares_settled_creations_with(&machine_api.local_machine()));
    }
}
