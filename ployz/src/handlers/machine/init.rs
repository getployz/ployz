use clap::ArgMatches;
use ployz_core::{InitializeRequest, InspectRequest, LocalMachinePhase, MachineName, op};

use super::super::runtime;
use super::{ConnectionOptions, helpers};
use crate::{
    connect::DEFAULT_LOCAL_SOCKET,
    context::{Connection, Context},
    handlers::{Error, leaf_matches},
};

pub(in crate::handlers) fn init(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
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
    let destination = matches.get_one::<String>("destination");
    let connection = match destination {
        None => Connection::unix(DEFAULT_LOCAL_SOCKET)?,
        Some(dest) => helpers::configure_ssh_key(
            dest.parse()?,
            matches.get_one::<String>("ssh-key").map(String::as_str),
        )?,
    };
    let local = destination.is_none();
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
    let no_install = matches.get_flag("no-install");
    let storage = crate::provisioning::resolve_storage(matches)?;
    if !no_install {
        if local {
            crate::provisioning::provision_local(storage)?;
        } else {
            crate::provisioning::provision(matches, storage)?;
        }
    }

    let (machine, connection) = runtime()?.block_on(async {
        let mut target = if !no_install {
            helpers::reconnect_direct(&connection).await?
        } else {
            helpers::connect_direct(&connection).await?
        };
        let mut token = target
            .call_repeatable::<op::MachineToken>(token_request.clone(), None)
            .await?;
        let details = target
            .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
            .await?;
        if details.phase != LocalMachinePhase::Uninitialized {
            helpers::confirm(yes, "Reset the Machine before initialising a new Cluster?")?;
            helpers::reset(&mut target).await?;
            target = helpers::reconnect_direct(&connection).await?;
            token = target
                .call_repeatable::<op::MachineToken>(token_request, None)
                .await?;
        }
        let name = helpers::machine_name(requested_name, &token)?;
        let machine = helpers::initialize(
            &mut target,
            InitializeRequest {
                name,
                cluster_network,
                public_ip: token.public_ip,
                advertised_endpoints: token.advertised_endpoints,
                wireguard_mtu,
                cloud_pairing: None,
            },
        )
        .await?
        .machine;
        let connection = connection.with_machine_id(machine.id);
        Ok::<_, Error>((machine, connection))
    })?;

    config.contexts.insert(
        context_name.clone(),
        Context {
            connections: vec![connection.clone()],
        },
    );
    config.set_current_context(Some(context_name.clone()))?;
    config.save()?;
    if let Some(current_context) = config.current_context() {
        println!("Switched context to '{current_context}'");
    }
    println!("Initialised Machine {} ({})", machine.name, machine.id);
    let want_ingress = !matches.get_flag("no-ingress");
    let want_dns = !matches.get_flag("no-dns");
    let ingress_recovery =
        super::super::recovery_command(matches, &context_name, &["ingress", "deploy"]);
    let inspect_recovery = super::super::recovery_command(
        matches,
        &context_name,
        &["machine", "inspect", machine.name.as_str()],
    );
    runtime()?.block_on(async {
        let mut ready =
            helpers::wait_direct_participating(&connection, "initial Machine did not become ready")
                .await.map_err(|error| Error::usage(format!("Machine initialized; startup incomplete: {error}\nInspect with: {inspect_recovery}")))?;
        if want_dns {
            let endpoint = matches
                .get_one::<String>("dns-endpoint")
                .cloned()
                .ok_or_else(|| Error::usage("dns-endpoint is required"))?;
            let show = super::super::recovery_command(matches, &context_name, &["dns", "show"]);
            let reserve = super::super::recovery_command(matches, &context_name, &["dns", "reserve", "--endpoint", &endpoint]);
            let domain = crate::dns::reserve_if_missing(&mut ready, endpoint).await.map_err(|error| {
                Error::usage(format!("Machine initialized; domain reservation incomplete: {error}\nCheck: {show}\nIf no domain is reserved: {reserve}\nContinue ingress setup with: {ingress_recovery}"))
            })?;
            println!("Reserved Cluster domain: {domain}");
        }
        if want_ingress {
            let requested = crate::ingress::service_spec(None, Vec::new(), None).await.map_err(|error| Error::usage(format!("Machine initialized; ingress image discovery failed: {error}\nContinue with: {ingress_recovery}")))?;
            crate::deploy::apply_requested(&mut ready, &requested).await.map_err(|error| {
                let error: Error = error.into();
                Error::usage(format!("Machine initialized; ingress deployment incomplete: {error}\nContinue with: {ingress_recovery}"))
            })?;
            if want_dns {
                crate::dns::update_records_for_ingress(&mut ready).await.map_err(|error| Error::usage(format!("Machine initialized; ingress healthy; DNS publication pending: {error}\nAllow outbound access if blocked, then run: {ingress_recovery}")))?;
            }
        }
        Ok::<_, Error>(())
    })?;
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
