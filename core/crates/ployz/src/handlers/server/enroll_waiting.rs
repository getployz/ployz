//! Select waiting hosts by name, review Cloud's plan, and follow its enrollment attempts.

use std::{collections::BTreeMap, io::IsTerminal, time::Duration};

use clap::{ArgMatches, Command};
use globset::GlobMatcher;
use ployz_core::{RpcErrorCode, StorageChoice};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::waiting::{self, Waiting};
use crate::cli::{base, env, positional, switch, value};
use crate::cloud_account::{self, Credential};
use crate::handlers::{Error, leaf_matches, runtime, typed_with};
use crate::ui::{self, Cell, Hint, Table, printable};

pub(crate) fn command(command: Command) -> Command {
    base(command, "Enroll servers waiting to join")
        .long_about("Select waiting servers by name or a quoted glob. Cloud plans the founder, names and storage. Review the plan and confirm, or pass --yes. --first enrolls only the oldest match; --first NAME picks the founder from the selection. --status follows attempts; alone it resumes following open attempts. Ctrl-C stops following without cancelling enrollment.")
        .arg(positional("pattern", false).conflicts_with("all"))
        .arg(switch("all", None).help("Select every waiting server"))
        .arg(value("first", None).num_args(0..=1).default_missing_value("").value_name("NAME").help("Enroll only the oldest match, or pick NAME as founder"))
        .arg(value("name", None).value_name("PATTERN").help("Server name; use {n} to number several servers"))
        .arg(switch("status", None).help("Follow enrollment stages, or resume open attempts"))
        .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM).help("Start after printing the plan without asking"))
        .arg(value("storage", None).value_parser(clap::value_parser!(StorageChoice)).help("zfs (default) or none for Docker volumes"))
        .arg(switch("no-spare-disk", None).help("Keep ZFS on the main disk"))
        .arg(switch("no-fallback", None).help("Skip hosts that cannot use ZFS"))
}

#[derive(Deserialize, Serialize)]
struct Plan {
    plan: Vec<Planned>,
    skipped: Vec<Skipped>,
    spread: Spread,
    #[serde(default)]
    waiting_servers: Vec<Value>,
}

