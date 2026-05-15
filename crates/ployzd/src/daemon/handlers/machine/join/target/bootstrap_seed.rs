use ipnet::Ipv4Net;
use ployz_api::MeshBootstrapRequest;
use ployz_model::{MachineMembership, MachineStorageRole, RegionRole, management_ip_from_key};
use ployz_orchestrator::mesh::wireguard::DEFAULT_LISTEN_PORT;
use ployz_store_api::MachineMembershipStore;

use super::super::super::types::MachineAddContext;
use super::super::remote::RemoteDaemonIdentity;

pub(super) fn build_bootstrap_membership_seed(
    target: &str,
    remote_identity: &RemoteDaemonIdentity,
) -> MachineMembership {
    let bootstrap_overlay_ip = management_ip_from_key(&remote_identity.public_key);
    let mut bootstrap_record = MachineMembership::seed(
        remote_identity.machine_id.clone(),
        remote_identity.public_key.clone(),
        bootstrap_overlay_ip,
        None,
        bootstrap_wireguard_endpoints(target),
    );
    bootstrap_record.storage_role = MachineStorageRole::Candidate;
    bootstrap_record.region_role = RegionRole::Compute;
    bootstrap_record.created_at = ployz_time::now_unix_secs();
    bootstrap_record.updated_at = bootstrap_record.created_at;
    bootstrap_record
}

pub(super) async fn publish_bootstrap_membership_seed(
    context: &MachineAddContext,
    record: &MachineMembership,
) -> Result<(), String> {
    if let Some(existing) =
        super::super::super::list::find_machine_record(&context.store, &record.id).await?
    {
        return Err(format!(
            "machine '{}' already exists with lifecycle '{}'",
            existing.id, existing.lifecycle
        ));
    }
    context
        .store
        .upsert_self_machine(record)
        .await
        .map_err(|err| format!("publish bootstrap membership seed: {err}"))
}

pub(super) async fn build_mesh_bootstrap_request(
    context: &MachineAddContext,
    assigned_subnet: Ipv4Net,
) -> Result<MeshBootstrapRequest, String> {
    let bootstrap_peers = context
        .store
        .list_machines()
        .await
        .map_err(|err| format!("list machines for bootstrap: {err}"))?
        .into_iter()
        .filter(|machine| !machine.endpoints.is_empty())
        .collect::<Vec<_>>();

    Ok(MeshBootstrapRequest {
        network_id: context.network_id.clone(),
        network_name: context.network_name.clone(),
        cluster_cidr: context.cluster_cidr.clone(),
        assigned_subnet,
        bootstrap_peers,
    })
}

fn bootstrap_wireguard_endpoints(target: &str) -> Vec<String> {
    let host = target
        .rsplit_once('@')
        .map_or(target, |(_, host)| host)
        .trim();
    let Some(host) = host.split_whitespace().next() else {
        return Vec::new();
    };
    if host.is_empty() {
        return Vec::new();
    }

    if let Some(stripped) = host.strip_prefix('[') {
        let Some((address, _rest)) = stripped.split_once(']') else {
            return Vec::new();
        };
        return vec![format!("[{address}]:{DEFAULT_LISTEN_PORT}")];
    }

    if host.contains(':') {
        return vec![format!("[{host}]:{DEFAULT_LISTEN_PORT}")];
    }

    vec![format!("{host}:{DEFAULT_LISTEN_PORT}")]
}

#[cfg(test)]
mod tests {
    use super::bootstrap_wireguard_endpoints;

    #[test]
    fn bootstrap_wireguard_endpoint_uses_ssh_target_host() {
        assert_eq!(
            bootstrap_wireguard_endpoints("root@192.168.227.3"),
            vec!["192.168.227.3:51820"]
        );
    }

    #[test]
    fn bootstrap_wireguard_endpoint_brackets_ipv6_hosts() {
        assert_eq!(
            bootstrap_wireguard_endpoints("root@fd00::12"),
            vec!["[fd00::12]:51820"]
        );
        assert_eq!(
            bootstrap_wireguard_endpoints("root@[fd00::12]"),
            vec!["[fd00::12]:51820"]
        );
    }
}
