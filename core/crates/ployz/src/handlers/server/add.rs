use clap::ArgMatches;
use ployz_core::{
    InspectRequest, JoinRequest, LocalMachinePhase, MachineName, RegisterRequest, op,
};

use super::super::{connect_context, runtime};
use super::{ConnectionOptions, helpers, target};
use serde_json::json;

use crate::handlers::{Error, leaf_matches};

pub(in crate::handlers) fn add(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let policy = super::enrollment_policy(matches)?;
    let options = ConnectionOptions::from_matches(root)?;
    let (config, context_name) = options.active_config()?;
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
    let no_install = matches.get_flag("no-install");
    let runtime = runtime()?;
    let assigned = runtime.block_on(async {
        if !no_install {
            crate::provisioning::provision(matches, storage).await?;
        }
        let mut entry = connect_context(matches, options.context()).await?;
        let snapshot = crate::enrollment::observe_enrollment(&mut entry).await?;
        let mut target_client = if no_install {
            helpers::connect_direct(matches, &connection).await?
        } else {
            helpers::reconnect_direct(matches, &connection).await?
        };
        let mut token = target_client
            .call_repeatable::<op::MachineToken>(token_request.clone(), None)
            .await?;
        let details = target_client
            .call_repeatable::<op::Inspect>(
                InspectRequest {
                    advertised_endpoints: token.advertised_endpoints.clone(),
                    ..Default::default()
                },
                None,
            )
            .await?;
        let history = config.path().with_extension("enrollment");
        let resuming = crate::enrollment::local::has_assignment(&history, &snapshot, token.id)?
            || snapshot
                .machines
                .iter()
                .any(|machine| machine.id == token.id && machine.public_key == token.public_key);
        if details.phase != LocalMachinePhase::Uninitialized && !resuming {
            helpers::confirm(yes, "Reset the Server before adding it to this Cluster?")?;
            helpers::reset(&mut target_client).await?;
            target_client = helpers::reconnect_direct(matches, &connection).await?;
            token = target_client
                .call_repeatable::<op::MachineToken>(token_request, None)
                .await?;
        }
        let name = helpers::machine_name(requested_name, &token)?;

        let assignment = crate::enrollment::local::save_assignment(
            &history,
            &RegisterRequest {
                machine_id: token.id,
                assigned_subnet: None,
                initial_policy: policy,
                name,
                storage,
                public_key: token.public_key,
                public_ip: token.public_ip,
                advertised_endpoints: token.advertised_endpoints,
                runtime: token.runtime,
            },
            &snapshot,
        )?;
        let assigned = assignment.machine.clone();
        let registration = crate::enrollment::publish_enrollment(&mut entry, &assignment).await?;
        helpers::join(
            &mut target_client,
            JoinRequest {
                registration,
                wireguard_mtu,
            },
        )
        .await?;

        Ok::<_, Error>(assigned)
    })?;
    // The Machine joined: everything after is a follow-up that makes the result partial.
    let follow_up = (|| {
        connection = connection.with_machine_id(assigned.id);
        config.save_connection(&context_name, connection.clone())?;
        crate::ui::stream(format_args!("Added Server {}.", assigned.name));

        runtime.block_on(helpers::wait_direct_participating(
            matches,
            &connection,
            "added Server did not become ready",
        ))?;

        let catch_up = runtime.block_on(async {
            let mut entry = super::super::reconnect_client(matches, options.context()).await?;
            Ok::<_, Error>(crate::global_catch_up::follow_globals(&mut entry, &assigned).await)
        })?;
        catch_up.map_err(|error| crate::global_catch_up::joined_catch_up_error(error, &assigned))
    })();
    crate::ui::emit_committed(
        json!({ "server": super::server_json(&assigned) }),
        follow_up,
    )
}

#[cfg(test)]
mod tests {
    use ployz_core::DOCKER_NETWORK_CONFLICT_RECOVERY;
    use ployz_core::{Machine, MachineId, MachineName, WireGuardPublicKey};

    use super::*;

    #[test]
    fn add_timeout_surfaces_the_docker_network_recovery() {
        let message = helpers::readiness_timeout_message("added Server did not become ready");

        assert!(message.contains("added Server did not become ready"));
        assert!(message.contains(DOCKER_NETWORK_CONFLICT_RECOVERY));
    }

    #[test]
    fn catch_up_failure_after_add_reports_joined_membership() {
        let assigned = assigned_machine("edge", 'a');
        let failure = crate::global_catch_up::joined_catch_up_error(
            crate::global_catch_up::CatchUpError::new(
                crate::failure::Failure::usage("deploy timed out".to_owned())
                    .hint(crate::ui::Hint::Next("ployz deploy".into())),
                vec![ployz_core::QualifiedService::system_ingress()],
            ),
            &assigned,
        );
        let message = failure.to_string();
        assert!(message.starts_with("Server joined"), "{message}");
        assert!(message.contains("remains a Cluster member"), "{message}");
        let hints = failure.hints();
        let command = hints
            .iter()
            .find_map(|hint| match hint {
                crate::ui::Hint::Retry(command) => Some(command),
                crate::ui::Hint::Next(_)
                | crate::ui::Hint::Inspect(_)
                | crate::ui::Hint::Undo(_)
                | crate::ui::Hint::Closest(_)
                | crate::ui::Hint::Valid(_) => None,
            })
            .unwrap();
        assert_eq!(
            shell_words::split(command).unwrap(),
            [
                "ployz",
                "server",
                "set",
                &assigned.id.to_string(),
                "--accepts-ingress=true"
            ]
        );
        assert_eq!(failure.causes(), ["deploy timed out"]);
        assert_eq!(
            failure.report().code,
            ployz_core::RpcErrorCode::InvalidArgument
        );
        assert!(hints.contains(&crate::ui::Hint::Next("ployz deploy".into())));
    }

    fn assigned_machine(name: &str, seed: char) -> Machine {
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::parse(seed.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: "10.210.1.0/24".parse().unwrap(),
            public_key: WireGuardPublicKey([seed as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
            build_concurrency: None,
        }
    }
}
