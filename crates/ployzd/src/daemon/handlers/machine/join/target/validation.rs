use ipnet::Ipv4Net;
use ployz_model::{MachineMembership, StorageParticipation};

pub(super) fn validate_joined_machine_subnet(
    record: &MachineMembership,
    expected_subnet: Ipv4Net,
) -> Result<(), String> {
    match record.subnet {
        Some(subnet) if subnet == expected_subnet => Ok(()),
        Some(subnet) => Err(format!(
            "remote machine '{}' reported subnet '{}' but founder reserved '{}'",
            record.id, subnet, expected_subnet
        )),
        None => Err(format!(
            "remote machine '{}' reported no subnet but founder reserved '{}'",
            record.id, expected_subnet
        )),
    }
}

pub(super) fn validate_joined_machine_authority_posture(
    record: &MachineMembership,
) -> Result<(), String> {
    match &record.storage_participation() {
        StorageParticipation::Candidate => Ok(()),
        StorageParticipation::Authority { authority_id } => Err(format!(
            "remote machine '{}' reported authority storage for '{}' during machine add; use explicit storage promotion",
            record.id, authority_id
        )),
    }
}
