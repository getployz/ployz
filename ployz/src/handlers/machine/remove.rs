use clap::ArgMatches;
use ployz_core::{
    DescribeContractRequest, Machine, MachineSelector, NameMatches, RemoveLocalMachineRequest,
    RemoveMachineRequest, op, resolve_machine_selector,
};

use super::super::{connect_client, runtime};
use super::{ConnectionOptions, helpers, machine_list, target};
use crate::{
    context::ConnectionSource,
    handlers::{Error, leaf_matches},
};

pub(in crate::handlers) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let matches = leaf_matches(root);
    let selector = target(matches, "machine")?.to_owned();
    let no_reset = matches.get_flag("no-reset");
    let yes = matches.get_flag("yes");
    runtime()?.block_on(async {
        let mut client = connect_client(matches, options.context()).await?;
        let context_name = match client.connection_source() {
            ConnectionSource::Context(name) => Some(name.clone()),
            ConnectionSource::Direct | ConnectionSource::LocalSocket => None,
        };
        let machines = machine_list(&mut client).await?;
        let selected = select_machine(&machines, &selector)?;
        let selected_target = MachineSelector::from(&selected.id);
        let current = client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await?
            .machine_id;
        if selected.id == current && machines.len() > 1 {
            return Err(Error::usage(
                "the current entry Machine cannot be removed while another Machine is visible",
            ));
        }
        helpers::confirm(
            yes,
            &format!("Remove Machine {} ({})?", selected.name, selected.id),
        )?;

        // TODO(UT-055): do not reroute away from the current entry before removal.
        // TODO(UT-056): there is no drain or unschedulable phase before cleanup.
        let mut shared_rows_removed_by_entry = false;
        if !no_reset {
            match client
                .call::<op::RemoveLocalMachine>(
                    RemoveLocalMachineRequest {
                        restart_on_cleanup_failure: selected.id != current,
                    },
                    Some(&selected_target),
                )
                .await
            {
                Ok(removed) => {
                    if let Some(warning) = &removed.reset_warning {
                        eprintln!("WARNING: target cleanup/reset failed: {warning}");
                    } else if selected.id == current {
                        shared_rows_removed_by_entry = true;
                    }
                }
                Err(error) if error.is_unreachable() => {
                    eprintln!("WARNING: target is unreachable; removing shared rows: {error}");
                }
                Err(error) => return Err(error.into()),
            }
        }
        if !shared_rows_removed_by_entry {
            client
                .call::<op::RemoveMachine>(
                    RemoveMachineRequest {
                        machine_id: selected.id,
                    },
                    None,
                )
                .await?;
        }

        // TODO(UT-057): removal does not update hosted DNS or Caddy projections.
        if let Some(context_name) = context_name {
            let mut config = options.load_config()?;
            let context = config
                .contexts
                .get_mut(&context_name)
                .ok_or_else(|| Error::usage(format!("context {context_name:?} not found")))?;
            if let Some(index) = context
                .connections
                .iter()
                .position(|connection| connection.machine_id() == Some(&selected.id))
            {
                context.connections.remove(index);
            }
            config.save()?;
        }
        println!("Removed Machine {} ({})", selected.name, selected.id);
        Ok::<_, Error>(())
    })
}

fn select_machine(
    machines: &[ployz_core::MachineObservation],
    selector: &str,
) -> Result<Machine, Error> {
    let selector = MachineSelector::parse(selector)?;
    match resolve_machine_selector(&selector, machines.iter().map(|entry| &entry.machine)) {
        NameMatches::None => Err(Error::usage(format!("Machine {selector:?} was not found"))),
        NameMatches::One(machine) => Ok(machine.clone()),
        NameMatches::Ambiguous(matches) => Err(Error::usage(format!(
            "Machine name {selector:?} is ambiguous: {}",
            matches
                .into_iter()
                .map(|machine| machine.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{RpcError, RpcErrorCode};
    use serde_json::Value;

    use crate::connect::ConnectError;

    #[test]
    fn reached_target_cleanup_rejections_are_not_unreachable_fallbacks() {
        assert!(
            ConnectError::Rpc(crate::connect::TransportError::from(
                tonic::Status::unavailable("route failed")
            ))
            .is_unreachable()
        );
        assert!(
            !ConnectError::Rpc(crate::connect::TransportError::from(
                tonic::Status::unimplemented("older daemon")
            ))
            .is_unreachable()
        );
        assert!(
            !ConnectError::Remote(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "Docker is unavailable".into(),
                details: Value::Null,
            })
            .is_unreachable()
        );
    }
}
