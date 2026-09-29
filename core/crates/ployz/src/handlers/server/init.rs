use clap::ArgMatches;
use ployz_core::{
    InitializeRequest, InspectRequest, LocalMachinePhase, MachineName, MachineRelease, op,
};

use super::super::runtime;
use super::{ConnectionOptions, helpers};
use crate::{
    connect::DEFAULT_LOCAL_SOCKET,
    context::{Connection, Context},
    handlers::{Error, leaf_matches},
    output::{self, say},
};
use serde_json::json;

pub(in crate::handlers) fn init(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let policy = super::enrollment_policy(matches)?;
    if matches.get_one::<String>("connect").is_some() {
        return Err(Error::usage(
            "server add --standalone founds a new context; do not use --connect",
        ));
    }
    let options = ConnectionOptions::from_matches(root)?;
    let mut config = options.load_or_empty_config()?;
    let context_name = matches
        .get_one::<String>("context")
        .cloned()
        .unwrap_or_else(|| "default".into());
    if config.contexts.contains_key(&context_name) {
        return Err(Error::conflict(format!(
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
    let cluster_network = *matches
        .get_one::<ipnet::Ipv4Net>("network")
        .expect("Cluster network has a default");
    let wireguard_mtu = matches.get_one::<u32>("wg-mtu").copied();
    let yes = matches.get_flag("yes");
    let no_install = matches.get_flag("no-install");
    let storage = crate::provisioning::resolve_storage(matches)?;
    let version = &matches
        .get_one::<MachineRelease>("version")
        .expect("version has a default")
        .to_string();
    let runtime = runtime()?;
    let (machine, connection) = runtime.block_on(async {
        if !no_install {
            if local {
                crate::provisioning::provision_local(version, storage).await?;
            } else {
                crate::provisioning::provision(matches, storage).await?;
            }
        }
        let mut target = if !no_install {
            helpers::reconnect_direct(matches, &connection).await?
        } else {
            helpers::connect_direct(matches, &connection).await?
        };
        let mut token = target
            .call_repeatable::<op::MachineToken>(token_request.clone(), None)
            .await?;
        let details = target
            .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
            .await?;
        if details.phase != LocalMachinePhase::Uninitialized {
            helpers::confirm(yes, "Reset the Server before initialising a new Cluster?")?;
            helpers::reset(&mut target).await?;
            target = helpers::reconnect_direct(matches, &connection).await?;
            token = target
                .call_repeatable::<op::MachineToken>(token_request, None)
                .await?;
        }
        let name = helpers::machine_name(requested_name, &token)?;
        let machine = helpers::initialize(
            &mut target,
            InitializeRequest {
                initial_policy: policy,
                name,
                cluster_network,
                public_ip: token.public_ip,
                advertised_endpoints: token.advertised_endpoints,
                wireguard_mtu,
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
        say!("Switched context to '{current_context}'");
    }
    say!("Initialised Server {} ({})", machine.name, machine.id);
    let ingress_recovery =
        super::super::recovery_command(matches, &context_name, &["ingress", "deploy"]);
    let inspect_recovery = super::super::recovery_command(
        matches,
        &context_name,
        &["server", "inspect", machine.name.as_str()],
    );
    let ingress = runtime.block_on(async {
        let mut ready =
            helpers::wait_direct_participating(matches, &connection, "initial Server did not become ready")
                .await.map_err(|error| error.reworded(format!("Server initialized; startup incomplete: {error}\nInspect with: {inspect_recovery}")))?;
        if machine.accepts_ingress {
            let requested = crate::ingress::service_spec(None, Default::default()).await.map_err(|error| {
                let error = Error::from(error);
                error.reworded(format!("Server initialized; ingress image discovery failed: {error}\nContinue with: {ingress_recovery}"))
            })?;
            let outcome = crate::deploy::apply_requested(&mut ready, &requested, false, false, "default").await.map_err(|error| {
                let error: Error = error.into();
                error.reworded(format!("Server initialized; ingress deployment incomplete: {error}\nContinue with: {ingress_recovery}"))
            })?;
            return Ok(Some(outcome));
        }
        Ok::<_, Error>(None)
    });
    // The Machine and its context are committed: print them before a follow-up failure.
    let result = json!({
        "server": machine,
        "context": context_name,
        "ingress": ingress.as_ref().ok().and_then(Option::as_ref),
    });
    output::emit_committed(result, ingress.map(drop))
}

#[cfg(test)]
mod tests {
    use ployz_core::DOCKER_NETWORK_CONFLICT_RECOVERY;

    use super::*;

    #[test]
    fn init_timeout_surfaces_the_docker_network_recovery() {
        let message = helpers::readiness_timeout_message("initial Server did not become ready");

        assert!(message.contains("initial Server did not become ready"));
        assert!(message.contains(DOCKER_NETWORK_CONFLICT_RECOVERY));
    }
}
