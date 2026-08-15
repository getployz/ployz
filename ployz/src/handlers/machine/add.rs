use std::time::Duration;

use clap::ArgMatches;
use ployz_core::{
    InspectRequest, JoinRequest, LocalMachinePhase, Machine, MachineId, MachineName,
    MachineObservation, MembershipObservation, RegisterRequest, ResetRequest, WireGuardPublicKey,
    op,
};

use super::super::{connect_client, runtime};
use super::{ConnectionOptions, helpers, machine_list, target};
use crate::{
    connect::Client,
    handlers::{Error, leaf_matches},
};

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
    if !matches.get_flag("no-install") {
        crate::provisioning::provision(matches).map_err(Error::usage)?;
    }

    let (assigned, caddy_settings) = runtime()?.block_on(async {
        let mut entry = connect_client(matches, options.context()).await?;
        let visible = machine_list(&mut entry).await?;
        let caddy_settings = if deploy_caddy {
            let live = entry.live_services().await?;
            crate::caddy::newest_existing_settings(
                &live
                    .containers
                    .successes
                    .into_iter()
                    .flat_map(|success| success.value)
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
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
                },
                None,
            )
            .await?;

        Ok::<_, Error>((assigned, caddy_settings))
    })?;

    connection = connection.with_machine_id(assigned.id.clone());
    config
        .contexts
        .get_mut(&context_name)
        .expect("active context was validated")
        .connections
        .push(connection);
    config.save()?;
    println!("{}", added_machine_line(&assigned));

    let caddy = if let Some((image, machines, caddy_config)) = caddy_settings {
        Some(
            runtime()?
                .block_on(async {
                    let mut entry = connect_client(matches, options.context()).await?;
                    wait_machine_up(&mut entry, &assigned.id).await?;
                    // TODO(UT-050): preserve upstream's bounded redeploy instead of a dedicated scale.
                    let requested = crate::caddy::service_spec(image, machines, caddy_config);
                    crate::deploy::apply_requested(&mut entry, &requested).await
                })
                .map_err(|error| error.to_string()),
        )
    } else {
        None
    };
    let outcome = MachineAddOutcome::after_saved(caddy);
    if let Some(warning) = &outcome.caddy_warning {
        eprintln!("{warning}");
    }
    if outcome.refresh_hosted_dns
        && let Err(error) = runtime()?.block_on(async {
            let mut entry = connect_client(matches, options.context()).await?;
            crate::dns::update_records_if_reserved(&mut entry).await?;
            Ok::<_, Error>(())
        })
    {
        eprintln!("{}", caddy_follow_on_warning(&error.to_string()));
        return Err(error);
    }
    match outcome.follow_on_error {
        Some(error) => Err(Error::usage(error)),
        None => Ok(()),
    }
}

async fn wait_machine_up(entry: &mut Client, machine_id: &MachineId) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if machine_list(entry).await.is_ok_and(|machines| {
                machines.iter().any(|observation| {
                    observation.machine.id == *machine_id
                        && observation.membership == MembershipObservation::Up
                })
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| Error::usage("new Machine did not become available for Caddy deployment"))
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

struct MachineAddOutcome {
    caddy_warning: Option<String>,
    refresh_hosted_dns: bool,
    follow_on_error: Option<String>,
}

impl MachineAddOutcome {
    fn after_saved(caddy: Option<Result<(), String>>) -> Self {
        match caddy {
            Some(Ok(())) => Self {
                caddy_warning: None,
                refresh_hosted_dns: true,
                follow_on_error: None,
            },
            Some(Err(error)) => Self {
                caddy_warning: Some(caddy_follow_on_warning(&error)),
                refresh_hosted_dns: false,
                follow_on_error: Some(error),
            },
            None => Self {
                caddy_warning: None,
                refresh_hosted_dns: false,
                follow_on_error: None,
            },
        }
    }
}

fn added_machine_line(assigned: &Machine) -> String {
    format!("Added Machine {} ({})", assigned.name, assigned.id)
}

fn caddy_follow_on_warning(error: &str) -> String {
    format!(
        "WARNING: Caddy Deploy failed after adding the Machine: {error}. Run `caddy deploy` to retry."
    )
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        LocalMachinePhase, Machine, MachineId, MachineName, MachineObservation, MachineSubnet,
        ManagementAddress, MembershipObservation, WireGuardPublicKey,
    };

    use super::*;

    #[test]
    fn machine_add_reports_added_when_follow_on_caddy_deploy_fails() {
        let assigned = assigned_machine("edge", 'a');
        let outcome = MachineAddOutcome::after_saved(Some(Err("deploy timed out".into())));
        assert_eq!(
            added_machine_line(&assigned),
            "Added Machine edge (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)"
        );
        assert!(outcome.follow_on_error.is_some());
    }

    #[test]
    fn caddy_deploy_failure_after_add_warns_operator_to_run_caddy_deploy() {
        let outcome = MachineAddOutcome::after_saved(Some(Err("deploy timed out".into())));
        let warning = outcome
            .caddy_warning
            .expect("Caddy Deploy failure must warn the operator");
        assert!(
            warning.contains("`caddy deploy`"),
            "warning must tell the operator to run `caddy deploy`, got {warning:?}"
        );
        assert!(
            warning.contains("deploy timed out"),
            "warning must include the follow-on failure, got {warning:?}"
        );
    }

    #[test]
    fn successful_add_caddy_deploy_refreshes_hosted_dns() {
        let assigned = assigned_machine("edge", 'a');
        let outcome = MachineAddOutcome::after_saved(Some(Ok(())));
        assert!(
            outcome.refresh_hosted_dns,
            "successful Caddy Deploy on add must refresh hosted DNS the same way caddy deploy does"
        );
        assert!(outcome.caddy_warning.is_none());
        assert!(outcome.follow_on_error.is_none());
        assert_eq!(
            added_machine_line(&assigned),
            "Added Machine edge (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)"
        );

        let skipped = MachineAddOutcome::after_saved(None);
        assert!(!skipped.refresh_hosted_dns);
        let failed = MachineAddOutcome::after_saved(Some(Err("boom".into())));
        assert!(!failed.refresh_hosted_dns);
    }

    #[test]
    fn re_adding_a_joined_machine_reports_it_already_belongs_to_the_cluster() {
        let assigned = assigned_machine("edge", 'a');
        let visible = [MachineObservation {
            machine: assigned.clone(),
            membership: MembershipObservation::Up,
            selected_endpoint: None,
        }];
        assert_eq!(
            cluster_membership_conflict(
                &LocalMachinePhase::Participating,
                &visible,
                &assigned.public_key,
            ),
            Err(Error::usage("Machine already belongs to this Cluster"))
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
            subnet: MachineSubnet("10.210.1.0/24".parse().unwrap()),
            management_address: ManagementAddress("::1".parse().unwrap()),
            public_key: WireGuardPublicKey([seed as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
        }
    }
}