#[derive(Deserialize, Serialize)]
struct Planned {
    waiting_server_id: String,
    hostname: String,
    role: Role,
    name: String,
    storage: Storage,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Role {
    Founder,
    Joiner,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Storage {
    None,
    Zfs { device: Option<String> },
}

impl Storage {
    fn label(&self) -> String {
        match self {
            Self::None => "Docker volumes".into(),
            Self::Zfs { device: None } => "ZFS on main disk".into(),
            Self::Zfs {
                device: Some(device),
            } => format!("ZFS on {}", printable(device)),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Skipped {
    id: String,
    reason: String,
}

#[derive(Deserialize, Serialize)]
struct Spread {
    providers: usize,
    networks: usize,
}

#[derive(Deserialize, Serialize)]
struct Started {
    enrollments: Vec<Value>,
    skipped: Vec<Skipped>,
    spread: Spread,
}

#[derive(Deserialize)]
struct Attempts {
    enrollments: Vec<Value>,
}

#[derive(Deserialize)]
struct Attempt {
    id: String,
    waiting_server_id: String,
    name: String,
    role: Role,
    state: State,
    stage: Option<String>,
    #[serde(default)]
    queued: bool,
    ahead: Option<u64>,
    failure: Option<Failure>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum State {
    AwaitingFounder,
    Instructed,
    Running,
    Joined,
    Failed,
    Cancelled,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct Failure {
    message: String,
}

impl Attempt {
    fn ended(&self) -> bool {
        matches!(self.state, State::Joined | State::Failed | State::Cancelled)
    }

    fn label(&self) -> String {
        match self.state {
            State::Joined => return "Joined".into(),
            State::Failed => {
                return self.failure.as_ref().map_or_else(
                    || "Failed".into(),
                    |failure| format!("Failed · {}", printable(&failure.message)),
                );
            }
            State::Cancelled => return "Cancelled".into(),
            State::AwaitingFounder => return "Waiting for the first server".into(),
            State::Unknown => return "Unknown enrollment state".into(),
            State::Instructed if self.queued => return "Queued · Preparing storage".into(),
            State::Instructed | State::Running => {}
        }
        if let Some(ahead) = self.ahead {
            return format!(
                "In line · {ahead} server{} ahead",
                if ahead == 1 { "" } else { "s" }
            );
        }
        match self.stage.as_deref() {
            Some("installing") => "Installing Ployz".into(),
            Some("storage") => "Preparing storage".into(),
            Some("joining") => "Joining".into(),
            Some("publishing") => "Publishing".into(),
            Some("queued") => "Enrolling · Join position unknown".into(),
            Some("enrolling") | None if matches!(self.role, Role::Joiner) => {
                "Enrolling · Join position unknown".into()
            }
            Some("enrolling") => "Enrolling".into(),
            None => "Instructed".into(),
            Some(stage) => printable(stage),
        }
    }
}

struct Selection {
    literal: Option<String>,
    glob: Option<GlobMatcher>,
}

impl Selection {
    fn parse(pattern: Option<&str>) -> Result<Self, Error> {
        let glob = pattern
            .map(|pattern| globset::Glob::new(pattern).map(|glob| glob.compile_matcher()))
            .transpose()
            .map_err(|error| {
                Error::caused(
                    RpcErrorCode::InvalidArgument,
                    "The server name glob is invalid.",
                    error,
                )
            })?;
        Ok(Self {
            literal: pattern.map(str::to_owned),
            glob,
        })
    }

    fn matches(&self, name: &str) -> bool {
        self.literal.as_deref() == Some(name)
            || self.glob.as_ref().is_none_or(|glob| glob.is_match(name))
    }
}

fn selected<'a>(
    listed: &'a [Waiting],
    selection: &Selection,
    first: Option<&str>,
) -> Vec<&'a Waiting> {
    let mut selected: Vec<_> = listed
        .iter()
        .filter(|host| {
            selection.matches(&host.row.name)
                || host
                    .raw
                    .pointer("/report/hostname")
                    .and_then(Value::as_str)
                    .is_some_and(|name| selection.matches(name))
        })
        .collect();
    selected.sort_by(|left, right| {
        left.raw
            .get("first_seen_at")
            .and_then(Value::as_str)
            .cmp(&right.raw.get("first_seen_at").and_then(Value::as_str))
            .then_with(|| left.row.id.cmp(&right.row.id))
    });
    if first == Some("") {
        selected.truncate(1);
    }
    selected
}

fn request(matches: &ArgMatches, selected: &[&Waiting]) -> Result<Value, Error> {
    let first = match matches.get_one::<String>("first").map(String::as_str) {
        Some(name) if !name.is_empty() => {
            let founders: Vec<_> = selected
                .iter()
                .filter(|host| {
                    host.row.name == name
                        || host.raw.pointer("/report/hostname").and_then(Value::as_str)
                            == Some(name)
                })
                .collect();
            match founders.as_slice() {
                [host] => &host.row.id,
                [] => {
                    return Err(Error::usage(
                        "The first server must match the selection by name.",
                    ));
                }
                _ => {
                    return Err(Error::coded(
                        RpcErrorCode::Ambiguous,
                        "More than one selected host has the first server's name. Narrow the selection.",
                    ));
                }
            }
        }
        _ => {
            &selected
                .first()
                .ok_or_else(|| Error::not_found("No waiting servers match."))?
                .row
                .id
        }
    };
    let names = match matches.get_one::<String>("name") {
        Some(pattern) => json!({ "kind": "pattern", "pattern": pattern }),
        None => json!({ "kind": "hostnames" }),
    };
    let storage = match matches
        .get_one::<StorageChoice>("storage")
        .copied()
        .unwrap_or(StorageChoice::Zfs)
    {
        StorageChoice::None => json!({ "kind": "none" }),
        StorageChoice::Zfs => {
            json!({ "kind": "zfs", "use_spare_disk": !matches.get_flag("no-spare-disk"), "fallback_to_docker": !matches.get_flag("no-fallback") })
        }
    };
    Ok(
        json!({ "ids": selected.iter().map(|host| &host.row.id).collect::<Vec<_>>(), "first": first, "names": names, "storage": storage, "runs": { "services": true, "ingress": true, "builds": true } }),
    )
}

fn plan_table(plan: &Plan, selected: &[&Waiting]) -> Table {
    let mut table = Table::new(
        ["HOST", "SOURCE IP", "ROLE", "NAME", "STORAGE / SKIP"],
        "No servers can enroll.",
    );
    for row in &plan.plan {
        let ip = selected
            .iter()
            .find(|host| host.row.id == row.waiting_server_id)
            .map_or("Unknown", |host| host.row.ip());
        table.row([
            Cell::from(printable(&row.hostname)),
            Cell::from(printable(ip)),
            Cell::from(match row.role {
                Role::Founder => "Founder",
                Role::Joiner => "Joiner",
            }),
            Cell::from(printable(&row.name)),
            Cell::from(row.storage.label()),
        ]);
    }
    for skip in &plan.skipped {
        let host = selected.iter().find(|host| host.row.id == skip.id);
        table.row([
            Cell::from(
                host.map_or_else(|| "Unknown host".into(), |host| printable(&host.row.name)),
            ),
            Cell::from(host.map_or("Unknown", |host| host.row.ip())),
            Cell::from("Skipped"),
            Cell::from("-"),
            Cell::from(printable(&skip.reason)),
        ]);
    }
    table
}

fn review(plan: &Plan, selected: &[&Waiting], yes: bool, all: bool) -> Result<(), Error> {
    if !ui::json() {
        let _ = plan_table(plan, selected)
            .write(&mut anstream::stderr(), std::io::stderr().is_terminal());
        ui::note(format_args!(
            "{} to enroll · {} skipped · {} providers · {} networks",
            plan.plan.len(),
            plan.skipped.len(),
            plan.spread.providers,
            plan.spread.networks
        ));
        ui::note("Runs on every planned server · Services, ingress and builds");
        if all {
            let mut providers = BTreeMap::new();
            for host in selected {
                *providers
                    .entry(
                        host.raw
                            .pointer("/report/provider")
                            .and_then(Value::as_str)
                            .unwrap_or("Unknown"),
                    )
                    .or_insert(0usize) += 1;
            }
            for (provider, count) in providers {
                ui::note(format_args!("{} · {count} selected", printable(provider)));
            }
        }
    }
    if plan.plan.is_empty() {
        return Ok(());
    }
    if !yes {
        ui::confirm(
            &format!("Enroll {} waiting servers?", plan.plan.len()),
            || {
                Error::detailed(RpcErrorCode::ConfirmationRequired, "Enrollment needs confirmation. Nothing enrolled.", json!({ "plan": plan.plan, "skipped": plan.skipped, "spread": plan.spread, "waiting_servers": plan.waiting_servers, "runs": { "services": true, "ingress": true, "builds": true } })).hint(Hint::Retry(typed_with(&["--yes"])))
            },
            "Nothing enrolled.",
        )?;
    }
    Ok(())
}

async fn attempts(credential: &Credential) -> Result<Vec<Value>, Error> {
    let reply: Attempts =
        cloud_account::refusable(credential, Method::GET, "enrollments", None).await?;
    Ok(reply.enrollments)
}

fn finish(rows: &[Value], plan: Option<&Plan>, interrupted: bool) -> Result<(), Error> {
    ui::finish(
        &json!({ "enrollments": rows, "plan": plan.map(|plan| &plan.plan), "skipped": plan.map(|plan| &plan.skipped), "spread": plan.map(|plan| &plan.spread), "runs": plan.map(|_| json!({ "services": true, "ingress": true, "builds": true })), "waiting_servers": plan.map(|plan| &plan.waiting_servers), "interrupted": interrupted }),
        || {},
    )
}

async fn follow(
    credential: &Credential,
    mut rows: Vec<Value>,
    plan: Option<&Plan>,
) -> Result<(), Error> {
    let mut shown = BTreeMap::new();
    let stop = crate::cancellation::on_ctrl_c();
    loop {
        let parsed: Vec<Attempt> = rows
            .iter()
            .cloned()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()?;
        for row in &parsed {
            let label = row.label();
            if shown.get(&row.id) != Some(&label) {
                ui::note(format_args!("{} · {label}", printable(&row.name)));
                shown.insert(row.id.clone(), label);
            }
        }
        if parsed.iter().all(Attempt::ended) {
            finish(&rows, plan, false)?;
            return if parsed.iter().any(|row| row.state != State::Joined)
                || plan.is_some_and(|plan| !plan.skipped.is_empty())
            {
                Err(Error::partial())
            } else {
                Ok(())
            };
        }
        let refresh = async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            attempts(credential).await
        };
        let latest = tokio::select! {
            () = stop.cancelled() => {
                ui::note("Stopped following. Enrollment continues in Cloud. Resume with ployz server enroll --status.");
                finish(&rows, plan, true)?;
                return Err(Error::cancelled());
            }
            latest = refresh => latest,
        };
        let latest = match latest {
            Ok(latest) => latest,
            Err(error) => {
                ui::warn_cause(
                    "Could not refresh enrollment. Resume with ployz server enroll --status",
                    &error,
                );
                finish(&rows, plan, false)?;
                return Err(Error::partial());
            }
        };
        for current in &mut rows {
            if let Some(updated) = latest.iter().find(|row| row.get("id") == current.get("id")) {
                *current = updated.clone();
            } else {
                ui::warn("An enrollment is no longer listed. Inspect it in Ployz Cloud.");
                finish(&rows, plan, false)?;
                return Err(Error::partial());
            }
        }
    }
}

pub(crate) fn enroll(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let pattern = matches.get_one::<String>("pattern").map(String::as_str);
    let status = matches.get_flag("status");
    if pattern.is_none()
        && !matches.get_flag("all")
        && !status
        && matches.get_one::<String>("first").is_none()
    {
        return Err(Error::usage(
            "Select a server name or glob, --all, --first, or resume with --status.",
        ));
    }
    let selection = Selection::parse(pattern)?;
    let runtime = runtime()?;
    let credential = waiting::credential(&runtime, matches)?;
    let listed = runtime.block_on(waiting::fetch(&credential))?;
    let selected = selected(
        &listed,
        &selection,
        matches.get_one::<String>("first").map(String::as_str),
    );
    if status {
        let existing = runtime.block_on(attempts(&credential))?;
        let mut open = Vec::new();
        for raw in existing {
            let row: Attempt = serde_json::from_value(raw.clone())?;
            if !row.ended()
                && (selection.matches(&row.name)
                    || selected
                        .iter()
                        .any(|host| host.row.id == row.waiting_server_id))
            {
                open.push(raw);
            }
        }
        if !open.is_empty() {
            return runtime.block_on(follow(&credential, open, None));
        }
        if pattern.is_none()
            && !matches.get_flag("all")
            && matches.get_one::<String>("first").is_none()
        {
            return ui::done(&json!({ "enrollments": [] }), "No open enrollments.");
        }
    }
    if selected.is_empty() {
        return Err(Error::not_found("No waiting servers match.")
            .hint(Hint::Next("ployz server ls --waiting".into())));
    }
    if selected.len() > 1000 {
        return Err(Error::usage(
            "Enroll at most 1000 waiting servers at once. Narrow the name glob.",
        ));
    }
    let mut body = request(matches, &selected)?;
    let mut plan: Plan = runtime.block_on(cloud_account::refusable(
        &credential,
        Method::POST,
        "enrollments/plan",
        Some(&body),
    ))?;
    plan.waiting_servers = selected.iter().map(|host| host.raw.clone()).collect();
    review(
        &plan,
        &selected,
        matches.get_flag("yes"),
        matches.get_flag("all"),
    )?;
    if plan.plan.is_empty() {
        finish(&[], Some(&plan), false)?;
        return Err(Error::partial());
    }
    *body
        .get_mut("ids")
        .expect("enrollment requests contain ids") = json!(
        plan.plan
            .iter()
            .map(|row| &row.waiting_server_id)
            .collect::<Vec<_>>()
    );
    let Started {
        enrollments,
        skipped,
        spread,
    } = runtime
        .block_on(cloud_account::refusable(
            &credential,
            Method::POST,
            "enrollments",
            Some(&body),
        ))
        .map_err(|error| {
            Error::from(error).hint(Hint::Inspect("ployz server enroll --status".into()))
        })?;
    for skip in &skipped {
        let name = selected
            .iter()
            .find(|host| host.row.id == skip.id)
            .map_or("Unknown host", |host| host.row.name.as_str());
        ui::note(format_args!(
            "{} · Skipped · {}",
            printable(name),
            printable(&skip.reason)
        ));
    }
    plan.skipped.extend(skipped);
    plan.spread = spread;
    if status {
        return runtime.block_on(follow(&credential, enrollments, Some(&plan)));
    }
    ui::finish(
        &json!({ "enrollments": enrollments, "plan": plan.plan, "skipped": plan.skipped, "spread": plan.spread, "waiting_servers": plan.waiting_servers, "runs": { "services": true, "ingress": true, "builds": true } }),
        || {
            for raw in &enrollments {
                let name = raw
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown server");
                ui::stream(format_args!("{} · Enrollment started", printable(name)));
            }
            ui::hint(&Hint::Next("ployz server enroll --status".into()));
        },
    )?;
    if plan.skipped.is_empty() {
        Ok(())
    } else {
        Err(Error::partial())
    }
}

#[cfg(test)]
#[path = "enroll_waiting_tests.rs"]
mod tests;
