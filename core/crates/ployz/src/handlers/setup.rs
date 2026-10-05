//! `ployz setup agent`: a Ployz skill for coding agents, versioned with this CLI,
//! and the health line `ployz --help` prints about it.
//!
//! The skill is generated from the command tree and the settings catalog. Each copy
//! records a digest of what was written, so a copy that no longer matches its digest
//! has local edits and is kept unless `--force`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::{ArgMatches, Command};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{Error, leaf_matches};
use crate::cli::switch;
use crate::output::say;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DIGEST_KEY: &str = "ployz-digest: ";

/// Where agents find the catalog, stated as a fact in `--help` and in the skill.
pub(crate) const SIGNPOST: &str = "Settings catalog: `ployz schema --json` lists every command and Setting; `ployz explain SERVICE.SETTING` describes one Setting.";

/// Agent tools that don't read `~/.agents/skills`, by the directory that marks them installed.
const TOOL_DIRS: [&str; 1] = [".claude"];

pub(crate) fn command() -> Command {
    Command::new("setup")
        .about("Set up tooling around the CLI")
        .arg_required_else_help(true)
        .subcommand(
            Command::new("agent")
                .about("Install the Ployz skill for coding agents in ~/.agents/skills and detected agent tools")
                .arg(switch("force", None).help("Replace copies that have local edits")),
        )
}

pub(super) fn handler(path: &str) -> Option<super::Handler> {
    match path {
        "agent" => Some(agent),
        _ => None,
    }
}

/// One copy of the skill, as found on disk.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Found {
    Missing,
    Current,
    /// Unedited, written by another CLI version.
    Stale(String),
    Edited,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Installed,
    Updated,
    Current,
    /// Local edits, left in place.
    Kept,
    Replaced,
}

#[derive(Serialize)]
struct Location {
    path: PathBuf,
    outcome: Outcome,
}

#[derive(Serialize)]
struct SetUp {
    version: &'static str,
    locations: Vec<Location>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

fn agent(root: &ArgMatches) -> Result<(), Error> {
    let force = leaf_matches(root).get_flag("force");
    let skill = render();
    let mut locations = Vec::new();
    for dir in targets(&home()) {
        let file = dir.join("SKILL.md");
        let outcome = match (inspect(&file, &skill), force) {
            (Found::Current, _) => Outcome::Current,
            (Found::Edited, false) => Outcome::Kept,
            (found, _) => {
                std::fs::create_dir_all(&dir)?;
                std::fs::write(&file, &skill)?;
                match found {
                    Found::Missing => Outcome::Installed,
                    Found::Edited => Outcome::Replaced,
                    Found::Current | Found::Stale(_) => Outcome::Updated,
                }
            }
        };
        locations.push(Location {
            path: file,
            outcome,
        });
    }
    let kept = locations
        .iter()
        .any(|location| matches!(location.outcome, Outcome::Kept));
    let result = SetUp {
        version: VERSION,
        next: kept.then(|| "ployz setup agent --force".to_owned()),
        locations,
    };
    crate::output::finish(&result, || {
        for location in &result.locations {
            let outcome = match location.outcome {
                Outcome::Installed => "installed",
                Outcome::Updated => "updated",
                Outcome::Current => "already current",
                Outcome::Kept => "kept: it has local edits; --force replaces it",
                Outcome::Replaced => "replaced",
            };
            say!("{}: {outcome}", location.path.display());
        }
    })
}

/// The `--help` footer: the catalog signpost and the skill's health, as facts.
pub(crate) fn help_footer() -> String {
    format!("{SIGNPOST}\n{}", health(&home()))
}

fn health(home: &Path) -> String {
    let skill = render();
    let copies: Vec<(PathBuf, Found)> = targets(home)
        .into_iter()
        .map(|dir| {
            let file = dir.join("SKILL.md");
            let copy = inspect(&file, &skill);
            (file, copy)
        })
        .collect();
    let first = |wanted: fn(&Found) -> bool| {
        copies
            .iter()
            .find(|(_, copy)| wanted(copy))
            .map(|(file, copy)| (file.display(), copy))
    };
    if copies.iter().all(|(_, copy)| *copy == Found::Missing) {
        return "Agent tooling: no Ployz skill is installed; `ployz setup agent` installs it."
            .into();
    }
    if let Some((file, _)) = first(|copy| *copy == Found::Missing) {
        return format!(
            "Agent tooling: the Ployz skill is missing from {file}; `ployz setup agent` installs it."
        );
    }
    if let Some((file, Found::Stale(version))) = first(|copy| matches!(copy, Found::Stale(_))) {
        return format!(
            "Agent tooling: the Ployz skill at {file} is from ployz {version}; `ployz setup agent` updates it to {VERSION}."
        );
    }
    if let Some((file, _)) = first(|copy| *copy == Found::Edited) {
        return format!(
            "Agent tooling: the Ployz skill at {file} has local edits, which `ployz setup agent` keeps and `--force` replaces."
        );
    }
    format!("Agent tooling: the Ployz skill is current ({VERSION}).")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("~"), PathBuf::from)
}

/// `~/.agents/skills/ployz`, plus the skill directory of each detected agent tool.
fn targets(home: &Path) -> Vec<PathBuf> {
    std::iter::once(home.join(".agents"))
        .chain(
            TOOL_DIRS
                .iter()
                .map(|tool| home.join(tool))
                .filter(|tool| tool.is_dir()),
        )
        .map(|tool| tool.join("skills").join("ployz"))
        .collect()
}

