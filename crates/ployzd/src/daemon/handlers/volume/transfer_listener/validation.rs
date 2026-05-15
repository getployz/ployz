use std::net::{IpAddr, SocketAddr};

use ployz_model::MachineId;
use ployz_spec::Namespace;
use ployz_store_api::{DeployStore, MachineMembershipStore, StoreDriver};
use ployz_volume_zfs::{
    VolumeAuthorizationRejection, ZfsReceiveRequest, ZfsTransferOpen, ZfsTransferValidationError,
};

pub(super) async fn validate_open_source(
    store: &StoreDriver,
    open: &ZfsTransferOpen,
    remote_addr: SocketAddr,
) -> Result<MachineId, ZfsTransferValidationError> {
    let Some(source) = open.source_machine_id.as_ref() else {
        return Err(ZfsTransferValidationError::MissingSourceMachineId);
    };
    validate_source_overlay(store, source, remote_addr).await?;
    if let Err(error) =
        validate_volume_ownership(store, source, &open.namespace, &open.volume).await
    {
        log_validation_context(&error);
        return Err(error);
    }
    Ok(source.clone())
}

pub(super) async fn authorized_receive_request(
    store: &StoreDriver,
    open: &ZfsTransferOpen,
    remote_addr: SocketAddr,
) -> Result<(MachineId, ZfsReceiveRequest), ZfsTransferValidationError> {
    let source = validate_open_source(store, open, remote_addr).await?;
    let namespace = Namespace::try_new(&open.namespace)
        .map_err(|message| ZfsTransferValidationError::InvalidNamespace { message })?;
    Ok((
        source,
        ZfsReceiveRequest::from_authorized_open(open, namespace),
    ))
}

fn log_validation_context(error: &ZfsTransferValidationError) {
    if let ZfsTransferValidationError::VolumeNotAuthorized {
        namespace,
        volume,
        source_machine_id,
        reason,
    } = error
    {
        match reason {
            VolumeAuthorizationRejection::OwnedByOtherMachine { owner } => tracing::warn!(
                namespace = %namespace.as_str(),
                volume,
                source = %source_machine_id,
                owner = %owner,
                "zfs transfer rejected: source is not the volume owner",
            ),
            VolumeAuthorizationRejection::VolumeNotFound => tracing::warn!(
                namespace = %namespace.as_str(),
                volume,
                source = %source_machine_id,
                "zfs transfer rejected: volume not found",
            ),
            VolumeAuthorizationRejection::LookupFailed { message } => tracing::warn!(
                error = %message,
                namespace = %namespace.as_str(),
                volume,
                "zfs transfer authorization lookup failed",
            ),
        }
    } else if let ZfsTransferValidationError::InvalidNamespace { message } = error {
        tracing::warn!(
            error = %message,
            "zfs transfer rejected: invalid namespace",
        );
    }
}

/// Verify the claimed source machine actually owns the namespace/volume the
/// header asks us to write into. Without this check, any active mesh peer can
/// pass IP validation and then redirect a `zfs recv` at an arbitrary dataset
/// under the configured root by picking different namespace/volume fields.
///
/// Returns a uniform "not authorized" message regardless of cause so that any
/// peer on the overlay cannot probe which namespaces or volumes exist on the
/// receiver. The detailed reason is logged locally for operators.
async fn validate_volume_ownership(
    store: &StoreDriver,
    source: &MachineId,
    namespace: &str,
    volume: &str,
) -> Result<(), ZfsTransferValidationError> {
    let namespace = Namespace::try_new(namespace)
        .map_err(|message| ZfsTransferValidationError::InvalidNamespace { message })?;
    match store.get_volume(&namespace, volume).await {
        Ok(Some(record)) if record.machine_id == *source => Ok(()),
        Ok(Some(record)) => Err(ZfsTransferValidationError::VolumeNotAuthorized {
            namespace,
            volume: volume.to_string(),
            source_machine_id: source.clone(),
            reason: VolumeAuthorizationRejection::OwnedByOtherMachine {
                owner: record.machine_id,
            },
        }),
        Ok(None) => Err(ZfsTransferValidationError::VolumeNotAuthorized {
            namespace,
            volume: volume.to_string(),
            source_machine_id: source.clone(),
            reason: VolumeAuthorizationRejection::VolumeNotFound,
        }),
        Err(error) => {
            let message = error.to_string();
            Err(ZfsTransferValidationError::VolumeNotAuthorized {
                namespace,
                volume: volume.to_string(),
                source_machine_id: source.clone(),
                reason: VolumeAuthorizationRejection::LookupFailed { message },
            })
        }
    }
}

