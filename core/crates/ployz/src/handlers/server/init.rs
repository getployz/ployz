use clap::ArgMatches;
use ployz_core::{ClusterName, EnrollAs, EnrollRequest, Hint, LocalMachinePhase, RpcErrorCode};

use super::super::runtime;
use super::{ConnectionOptions, helpers};
use crate::enrollment::machine;
use crate::{
    context::{Context, Transport},
    handlers::{Error, leaf_matches},
};
use serde_json::json;

pub(in crate::handlers) fn init(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let policy = super::enrollment_policy(matches)?;
    if crate::cli::connection(matches).is_some() {
        return Err(Error::usage(
            "server add --standalone founds a new context; do not use --connect",
        ));
    }
    let options = ConnectionOptions::from_matches(root)?;
    let config = options.load_or_empty_config()?;
    let context_name = config
        .context_name(options.context())
        .unwrap_or("default")
        .to_owned();
    if config
        .contexts
        .get(&context_name)
        .is_some_and(|context| !context.connections.is_empty())
    {
        return Err(Error::conflict(format!(
            "context {context_name:?} already exists"
        )));
    }
    // On the Server's own socket the person is root there: a Device made there would be root's
    // on that Server, ride into every later join approval, and outlive the Server. With none,
    // the Cluster would trust no key, so founding takes a computer that holds one.
    let on_server = || {
        Error::usage(
            "A Cluster started on its own Server would trust no key. Start it from your computer, which approves your device on it:\n  ployz server add --standalone root@HOST",
        )
    };
    let destination = matches
        .get_one::<String>("destination")
        .ok_or_else(on_server)?;
    let connection = helpers::configure_ssh_key(
        destination.parse()?,
        matches.get_one::<String>("ssh-key").map(String::as_str),
    )?;
    if matches!(connection.transport(), Transport::Unix(_)) {
        return Err(on_server());
    }
    let requested_name = helpers::requested_name(matches)?;
    let cluster_name = founding_name(root)?;
    let token_request = helpers::token_request(matches)?;
    let pinned = machine::pinned(&token_request);
    let cluster_network = *matches
        .get_one::<ipnet::Ipv4Net>("network")
        .expect("Cluster network has a default");
    let wireguard_mtu = matches.get_one::<u32>("wg-mtu").copied();
    let reset_asks =
        (!matches.get_flag("yes")).then(|| crate::cli::Rerun::again(root).switch("yes"));
    let no_install = matches.get_flag("no-install");
    let storage = super::provision::resolve_storage(root)?;
    let runtime = runtime()?;
    let (machine, connection) = runtime.block_on(async {
        if !no_install {
            super::provision::provision(matches, &storage).await?;
        }
        let mut target = if !no_install {
            helpers::reconnect_direct(matches, &connection).await?
        } else {
            helpers::connect_direct(matches, &connection).await?
        };
        let (details, mut token) =
            machine::inspect_token(&mut target, token_request.clone()).await?;
        if details.phase != LocalMachinePhase::Uninitialized {
            machine::confirm(
                reset_asks.clone(),
                "Reset the Server before initialising a new Cluster?",
            )?;
            machine::leave(&mut target, &details).await?;
            target = helpers::reconnect_direct(matches, &connection).await?;
            token = machine::inspect_token(&mut target, token_request).await?.1;
        }
        let name = machine::machine_name(requested_name, &token);
        let machine = machine::enroll(
            &mut target,
            EnrollRequest {
                role: EnrollAs::Founder {
                    initial_policy: policy
                        .settle(None, |args| crate::handlers::rerun(matches, args))?,
                    name,
                    cluster_network,
                    public_ip: token.public_ip,
                    advertised_endpoints: token.advertised_endpoints,
                    grant: Some(ployz_core::FoundingGrant::Standalone),
                },
                wireguard_mtu,
                pinned,
            },
        )
        .await?;
        let connection = connection.with_machine_id(machine.id);
        Ok::<_, Error>((machine, connection))
    })?;

    config.update(|latest| {
        latest.contexts.insert(
            context_name.clone(),
            Context {
                connections: vec![connection.clone()],
            },
        );
        Ok::<_, Error>(latest.set_current_context(Some(context_name.clone()))?)
    })?;
    crate::ui::note(format_args!("Switched context to {context_name}."));
    crate::ui::stream(format_args!("Initialised Server {}.", machine.name));
    let recovery =
        |args: &[&str]| crate::cli::Rerun::then(matches, args).set("context", &context_name);
    let ingress_recovery = recovery(&[
        "server",
        "set",
        machine.id.as_str(),
        "--accepts-ingress=true",
    ])
    .line();
    let inspect_recovery = recovery(&["server", "show", machine.name.as_str()]).line();
    let mut named = None;
    let ingress = runtime.block_on(async {
        let mut ready = helpers::wait_direct_participating(
            matches,
            &connection,
            "initial Server did not become ready",
        )
        .await
        .map_err(|error| {
            error
                .context("Server initialized; startup incomplete.")
                .hint(Hint::Inspect(inspect_recovery.clone()))
        })?;
        named = crate::enrollment::founding::name_cluster(&ready, cluster_name, |args| {
            recovery(args).line()
        })
        .await;
        let code =
            crate::handlers::device::approve_own(&ready, &crate::handlers::config_path(matches)?)
                .await
                .map_err(|error| {
                    error
                        .context(
                            "Server initialized; this computer's device isn't approved on it yet.",
                        )
                        .hint(Hint::Next("ployz device request".to_owned()))
                })?;
        crate::ui::note(format_args!(
            "Approved this computer's device {code} on {}.",
            machine.name
        ));
        let closed = matches.get_flag("ingress-closed");
        crate::enrollment::founding::open_ingress(&mut ready, &machine, closed)
            .await
            .map_err(|error| {
                crate::enrollment::output::ingress_incomplete(
                    "Server initialized",
                    crate::enrollment::output::ingress_hints(error.into(), recovery),
                    Hint::Next(ingress_recovery.clone()),
                )
            })
    });
    // The Machine and its context are committed: print them before a follow-up failure.
    let named = named.unwrap_or(Ok(Default::default()));
    let result = json!({
        "server": crate::enrollment::output::server_json(&machine),
        "context": context_name,
        "cluster": named.as_ref().ok(),
        "ingress": ingress.as_ref().ok().and_then(Option::as_ref),
    });
    crate::ui::emit_committed(result, ingress.map(drop).and(named.map(drop)))
}

fn founding_name(root: &ArgMatches) -> Result<Option<ClusterName>, Error> {
    if let Some(name) = super::founding_cluster_name(leaf_matches(root))? {
        return Ok(Some(name));
    }
    let unnamed_founding = || unnamed_founding(root);
    if crate::ui::json() {
        return Err(unnamed_founding());
    }
    if !crate::ui::can_prompt() {
        return Ok(None);
    }
    crate::ui::input(
        "Name this Cluster",
        |typed: &str| ClusterName::parse(typed).map_err(|_| crate::cluster::name::VALID_NAME),
        unnamed_founding,
        "Cancelled. No Server was set up.",
    )
    .map(Some)
}

fn unnamed_founding(root: &ArgMatches) -> Error {
    Error::detailed(
        RpcErrorCode::InvalidArgument,
        "A new Cluster needs a name. Pass --cluster-name.",
        json!({ "flag": "--cluster-name" }),
    )
    .hint(crate::cli::Rerun::again(root).placeholder("cluster-name", "NAME"))
}
