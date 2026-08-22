//! `ployz cloud enroll`: enroll `initialize` or `join` on this Machine.

use std::time::Duration;

use clap::ArgMatches;
use ployz_core::{
    CloudEnrollToken, InitializeRequest, InspectRequest, JoinRequest, LocalMachinePhase,
    MachineName, MachineTokenRequest, ReserveDomainRequest, ResetRequest, RpcErrorCode,
    SetCloudPairingRequest, op,
};

use super::{Error, config_path, connect_client, leaf_matches, required, runtime};
use crate::cloud_enroll::{self, EnrollIdentity, Outcome};
use crate::connect::{Client, ConnectError};
use crate::context::ContextError;

pub(super) fn enroll(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let token = CloudEnrollToken::parse(required(matches, "token")?)?;
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
    let reset = matches.get_flag("reset");
    let no_caddy = matches.get_flag("no-caddy");
    let no_dns = matches.get_flag("no-dns");

    runtime()?.block_on(async {
        let mut client = connect_machine(matches).await?;
        loop {
            client = ensure_uninitialized(matches, yes, reset, client).await?;
            let machine_token = client
                .call::<op::MachineToken>(MachineTokenRequest::default(), None)
                .await?;
            let name =
                crate::handlers::machine::machine_name(requested_name.clone(), &machine_token)?;
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
                    let mut ready = wait_phase(
                        matches,
                        LocalMachinePhase::Participating,
                        "joined Machine did not become ready",
                    )
                    .await?;
                    if let Err(error) =
                        crate::global_catch_up::catch_up_globals(&mut ready, &assigned, no_caddy)
                            .await
                    {
                        return Err(Error::usage(crate::global_catch_up::joined_catch_up_error(
                            error,
                        )));
                    }
                    println!("Joined Machine {} ({})", assigned.name, assigned.id);
                    return Ok(());
                }
                Outcome::Initialize { pairing } => {
                    let initialized = match client
                        .call::<op::Initialize>(
                            InitializeRequest {
                                name,
                                cluster_network,
                                public_ip: machine_token.public_ip,
                                advertised_endpoints: machine_token.advertised_endpoints,
                                wireguard_mtu,
                                cloud_pairing: no_caddy.then(|| pairing.clone()),
                            },
                            None,
                        )
                        .await
                    {
                        Ok(initialized) => initialized,
                        Err(error) if pairing_revoked(&error) => continue,
                        Err(error) => return Err(error.into()),
                    };
                    let machine = initialized.machine;
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
                                    endpoint: crate::dns::HOSTED_DNS_ENDPOINT.to_owned(),
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
                        ready
                            .call::<op::SetCloudPairing>(
                                SetCloudPairingRequest {
                                    cloud_pairing: Some(pairing),
                                },
                                None,
                            )
                            .await?;
                    }
                    println!("Initialised Machine {} ({})", machine.name, machine.id);
                    return Ok(());
                }
            }
        }
    })
}

async fn connect_machine(matches: &ArgMatches) -> Result<Client, Error> {
    let config = config_path(matches)?;
    let connect = matches.get_one::<String>("connect").map(String::as_str);
    match crate::connect::connect(&config, connect, None).await {
        Ok(client) => Ok(client),
        Err(ConnectError::Context(ContextError::NoConfig)) => {
            crate::provisioning::provision_local()?;
            wait_client(matches).await
        }
        Err(error) => Err(error.into()),
    }
}

fn retry_local_connect(error: &ConnectError) -> bool {
    matches!(error, ConnectError::Context(ContextError::NoConfig)) || error.is_unreachable()
}

async fn wait_client(matches: &ArgMatches) -> Result<Client, Error> {
    let config = config_path(matches)?;
    let connect = matches.get_one::<String>("connect").map(String::as_str);
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match crate::connect::connect(&config, connect, None).await {
                Ok(client) => return Ok(client),
                Err(error) if retry_local_connect(&error) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(error) => return Err(Error::from(error)),
            }
        }
    })
    .await
    .map_err(|_| Error::usage("local daemon did not start".to_owned()))?
}

async fn ensure_uninitialized(
    matches: &ArgMatches,
    yes: bool,
    reset: bool,
    mut client: Client,
) -> Result<Client, Error> {
    let details = client
        .call::<op::Inspect>(InspectRequest::default(), None)
        .await?;
    if details.phase == LocalMachinePhase::Uninitialized {
        return Ok(client);
    }
    if !reset {
        return Err(Error::usage(
            "Machine is already initialised; rerun with --reset to reset it before enrollment"
                .to_owned(),
        ));
    }
    crate::handlers::machine::confirm(yes, "Reset the Machine before joining this Cluster?")?;
    client.call::<op::Reset>(ResetRequest {}, None).await?;
    wait_phase(
        matches,
        LocalMachinePhase::Uninitialized,
        "Machine did not reset",
    )
    .await
}

fn pairing_revoked(error: &ConnectError) -> bool {
    matches!(
        error,
        ConnectError::Remote(error) if error.code == RpcErrorCode::Unauthenticated
    )
}

async fn wait_phase(
    matches: &ArgMatches,
    phase: LocalMachinePhase,
    timeout_message: &str,
) -> Result<Client, Error> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io, path::PathBuf};

    use crate::context::ConnectionSource;

    #[test]
    fn wait_retries_no_config_and_unreachable_connect_errors() {
        assert!(retry_local_connect(&ConnectError::Context(
            ContextError::NoConfig
        )));
        assert!(retry_local_connect(&ConnectError::Io(io::Error::from(
            io::ErrorKind::ConnectionRefused
        ))));
        assert!(retry_local_connect(&ConnectError::AllFailed {
            source: ConnectionSource::LocalSocket,
            attempts: 1,
            last: None,
        }));
        assert!(!retry_local_connect(&ConnectError::Context(
            ContextError::NoCurrentContext(PathBuf::from("config.yaml"))
        )));
        assert!(!retry_local_connect(&ConnectError::InvalidDialCredential));
    }

    #[test]
    fn post_join_caddy_error_names_membership_and_recovery() {
        let message = crate::global_catch_up::joined_catch_up_error(
            crate::global_catch_up::CatchUpError::new(
                crate::failure::Failure::usage("not running".to_owned()),
                vec![ployz_core::QualifiedService::system_caddy()],
            ),
        );
        assert!(message.contains("Machine joined"));
        assert!(message.contains("ployz caddy deploy"));
    }

    #[test]
    fn post_join_other_error_names_membership() {
        let message = crate::global_catch_up::joined_catch_up_error(
            crate::global_catch_up::CatchUpError::new(
                crate::failure::Failure::usage("listing failed".to_owned()),
                Vec::new(),
            ),
        );
        assert!(message.contains("Machine joined"));
        assert!(message.contains("listing failed"));
    }
}
