//! `ployz init --cloud`: enroll `join` on this Machine.

use std::time::Duration;

use clap::ArgMatches;
use ployz_core::{
    CloudEnrollToken, InspectRequest, JoinRequest, LocalMachinePhase, MachineName,
    MachineTokenRequest, ResetRequest, op,
};

use super::{Error, connect_client, leaf_matches, required, runtime};
use crate::cloud_enroll::{self, EnrollIdentity};

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
    let wireguard_mtu = matches.get_one::<u32>("wg-mtu").copied();
    let yes = matches.get_flag("yes");

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
        let join = cloud_enroll::enroll_join(
            &url,
            &EnrollIdentity::from_machine_token(name, &machine_token),
        )
        .await?;
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
