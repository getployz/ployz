//! `ployz init --cloud`: enroll `initialize` or `join` on this Machine.

use std::time::Duration;

use clap::ArgMatches;
use ployz_core::{
    CloudEnrollToken, InitializeRequest, InspectRequest, JoinRequest, LocalMachinePhase,
    MachineName, MachineTokenRequest, ReserveDomainRequest, ResetRequest, op,
};

use super::{Error, connect_client, leaf_matches, required, runtime};
use crate::cloud_enroll::{self, EnrollIdentity, Outcome};

pub(super) fn run(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let token = CloudEnrollToken::parse(required(matches, "cloud")?)?;
    let cloud_url = matches
        .get_one::<String>("cloud-url")
        .expect("cloud-url has a default");
    let url = cloud_enroll::enroll_url(cloud_url, &token);
    let requested_name = matches
        .get_one::<String>("name")
        .map(MachineName::parse)
        .transpose()?;
    let cluster_network = matches
        .get_one::<String>("network")
        .expect("Cluster network has a default")
        .parse()
        .map_err(|error| Error::usage(format!("invalid Cluster network: {error}")))?;
    let wireguard_mtu = matches.get_one::<u32>("wg-mtu").copied();
    let yes = matches.get_flag("yes");
    let no_caddy = matches.get_flag("no-caddy");
    let no_dns = matches.get_flag("no-dns");

    runtime()?.block_on(async {
        let mut client = connect_client(matches, None).await?;
        let details = client
            .call::<op::Inspect>(InspectRequest::default(), None)
            .await?;
        if details.phase != LocalMachinePhase::Uninitialized {
            crate::handlers::machine::confirm(
                yes,
                "Reset the Machine before joining this Cluster?",
            )?;
            client.call::<op::Reset>(ResetRequest {}, None).await?;
            client = wait_phase(
                matches,
                LocalMachinePhase::Uninitialized,
                "Machine did not reset",
            )
            .await?;
        }
        let machine_token = client
            .call::<op::MachineToken>(MachineTokenRequest::default(), None)
            .await?;
        let name = crate::handlers::machine::machine_name(requested_name, &machine_token)?;
        let identity = EnrollIdentity::from_machine_token(name.clone(), &machine_token);
        match cloud_enroll::enroll(&url, &identity).await? {
            Outcome::Join(join) => {
                let assigned = join.registration.assigned_machine.clone();
                client
                    .call::<op::Join>(
                        JoinRequest {
                            registration: join.registration,
                            wireguard_mtu,
                            cloud_pairing: Some(join.pairing),
                        },
                        None,
                    )
                    .await?;
                wait_phase(
                    matches,
                    LocalMachinePhase::Participating,
                    "joined Machine did not become ready",
                )
                .await?;
                println!("Joined Machine {} ({})", assigned.name, assigned.id);
            }
            Outcome::Initialize { pairing } => {
                let machine = client
                    .call::<op::Initialize>(
                        InitializeRequest {
                            name,
                            cluster_network,
                            public_ip: machine_token.public_ip,
                            advertised_endpoints: machine_token.advertised_endpoints,
                            wireguard_mtu,
                            cloud_pairing: Some(pairing),
                        },
                        None,
                    )
                    .await?
                    .machine;
                let mut ready = wait_phase(
                    matches,
                    LocalMachinePhase::Participating,
                    "initial Machine did not become ready",
                )
                .await?;
                if let Err(error) = cloud_enroll::callback(
                    &cloud_enroll::callback_url(cloud_url, &token),
                    machine.id,
                )
                .await
                {
                    // List, not callback, is the lock.
                    eprintln!("{}", Error::warned("enroll callback failed", error));
                }
                if !no_dns {
                    let domain = ready
                        .call::<op::ReserveDomain>(
                            ReserveDomainRequest {
                                endpoint: cloud_enroll::dns_endpoint(cloud_url),
                            },
                            None,
                        )
                        .await?;
                    println!("Reserved Cluster domain: {}", domain.name);
                }
                if !no_caddy {
                    let image = crate::caddy::latest_image().await?;
                    let requested = crate::caddy::service_spec(image, Vec::new(), None);
                    crate::deploy::apply_requested(&mut ready, &requested).await?;
                    if !no_dns {
                        crate::dns::update_records_for_caddy(&mut ready).await?;
                    }
                }
                println!("Initialised Machine {} ({})", machine.name, machine.id);
            }
        }
        Ok(())
    })
}

async fn wait_phase(
    matches: &ArgMatches,
    phase: LocalMachinePhase,
    timeout_message: &str,
) -> Result<crate::connect::Client, Error> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(mut client) = connect_client(matches, None).await
                && client
                    .call::<op::Inspect>(InspectRequest::default(), None)
                    .await
                    .is_ok_and(|details| details.phase == phase)
            {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| Error::usage(timeout_message.to_owned()))
}
