use clap::ArgMatches;
use ployz_core::{
    InspectRequest, JoinRequest, LocalMachinePhase, Machine, MachineName, MachineObservation,
    RegisterRequest, ResetRequest, WireGuardPublicKey, op,
};

use super::super::{connect_client, runtime};
use super::{ConnectionOptions, helpers, machine_list, target};
use crate::handlers::{Error, leaf_matches};

pub(in crate::handlers) fn add(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let deploy_caddy = !matches.get_flag("no-caddy");
    let options = ConnectionOptions::from_matches(root)?;
    let (mut config, context_name) = options.active_config()?;
    let destination = target(matches, "destination")?;
    let connection = destination.parse()?;
    let mut connection = helpers::configure_ssh_key(
        connection,
        matches.get_one::<String>("ssh-key").map(String::as_str),
    )?;
    let requested_name = matches
        .get_one::<String>("name")
        .map(MachineName::parse)
        .transpose()?;
    let token_request = helpers::token_request(matches)?;
    let wireguard_mtu = matches.get_one::<u32>("wg-mtu").copied();
    let yes = matches.get_flag("yes");
    let storage = crate::provisioning::resolve_storage(matches)?;
    if !matches.get_flag("no-install") {
        crate::provisioning::provision(matches, storage)?;
    }

    let assigned = runtime()?.block_on(async {
        let mut entry = connect_client(matches, options.context()).await?;
        let visible = machine_list(&mut entry).await?;
        let mut target_client = helpers::connect_direct(&connection).await?;
        let mut token = target_client
            .call::<op::MachineToken>(token_request.clone(), None)
            .await?;
        let details = target_client
            .call::<op::Inspect>(
                InspectRequest {
                    advertised_endpoints: token.advertised_endpoints.clone(),
                    ..Default::default()
                },
                None,
            )
            .await?;
        if details.phase != LocalMachinePhase::Uninitialized {
            cluster_membership_conflict(&details.phase, &visible, &token.public_key)?;
            helpers::confirm(yes, "Reset the Machine before adding it to this Cluster?")?;
            target_client
                .call::<op::Reset>(ResetRequest {}, None)
                .await?;
            target_client = helpers::reconnect_direct(&connection).await?;
            token = target_client
                .call::<op::MachineToken>(token_request, None)
                .await?;
        }
        let name = helpers::machine_name(requested_name, &token)?;

        // TODO(UT-140): registration is intentionally unfenced and may succeed on a minority.
        let registration = entry
            .call::<op::Register>(
                RegisterRequest {
                    name,
                    storage,
                    public_key: token.public_key,
                    public_ip: token.public_ip,
                    advertised_endpoints: token.advertised_endpoints,
                    runtime: token.runtime,
                },
                None,
            )
            .await?;
        let assigned = registration.assigned_machine.clone();
        target_client
            .call::<op::Join>(
                JoinRequest {
                    registration,
                    wireguard_mtu,
                    cloud_pairing: None,
                },
                None,
            )
            .await?;

        Ok::<_, Error>(assigned)
    })?;

    connection = connection.with_machine_id(assigned.id);
    config
        .contexts
        .get_mut(&context_name)
        .expect("active context was validated")
        .connections
        .push(connection.clone());
    config.save()?;
    println!("{}", added_machine_line(&assigned));

    runtime()?.block_on(helpers::wait_direct_participating(
        &connection,
        "added Machine did not become ready",
    ))?;

    let catch_up = runtime()?.block_on(async {
        let mut entry = connect_client(matches, options.context()).await?;
        Ok::<_, Error>(
            crate::global_catch_up::catch_up_globals(&mut entry, &assigned, !deploy_caddy).await,
        )
    })?;
    if let Err(error) = catch_up {
        return Err(Error::usage(crate::global_catch_up::joined_catch_up_error(
            error,
        )));
    }
    let dns_result = runtime()?.block_on(async {
        let mut entry = connect_client(matches, options.context()).await?;
        crate::dns::update_records_if_reserved(&mut entry).await?;
        Ok::<_, Error>(())
    });
    if let Err(error) = dns_result {
        eprintln!(
            "{}",
            Error::warned("hosted DNS refresh failed after adding the Machine", error)
        );
    }
    Ok(())
}

