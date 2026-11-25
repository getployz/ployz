use clap::ArgMatches;
use ployz_core::{InspectRequest, MACHINE_START_WAIT, MachineName};

use crate::enrollment::machine::{
    Joining, STARTING, check_listed, connect_direct_within, machine_name_requested,
    token_request_for,
};
use crate::{
    connect::{Client, ConnectError},
    context::{Connection, Transport},
    handlers::{Error, string_values},
};

pub(in crate::handlers) fn token_request(matches: &ArgMatches) -> Result<InspectRequest, Error> {
    token_request_for(
        matches.get_one::<String>("public-ip").map(String::as_str),
        &string_values(matches, "wg-endpoint"),
    )
}

/// The `--name` the user passed, refused with the normalised form when it isn't a Machine Name.
pub(in crate::handlers) fn requested_name(
    matches: &ArgMatches,
) -> Result<Option<MachineName>, Error> {
    machine_name_requested(matches.get_one::<String>("name").map(String::as_str))
}

pub(super) fn configure_ssh_key(
    mut connection: Connection,
    key: Option<&str>,
) -> Result<Connection, Error> {
    if matches!(connection.transport(), Transport::Ssh { .. })
        && let Some(key) = key
    {
        connection = connection.with_ssh_key_file(key)?;
    }
    Ok(connection)
}

pub(in crate::handlers) async fn connect_direct(
    matches: &ArgMatches,
    connection: &Connection,
) -> Result<Client, ConnectError> {
    connect_direct_within(crate::cli::ssh_timeout(matches), connection).await
}

pub(super) async fn reconnect_direct(
    matches: &ArgMatches,
    connection: &Connection,
) -> Result<Client, Error> {
    crate::setup_retry::run(
        &mut (),
        &crate::ui::WaitNotices,
        &format!("Reconnecting to {}", connection.shown()),
        crate::setup_retry::WAIT,
        ConnectError::is_setup_retryable,
        async |_| connect_direct(matches, connection).await,
    )
    .await
    .map_err(Into::into)
}

pub(super) async fn wait_direct_participating(
    matches: &ArgMatches,
    connection: &Connection,
    timeout_message: &str,
) -> Result<Client, Error> {
    let mut joining = Joining::default();
    crate::setup_retry::run_expecting(
        &mut joining,
        &crate::ui::WaitNotices,
        &format!("Waiting for {} to participate", connection.shown()),
        Some(STARTING),
        MACHINE_START_WAIT,
        ConnectError::is_start_retryable,
        async |joining| {
            let mut client = connect_direct(matches, connection).await?;
            check_listed(&mut client, joining).await?;
            Ok(client)
        },
    )
    .await
    .map_err(|error| joining.failed(error, timeout_message))
}

#[cfg(test)]
mod tests {
    use ployz_core::{Hint, MACHINE_API_PORT, RpcErrorCode};

    use super::*;

    #[test]
    fn machine_add_only_configures_keys_for_ssh_connections() {
        let key = "/tmp/id_ed25519";
        let ssh = configure_ssh_key("root@example.com".parse().unwrap(), Some(key)).unwrap();
        assert_eq!(ssh.ssh_key_file(), Some(std::path::Path::new(key)));

        for destination in [
            format!("tcp://127.0.0.1:{MACHINE_API_PORT}"),
            "unix:///tmp/ployz.sock".into(),
        ] {
            let connection = configure_ssh_key(destination.parse().unwrap(), Some(key)).unwrap();
            assert_eq!(connection.ssh_key_file(), None, "destination {destination}");
        }
    }

    #[test]
    fn an_invalid_requested_name_is_refused_with_its_normalised_form_as_a_hint() {
        let matches = |name: &str| {
            clap::Command::new("add")
                .arg(clap::Arg::new("name").long("name"))
                .get_matches_from(["add", "--name", name])
        };
        let refused = requested_name(&matches("MY-SERVER")).unwrap_err();
        assert_eq!(
            refused.to_string(),
            "invalid Machine Name: a 1-63 character lowercase DNS label"
        );
        assert_eq!(refused.report().code, RpcErrorCode::InvalidArgument);
        assert_eq!(refused.hints(), [Hint::Closest("my-server".into())]);
        let refused = requested_name(&matches("___")).unwrap_err();
        assert!(refused.hints().is_empty());
        assert_eq!(
            requested_name(&matches("web-1")).unwrap().unwrap().as_str(),
            "web-1"
        );
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod setup_tests;
