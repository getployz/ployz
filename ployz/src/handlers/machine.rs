use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use clap::ArgMatches;
use ployz_core::{
    AdvertisedEndpoint, ListMachinesRequest, MachineName, MachineObservation, MachineTarget,
    MachineUpdate, PublicIpUpdate, UpdateMachineRequest, op,
};

use crate::{connect::Client, context::Config};

use super::{Error, leaf_matches, string_values, with_client};

mod add;
mod helpers;
mod init;
mod inspect;
mod remove;

pub(super) use add::add;
pub(super) use init::init;
pub(super) use inspect::{list, rtt, wireguard_show};
pub(super) use remove::remove;

const DEFAULT_WIREGUARD_PORT: u16 = 51820;

pub(super) struct ConnectionOptions {
    config_path: PathBuf,
    context: Option<String>,
}

impl ConnectionOptions {
    pub(super) fn from_matches(matches: &ArgMatches) -> Result<Self, Error> {
        Ok(Self {
            config_path: super::config_path(matches)?,
            context: super::leaf_matches(matches)
                .get_one::<String>("context")
                .cloned(),
        })
    }

    pub(super) fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub(super) fn active_config(&self) -> Result<(Config, String), Error> {
        let config = self.load_config()?;
        let name = self
            .context
            .clone()
            .unwrap_or_else(|| config.current_context.clone());
        if name.is_empty() {
            return Err(Error::usage(format!(
                "current context is not set in Ployz config {}",
                config.path().display()
            )));
        }
        if !config.contexts.contains_key(&name) {
            return Err(Error::usage(format!("context {name:?} not found")));
        }
        Ok((config, name))
    }

    pub(super) fn load_config(&self) -> Result<Config, Error> {
        Ok(Config::load(&self.config_path)?)
    }

    pub(super) fn load_or_empty_config(&self) -> Result<Config, Error> {
        Ok(Config::load_or_empty(&self.config_path)?)
    }
}

pub(super) async fn machine_list(client: &mut Client) -> Result<Vec<MachineObservation>, Error> {
    Ok(client
        .call::<op::ListMachines>(ListMachinesRequest {}, None)
        .await?
        .machines)
}

pub(super) fn target<'a>(matches: &'a ArgMatches, name: &str) -> Result<&'a str, Error> {
    matches
        .get_one::<String>(name)
        .map(String::as_str)
        .ok_or_else(|| Error::usage(format!("{name} is required")))
}

pub(super) fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selector = target(matches, "old-name")?;
    let name = MachineName::parse(target(matches, "new-name")?)?;
    update_target(
        root,
        selector,
        MachineUpdate {
            name: Some(name),
            ..Default::default()
        },
    )
}

pub(super) fn update(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let selector = target(matches, "machine")?;
    let update = parse_update(matches)?;
    update_target(root, selector, update)
}

fn update_target(root: &ArgMatches, selector: &str, update: MachineUpdate) -> Result<(), Error> {
    let selector = MachineTarget::parse(selector)?;
    with_client(root, |client| {
        Box::pin(async move {
            let machine = client
                .call::<op::UpdateMachine>(UpdateMachineRequest { update }, Some(&selector))
                .await?;
            println!(
                "Updated Machine {} ({})",
                machine.machine.name, machine.machine.id
            );
            Ok(())
        })
    })
}

fn parse_update(matches: &ArgMatches) -> Result<MachineUpdate, Error> {
    let name = matches
        .get_one::<String>("name")
        .map(MachineName::parse)
        .transpose()?;
    let public_ip = match matches.get_one::<String>("public-ip").map(String::as_str) {
        None => PublicIpUpdate::Keep,
        Some("") | Some("none") => PublicIpUpdate::Remove,
        Some(value) => PublicIpUpdate::Set(
            value
                .parse::<IpAddr>()
                .map_err(|_| Error::usage(format!("invalid public IP {value:?}")))?,
        ),
    };
    let advertised_endpoints = if matches.get_many::<String>("wg-endpoint").is_some() {
        Some(parse_endpoints(
            &string_values(matches, "wg-endpoint"),
            DEFAULT_WIREGUARD_PORT,
        )?)
    } else {
        None
    };
    let update = MachineUpdate {
        name,
        public_ip,
        advertised_endpoints,
    };
    if update.is_empty() {
        return Err(Error::usage("at least one Machine update flag is required"));
    }
    if update
        .advertised_endpoints
        .as_ref()
        .is_some_and(Vec::is_empty)
    {
        return Err(Error::usage("at least one WireGuard endpoint is required"));
    }
    Ok(update)
}

pub(super) fn parse_endpoints(
    values: &[String],
    default_port: u16,
) -> Result<Vec<AdvertisedEndpoint>, Error> {
    values
        .iter()
        .map(|value| {
            value
                .parse::<SocketAddr>()
                .or_else(|_| {
                    value
                        .parse::<IpAddr>()
                        .map(|address| SocketAddr::new(address, default_port))
                })
                .map(AdvertisedEndpoint)
                .map_err(|_| Error::usage(format!("invalid WireGuard endpoint {value:?}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_accept_socket_or_ip_and_apply_the_default_port() {
        assert_eq!(
            parse_endpoints(&["192.0.2.1".into(), "[2001:db8::1]:6000".into()], 51820).unwrap(),
            [
                AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap()),
                AdvertisedEndpoint("[2001:db8::1]:6000".parse().unwrap()),
            ]
        );
        assert!(parse_endpoints(&["not-an-address".into()], 51820).is_err());
    }

    #[test]
    fn machine_add_rejects_a_wireguard_port_the_daemon_cannot_apply() {
        assert!(
            crate::cli::command()
                .try_get_matches_from([
                    "ployz",
                    "machine",
                    "add",
                    "root@example.com",
                    "--wg-port",
                    "51821",
                ])
                .is_err()
        );
    }

    #[test]
    fn update_cli_rejects_an_empty_patch_and_maps_none_to_public_ip_removal() {
        let command = crate::cli::command();
        let empty = command
            .clone()
            .try_get_matches_from(["ployz", "machine", "update", "node-a"])
            .unwrap();
        assert!(parse_update(leaf_matches(&empty)).is_err());

        let remove = command
            .try_get_matches_from(["ployz", "machine", "update", "node-a", "--public-ip=none"])
            .unwrap();
        assert_eq!(
            parse_update(leaf_matches(&remove)).unwrap().public_ip,
            PublicIpUpdate::Remove
        );
    }

    #[test]
    fn singular_machine_cli_target_rejects_star_and_keeps_all_as_identity() {
        let command = crate::cli::command();
        let star = command
            .clone()
            .try_get_matches_from(["ployz", "machine", "update", "*", "--name", "edge"])
            .unwrap();
        assert!(MachineTarget::parse(target(leaf_matches(&star), "machine").unwrap()).is_err());

        let named_all = command
            .try_get_matches_from(["ployz", "machine", "update", "all", "--name", "edge"])
            .unwrap();
        assert_eq!(
            MachineTarget::parse(target(leaf_matches(&named_all), "machine").unwrap())
                .unwrap()
                .as_str(),
            "all"
        );
    }
}