async fn validate_source_overlay(
    store: &StoreDriver,
    source: &MachineId,
    remote_addr: SocketAddr,
) -> Result<(), ZfsTransferValidationError> {
    let machines = store.list_machines().await.map_err(|error| {
        ZfsTransferValidationError::SourceMachineLookupFailed {
            message: error.to_string(),
        }
    })?;
    let source_machine = machines
        .into_iter()
        .find(|machine| machine.id == *source)
        .ok_or_else(|| ZfsTransferValidationError::SourceMachineNotFound {
            machine_id: source.clone(),
        })?;
    let expected = IpAddr::V6(source_machine.overlay_ip.0);
    if remote_addr.ip() != expected {
        return Err(ZfsTransferValidationError::SourceOverlayIpMismatch {
            machine_id: source.clone(),
            actual: remote_addr.ip(),
            expected,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv6Addr, SocketAddr};

    use ployz_model::{
        DeployId, DeployRecord, DeployRecordState, MachineId, MachineLifecycle, MachineMembership,
        MachineTopology, OverlayIp, PublicKey, StorageParticipation, VolumeRecord,
    };
    use ployz_spec::{Namespace, VolumeScope};
    use ployz_store_api::{DeployCommit, DeployStore, MachineMembershipStore, StoreDriver};
    use ployz_store_memory::StoreDriverMemoryExt as _;

    use super::{
        ZfsTransferValidationError, validate_open_source, validate_source_overlay,
        validate_volume_ownership,
    };
    use ployz_volume_zfs::ZfsTransferOpen;

    fn open(snapshot: &str, expected_guid: u64) -> ZfsTransferOpen {
        ZfsTransferOpen {
            namespace: "default".into(),
            volume: "data".into(),
            snapshot: snapshot.into(),
            expected_guid,
            source_machine_id: Some(MachineId::new("source")),
            from_snapshot: None,
            from_snapshot_guid: None,
        }
    }

    fn machine(id: &str, overlay: Ipv6Addr) -> MachineMembership {
        MachineMembership {
            id: MachineId::new(id),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(overlay),
            topology: MachineTopology::local(),
            region_role: ployz_model::RegionRole::HomeData,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
            storage_role: StorageParticipation::default_authority().into(),
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    async fn store_with(machines: Vec<MachineMembership>) -> StoreDriver {
        let store = StoreDriver::memory();
        for m in machines {
            store.upsert_self_machine(&m).await.expect("upsert");
        }
        store
    }

    async fn insert_volume(store: &StoreDriver, namespace: &str, volume: &str, owner: &str) {
        let namespace = Namespace::new(namespace.to_string());
        let deploy_id = DeployId::new(format!("deploy-{volume}"));
        let record = VolumeRecord {
            namespace: namespace.clone(),
            volume_name: volume.to_string(),
            scope: VolumeScope::Single,
            machine_id: MachineId::new(owner.to_string()),
            quota: "1G".into(),
            mode: "rw".into(),
            owner: "0:0".into(),
            attached_services: Vec::new(),
            created_at: 0,
            created_by_deploy_id: deploy_id.clone(),
            last_modified_at: 0,
            last_modified_by_deploy_id: deploy_id.clone(),
        };
        let deploy = DeployRecord {
            deploy_id: deploy_id.clone(),
            namespace: namespace.clone(),
            coordinator_machine_id: MachineId::new(owner.to_string()),
            manifest_hash: "test".into(),
            state: DeployRecordState::Committed {
                committed_at: 0,
                finished_at: 0,
                summary_json: "{}".into(),
            },
            started_at: 0,
        };
        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                volume_branches: Vec::new(),
                phase_commits: Vec::new(),
                releases: Vec::new(),
                volumes: vec![record],
                deploy,
            })
            .await
            .expect("commit volume");
    }

    #[tokio::test]
    async fn validate_source_overlay_accepts_matching_ip() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", overlay)]).await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);
        validate_source_overlay(&store, &MachineId::new("source"), remote)
            .await
            .expect("matching ip accepted");
    }

    #[tokio::test]
    async fn validate_source_overlay_rejects_mismatched_ip() {
        let claimed = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let attacker = "fd00::2".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", claimed)]).await;
        let remote = SocketAddr::new(IpAddr::V6(attacker), 4319);
        let err = validate_source_overlay(&store, &MachineId::new("source"), remote)
            .await
            .expect_err("mismatched ip rejected");
        assert!(matches!(
            err,
            ZfsTransferValidationError::SourceOverlayIpMismatch { .. }
        ));
    }

    #[tokio::test]
    async fn validate_source_overlay_rejects_unknown_machine() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("known", overlay)]).await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);
        let err = validate_source_overlay(&store, &MachineId::new("ghost"), remote)
            .await
            .expect_err("unknown machine rejected");
        assert!(matches!(
            err,
            ZfsTransferValidationError::SourceMachineNotFound { .. }
        ));
    }

    #[tokio::test]
    async fn validate_open_source_rejects_missing_source_machine_id() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", overlay)]).await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);
        let open = ZfsTransferOpen {
            namespace: "default".into(),
            volume: "data".into(),
            snapshot: "snap".into(),
            expected_guid: 1,
            source_machine_id: None,
            from_snapshot: None,
            from_snapshot_guid: None,
        };

        let err = validate_open_source(&store, &open, remote)
            .await
            .expect_err("missing source rejected");
        assert!(matches!(
            err,
            ZfsTransferValidationError::MissingSourceMachineId
        ));
    }

    #[tokio::test]
    async fn validate_open_source_accepts_when_source_owns_volume() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", overlay)]).await;
        insert_volume(&store, "default", "data", "source").await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);

        validate_open_source(&store, &open("snap", 1), remote)
            .await
            .expect("owner accepted");
    }

    #[tokio::test]
    async fn validate_open_source_rejects_when_volume_owned_by_other_machine() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let other = "fd00::2".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", overlay), machine("owner", other)]).await;
        insert_volume(&store, "default", "data", "owner").await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);

        let err = validate_open_source(&store, &open("snap", 1), remote)
            .await
            .expect_err("non-owner rejected");
        let rendered = err.to_string();
        assert!(matches!(
            err,
            ZfsTransferValidationError::VolumeNotAuthorized { .. }
        ));
        assert!(
            rendered.contains("not authorized"),
            "unexpected error: {rendered}"
        );
        assert!(
            !rendered.contains("owner"),
            "ownership detail leaked: {rendered}"
        );
    }

    #[tokio::test]
    async fn validate_volume_ownership_rejects_unknown_volume() {
        let store = StoreDriver::memory();
        let err =
            validate_volume_ownership(&store, &MachineId::new("source"), "default", "missing")
                .await
                .expect_err("unknown volume rejected");
        let rendered = err.to_string();
        assert!(matches!(
            err,
            ZfsTransferValidationError::VolumeNotAuthorized { .. }
        ));
        assert!(
            rendered.contains("not authorized"),
            "unexpected error: {rendered}"
        );
        assert!(
            !rendered.contains("missing"),
            "volume name leaked: {rendered}"
        );
    }

    #[tokio::test]
    async fn validate_volume_ownership_rejects_invalid_namespace() {
        let store = StoreDriver::memory();
        let err = validate_volume_ownership(&store, &MachineId::new("source"), "Prod", "data")
            .await
            .expect_err("invalid namespace rejected");
        let rendered = err.to_string();
        assert!(matches!(
            err,
            ZfsTransferValidationError::InvalidNamespace { .. }
        ));
        assert!(
            rendered.contains("not authorized"),
            "unexpected error: {rendered}"
        );
        assert!(
            !rendered.contains("Prod"),
            "namespace detail leaked: {rendered}"
        );
    }
}
