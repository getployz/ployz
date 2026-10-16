use clap::{ArgMatches, Command};
use ployz_core::{RpcError, RpcErrorCode};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::account::in_cloud;
use super::super::{Error, leaf_matches};
use crate::cli::{positional, switch, value};
use crate::cloud_account::{self, Credential};
use crate::cloud_login::{DEFAULT_CLOUD, LoginError};
use crate::ui::{self, Cell, Hint, Table, Tone};

const INSTALLER_URL: &str = "https://ployz.sh";

pub(crate) fn group(command: Command) -> Command {
    command.about("Make, list and revoke Fleet Tokens that let hosts ask to join")
}

pub(crate) fn create_command(command: Command) -> Command {
    command
        .about("Make a Fleet Token; its secret is shown once")
        .long_about(
            "Make a Fleet Token: a short-lived secret any host can carry to ask to join this \
             Organization. A host that carries it waits for you to accept it, unless --auto \
             lets up to --max hosts in on their own. When it expires or you revoke it, new \
             hosts stop joining; Servers already in stay.",
        )
        .after_help(concat!(
            "Examples:\n",
            "  ployz server token create\n",
            "  ployz server token create --expires 7d\n",
            "  ployz server token create --name rack-b --auto --max 5",
        ))
        .arg(
            value("name", None)
                .value_name("NAME")
                .help("Lowercase letters, digits and hyphens; one is picked when omitted"),
        )
        .arg(
            value("expires", None)
                .value_name("EXPIRY")
                .value_parser(["1d", "7d", "30d"])
                .default_value("1d")
                .help("How long new hosts can use it"),
        )
        .arg(
            switch("auto", None)
                .requires("max")
                .help("Let hosts join without waiting for you"),
        )
        .arg(
            value("max", None)
                .value_name("N")
                .value_parser(clap::value_parser!(u16).range(1..=1000))
                .requires("auto")
                .help("How many hosts --auto lets in"),
        )
}

pub(crate) fn ls_command(command: Command) -> Command {
    command.about("List the Organization's Fleet Tokens")
}

pub(crate) fn revoke_command(command: Command) -> Command {
    command
        .about("Revoke a Fleet Token and drop the hosts waiting on it")
        .long_about(
            "Revoke a Fleet Token at once. Hosts still waiting on it are dropped; Servers \
             already in stay.",
        )
        .arg(positional("name", true).help("The token's name, from ployz server token ls"))
}

#[derive(Deserialize, Serialize)]
struct Created {
    name: String,
    token: String,
    expires_at: String,
}

#[derive(Deserialize, Serialize)]
struct Listed {
    name: String,
    expires_at: String,
    expired: bool,
    auto_enroll_max: Option<u16>,
    last_used_at: Option<String>,
    created_at: String,
}

pub(crate) fn create(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let expires = matches
        .get_one::<String>("expires")
        .expect("expires has a default");
    let mut body = serde_json::Map::new();
    body.insert("expires_in".into(), json!(expires));
    if let Some(name) = matches.get_one::<String>("name") {
        body.insert("name".into(), json!(name));
    }
    if let Some(max) = matches.get_one::<u16>("max") {
        body.insert("auto_enroll_max".into(), json!(max));
    }
    let body = serde_json::Value::Object(body);
    let (cloud, reply) = in_cloud(root, async |_, credential| {
        let reply =
            ask::<CreatedReply>(credential, Method::POST, "fleet-tokens", Some(&body)).await?;
        Ok((credential.cloud().to_owned(), reply))
    })?;
    let created = reply?.fleet_token;
    let install = install_line(&cloud, &created.token);
    let lasts = lasts(expires);
    ui::finish(
        &json!({ "fleet_token": created, "install": install }),
        || {
            ui::note(format_args!("Created {}", created.name));
            if matches.get_flag("auto") {
                ui::warn("Anyone with this token can add servers. Keep it secret.");
            }
            ui::stream(&created.token);
            ui::note(format_args!("Run on each host:\n  {install}"));
            ui::note(format_args!("Shown once. Expires in {lasts}."));
        },
    )
}