fn inspect(file: &Path, skill: &str) -> Found {
    let Ok(found) = std::fs::read_to_string(file) else {
        return Found::Missing;
    };
    if found == skill {
        return Found::Current;
    }
    let recorded = found
        .lines()
        .find_map(|line| line.trim().strip_prefix(DIGEST_KEY))
        .map(|digest| digest.trim_matches('"'));
    let unedited = recorded.is_some_and(|digest| {
        digest_of(&found.replacen(
            &format!("{DIGEST_KEY}\"{digest}\""),
            &format!("{DIGEST_KEY}\"\""),
            1,
        )) == digest
    });
    if !unedited {
        return Found::Edited;
    }
    let version = found
        .lines()
        .find_map(|line| line.trim().strip_prefix("ployz-version: "))
        .unwrap_or("unknown")
        .trim_matches('"');
    Found::Stale(version.to_owned())
}

fn digest_of(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// The skill for this CLI, with the digest of its own text.
fn render() -> String {
    let unsigned = render_with("");
    render_with(&digest_of(&unsigned))
}

fn render_with(digest: &str) -> String {
    let mut out = format!(
        "---
name: ployz
description: Deploy and operate apps on Ployz servers with the `ployz` CLI — sign in, add servers, add Services and change their Settings, review, publish and deploy, read logs. Relevant whenever a task mentions Ployz or the `ployz` command.
metadata:
  ployz-version: \"{VERSION}\"
  {DIGEST_KEY}\"{digest}\"
---

# Ployz CLI {VERSION}

- {SIGNPOST}
- `ployz get SERVICE --json` shows every Setting of a Service, including unset ones with their defaults.
- `ployz set SERVICE.SETTING=VALUE` stages an edit; `ployz diff` shows what is staged and `ployz publish` saves it.
- Reach another Service by reference, so Ployz knows the two are linked: `ployz set 'web.env.API_URL=http://${{{{ api.PLOYZ_PRIVATE_DOMAIN }}}}:${{{{ api.PORT }}}}'`. Single-quote it: in double quotes the shell rejects `${{{{`. A typed `api.internal` comes back in `typed_addresses` with the reference to set instead.
- `ployz diff --json` carries a `version`; `ployz deploy --expect-version VERSION` deploys exactly that review or refuses with `conflict`.
- A destructive command without its confirmation fails with `confirmation_required`, naming what goes; `details.retry` is the exact command that confirms it.
- With `--json`, stdout carries one JSON object and nothing prompts. A failure is `{{\"error\": {{code, message, cause, details}}}}`. `cause` lists every underlying error, outermost first, and is empty when there are none; human output shows only the last, deepest one. Each of these is present only when it applies: `details.next`, the command to run next; `details.retry`, the corrected command; `details.did_you_mean`, the closest valid name; `details.valid_children`, every valid name.
- Exit codes: 0 success, 1 failure, 2 usage (a command-line mistake, a missing confirmation or an ambiguous name), 3 partial.
- `PLOYZ_TOKEN` authenticates without `ployz login`.
- `ployz COMMAND --help` describes each command's flags.

## Commands
"
    );
    for entry in super::catalog::commands() {
        let _ = writeln!(out, "- `ployz {}` — {}", entry.command, entry.about);
    }
    out.push_str("\n## Settings\n\nA path is SERVICE.SETTING, for example `web.replicas`.\n\n");
    let catalog = ployz_store::catalog::schema(None).expect("the whole catalog renders");
    let definitions = catalog
        .get("$defs")
        .and_then(Value::as_object)
        .into_iter()
        .flatten();
    for (_, definition) in definitions {
        let properties = definition
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten();
        for (name, setting) in properties {
            let text = |key: &str| setting.get(key).and_then(Value::as_str).unwrap_or_default();
            let _ = writeln!(
                out,
                "- `{name}` ({}, {}) — {}",
                text("type"),
                text("x-ployz-apply"),
                text("description"),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_lists_every_command_and_setting_and_states_the_catalog() {
        let skill = render();
        assert!(skill.starts_with("---\nname: ployz\n"));
        assert!(skill.contains(SIGNPOST));
        assert!(skill.contains("- `ployz setup agent` — "));
        assert!(skill.contains("- `ployz service add` — "));
        assert!(skill.contains("- `replicas` (integer, staged) — "));
        assert!(!skill.contains("ployz-digest: \"\""));
        assert_eq!(render(), skill, "deterministic");
    }

    #[test]
    fn health_follows_each_copy_from_missing_to_edited() {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join(".agents/skills/ployz/SKILL.md");
        assert!(health(home.path()).contains("no Ployz skill is installed"));

        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, render()).unwrap();
        assert!(health(home.path()).contains("is current"));

        std::fs::create_dir(home.path().join(".claude")).unwrap();
        assert!(health(home.path()).contains("missing from"));
        std::fs::remove_dir(home.path().join(".claude")).unwrap();

        let older = render_with("").replace(VERSION, "0.0.1");
        std::fs::write(
            &file,
            render_with(&digest_of(&older)).replace(VERSION, "0.0.1"),
        )
        .unwrap();
        assert_eq!(inspect(&file, &render()), Found::Stale("0.0.1".into()));
        assert!(health(home.path()).contains("is from ployz 0.0.1"));

        std::fs::write(&file, render() + "\nMy notes.\n").unwrap();
        assert_eq!(inspect(&file, &render()), Found::Edited);
        assert!(health(home.path()).contains("has local edits"));

        std::fs::write(&file, "---\nname: ployz\n---\nmine\n").unwrap();
        assert_eq!(inspect(&file, &render()), Found::Edited);
    }
}