fn cluster_membership_conflict(
    phase: &LocalMachinePhase,
    visible: &[MachineObservation],
    public_key: &WireGuardPublicKey,
) -> Result<(), Error> {
    if *phase != LocalMachinePhase::Uninitialized
        && visible
            .iter()
            .any(|observation| observation.machine.public_key == *public_key)
    {
        Err(Error::usage("Machine already belongs to this Cluster"))
    } else {
        Ok(())
    }
}

fn added_machine_line(assigned: &Machine) -> String {
    format!("Added Machine {} ({})", assigned.name, assigned.id)
}

#[cfg(test)]
mod tests {
    use ployz_core::DOCKER_NETWORK_CONFLICT_RECOVERY;
    use ployz_core::{
        LocalMachinePhase, Machine, MachineId, MachineName, MachineObservation, ManagementAddress,
        MembershipObservation, WireGuardPublicKey,
    };

    use super::*;

    #[test]
    fn add_timeout_surfaces_the_docker_network_recovery() {
        let message = helpers::readiness_timeout_message("added Machine did not become ready");

        assert!(message.contains("added Machine did not become ready"));
        assert!(message.contains(DOCKER_NETWORK_CONFLICT_RECOVERY));
    }

    #[test]
    fn machine_add_reports_added_when_follow_on_caddy_deploy_fails() {
        let assigned = assigned_machine("edge", 'a');
        assert_eq!(
            added_machine_line(&assigned),
            "Added Machine edge (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)"
        );
    }

    #[test]
    fn catch_up_failure_after_add_reports_joined_membership() {
        let error = crate::global_catch_up::joined_catch_up_error(
            crate::global_catch_up::CatchUpError::new(
                crate::failure::Failure::usage("deploy timed out".to_owned()),
                vec![ployz_core::QualifiedService::system_caddy()],
            ),
        );
        assert!(error.contains("Machine joined"));
        assert!(error.contains("remains a Cluster member"));
        assert!(
            error.contains("`ployz caddy deploy`"),
            "failure must tell the operator to run `caddy deploy`, got {error:?}"
        );
        assert!(
            error.contains("deploy timed out"),
            "failure must include the follow-on error, got {error:?}"
        );
        assert_eq!(
            error.matches("deploy timed out").count(),
            1,
            "failure must report the error once, got {error:?}"
        );
    }

    #[test]
    fn re_adding_a_joined_machine_reports_it_already_belongs_to_the_cluster() {
        let assigned = assigned_machine("edge", 'a');
        let visible = [MachineObservation {
            machine: assigned.clone(),
            membership: MembershipObservation::Up,
            storage: None,
            selected_endpoint: None,
            rtt: None,
        }];
        assert_eq!(
            cluster_membership_conflict(
                &LocalMachinePhase::Participating,
                &visible,
                &assigned.public_key,
            )
            .unwrap_err()
            .to_string(),
            "Machine already belongs to this Cluster"
        );
        assert!(
            cluster_membership_conflict(
                &LocalMachinePhase::Uninitialized,
                &visible,
                &assigned.public_key,
            )
            .is_ok()
        );
        let other = WireGuardPublicKey([9; 32]);
        assert!(
            cluster_membership_conflict(&LocalMachinePhase::Participating, &visible, &other,)
                .is_ok()
        );
    }

    fn assigned_machine(name: &str, seed: char) -> Machine {
        Machine {
            id: MachineId::parse(seed.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: "10.210.1.0/24".parse().unwrap(),
            management_address: ManagementAddress("::1".parse().unwrap()),
            public_key: WireGuardPublicKey([seed as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
        }
    }
}
