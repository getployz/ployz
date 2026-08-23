use clap::ArgMatches;
use ployz_core::{
    InitializeRequest, InspectRequest, LocalMachinePhase, MachineName, ReserveDomainRequest,
    ResetRequest, op,
};

use super::super::runtime;
use super::{ConnectionOptions, helpers};
use crate::{
    context::Context,
    handlers::{Error, leaf_matches},
};

pub(in crate::handlers) fn init(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let Some(destination) = matches.get_one::<String>("destination") else {
        // TODO(UT-049): local Machine initialization remains outside the remote lifecycle path.
        return Err(Error::usage(
            "local machine initialisation is not implemented; specify a remote machine",
        ));
    };
    if matches.get_one::<String>("connect").is_some() {
        return Err(Error::usage(
            "machine init creates a new context; do not use --connect",
        ));
    }
    let options = ConnectionOptions::from_matches(root)?;
    let mut config = options.load_or_empty_config()?;
    let context_name = matches
        .get_one::<String>("context")
        .cloned()
        .unwrap_or_else(|| "default".into());
    if config.contexts.contains_key(&context_name) {
        return Err(Error::usage(format!(
            "context {context_name:?} already exists"
        )));
    }
    let connection = destination.parse()?;
    let connection = helpers::configure_ssh_key(
        connection,
        matches.get_one::<String>("ssh-key").map(String::as_str),
    )?;
    let requested_name = matches
        .get_one::<String>("name")
        .map(MachineName::parse)
        .transpose()?;
    let token_request = helpers::token_request(matches)?;
    let cluster_network = matches
        .get_one::<String>("network")
        .expect("Cluster network has a default")
        .parse()
        .map_err(|error| Error::usage(format!("invalid Cluster network: {error}")))?;
    let wireguard_mtu = matches.get_one::<u32>("wg-mtu").copied();
    let yes = matches.get_flag("yes");
    let storage = crate::provisioning::resolve_storage(matches)?;
    if !matches.get_flag("no-install") {
        crate::provisioning::provision(matches, storage)?;
    }

    let (machine, connection) = runtime()?.block_on(async {
        let mut target = helpers::connect_direct(&connection).await?;
        let mut token = target
            .call::<op::MachineToken>(token_request.clone(), None)
            .await?;
        let details = target
            .call::<op::Inspect>(InspectRequest::default(), None)
            .await?;
        if details.phase != LocalMachinePhase::Uninitialized {
            helpers::confirm(yes, "Reset the Machine before initialising a new Cluster?")?;
            target.call::<op::Reset>(ResetRequest {}, None).await?;
            target = helpers::reconnect_direct(&connection).await?;
            token = target.call::<op::MachineToken>(token_request, None).await?;
        }
        let name = helpers::machine_name(requested_name, &token)?;
        let machine = target
            .call::<op::Initialize>(
                InitializeRequest {
                    name,
                    cluster_network,
                    public_ip: token.public_ip,
                    advertised_endpoints: token.advertised_endpoints,
                    wireguard_mtu,
                    cloud_pairing: None,
                },
                None,
            )
            .await?
            .machine;
        let connection = connection.with_machine_id(machine.id);
        Ok::<_, Error>((machine, connection))
    })?;

    config.set_current_context(Some(context_name.clone()));
    config.contexts.insert(
        context_name,
        Context {
            connections: vec![connection.clone()],
        },
    );
    config.save()?;
    if let Some(current_context) = config.current_context() {
        println!("Switched context to '{current_context}'");
    }
    let want_caddy = !matches.get_flag("no-caddy");
    let want_dns = !matches.get_flag("no-dns");
    runtime()?.block_on(async {
        let mut ready =
            helpers::wait_direct_participating(&connection, "initial Machine did not become ready")
                .await?;
        if want_dns {
            let endpoint = matches
                .get_one::<String>("dns-endpoint")
                .cloned()
                .ok_or_else(|| Error::usage("dns-endpoint is required"))?;
            let domain = ready
                .call::<op::ReserveDomain>(ReserveDomainRequest { endpoint }, None)
                .await?;
            println!("Reserved Cluster domain: {}", domain.name);
        }
        if want_caddy {
            let image = crate::caddy::latest_image().await?;
            let requested = crate::caddy::service_spec(image, Vec::new(), None);
            crate::deploy::apply_requested(&mut ready, &requested).await?;
            if want_dns {
                crate::dns::update_records_for_caddy(&mut ready).await?;
            }
        }
        Ok::<_, Error>(())
    })?;
    println!("Initialised Machine {} ({})", machine.name, machine.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use ployz_core::DOCKER_NETWORK_CONFLICT_RECOVERY;

    use super::*;

    #[test]
    fn init_timeout_surfaces_the_docker_network_recovery() {
        let message = helpers::readiness_timeout_message("initial Machine did not become ready");

        assert!(message.contains("initial Machine did not become ready"));
        assert!(message.contains(DOCKER_NETWORK_CONFLICT_RECOVERY));
    }
}