pub(crate) fn ls(root: &ArgMatches) -> Result<(), Error> {
    let reply = in_cloud(root, async |_, credential| {
        ask::<ListReply>(credential, Method::GET, "fleet-tokens", None).await
    })?;
    let tokens = reply?.fleet_tokens;
    let mut table = Table::new(
        ["NAME", "AUTO", "LAST USED", "EXPIRES", "STATE"],
        "No Fleet Tokens. Make one with ployz server token create.",
    );
    for token in &tokens {
        table.row([
            Cell::from(&token.name),
            Cell::from(
                token
                    .auto_enroll_max
                    .map_or(String::new(), |max| format!("up to {max}")),
            ),
            Cell::from(token.last_used_at.as_deref().unwrap_or("never")),
            Cell::from(&token.expires_at),
            if token.expired {
                Cell::status("expired", Tone::Bad)
            } else {
                Cell::status("active", Tone::Good)
            },
        ]);
    }
    ui::list(&tokens, &table)
}

pub(crate) fn revoke(root: &ArgMatches) -> Result<(), Error> {
    let name = leaf_matches(root)
        .get_one::<String>("name")
        .expect("name is required");
    let mut path = reqwest::Url::parse("ployz:/fleet-tokens").expect("a constant URL parses");
    path.path_segments_mut()
        .expect("a path URL has segments")
        .push(name);
    let reply = in_cloud(root, async |_, credential| {
        ask::<RevokedReply>(
            credential,
            Method::DELETE,
            path.path().trim_start_matches('/'),
            None,
        )
        .await
    })?;
    let removed = match reply {
        Ok(reply) => reply.waiting_removed,
        Err(refused) if refused.code == RpcErrorCode::NotFound => {
            return Err(Error::coded(
                RpcErrorCode::NotFound,
                format!("No Fleet Token named {name}."),
            )
            .hint(Hint::Next("ployz server token ls".into())));
        }
        Err(refused) => return Err(refused.into()),
    };
    ui::finish(&json!({ "name": name, "waiting_removed": removed }), || {
        let servers = if removed == 1 { "server" } else { "servers" };
        ui::stream(format_args!(
            "Revoked {name}. {removed} waiting {servers} removed."
        ));
    })
}

#[derive(Deserialize)]
struct CreatedReply {
    fleet_token: Created,
}

#[derive(Deserialize)]
struct ListReply {
    fleet_tokens: Vec<Listed>,
}

#[derive(Deserialize)]
struct RevokedReply {
    waiting_removed: u64,
}

/// Cloud's reply, or the refusal it sent as it sent it.
async fn ask<T: DeserializeOwned>(
    credential: &Credential,
    method: Method,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<Result<T, RpcError>, LoginError> {
    match cloud_account::call(credential, method, path, body).await {
        Err(LoginError::Status { body, .. })
            if let Some(refused) = cloud_account::refusal(body.as_bytes())
                .or_else(|| cloud_account::public_error(body.as_bytes())) =>
        {
            Ok(Err(refused))
        }
        reply => reply.map(Ok),
    }
}

/// How long an expiry lasts, as the person reads it.
fn lasts(expires: &str) -> &str {
    match expires {
        "1d" => "24h",
        other => other,
    }
}

/// The line a host runs to install Ployz and ask to join with `token`.
fn install_line(cloud: &str, token: &str) -> String {
    let cloud_env = if cloud == crate::cloud_enroll::cloud_origin(DEFAULT_CLOUD) {
        String::new()
    } else {
        format!("PLOYZ_CLOUD_URL='{cloud}' ")
    };
    format!("curl -fsSL {INSTALLER_URL} | sudo {cloud_env}PLOYZ_ENROLL_TOKEN={token} sh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_line_carries_the_token_and_names_a_non_default_cloud() {
        assert_eq!(
            install_line("https://ployz.dev", "pflt_abc"),
            "curl -fsSL https://ployz.sh | sudo PLOYZ_ENROLL_TOKEN=pflt_abc sh"
        );
        assert_eq!(
            install_line("http://localhost:3000", "pflt_abc"),
            "curl -fsSL https://ployz.sh | sudo PLOYZ_CLOUD_URL='http://localhost:3000' PLOYZ_ENROLL_TOKEN=pflt_abc sh"
        );
    }

    #[test]
    fn a_day_reads_as_24_hours() {
        assert_eq!(lasts("1d"), "24h");
        assert_eq!(lasts("7d"), "7d");
        assert_eq!(lasts("30d"), "30d");
    }
}
