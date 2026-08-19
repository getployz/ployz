use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use clap::{ArgAction, Command};
use clap_complete::{Shell, generate};

#[derive(Debug, Eq, PartialEq)]
struct Flag {
    short: Option<char>,
    default: Option<String>,
    env: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
struct Shape {
    aliases: BTreeSet<String>,
    flags: BTreeMap<String, Flag>,
    positionals: Vec<Positional>,
}

#[derive(Debug, Eq, PartialEq)]
struct Positional {
    required: bool,
    multiple: bool,
}

#[test]
fn clap_tree_matches_all_frozen_command_pages_and_declared_deviations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let deviations = deviations(&fs::read_to_string(root.join("CLI_DEVIATIONS.md")).unwrap());
    let mut expected = BTreeMap::new();
    for entry in fs::read_dir(root.join("evidence/upstream/cli-reference")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension() != Some(OsStr::new("md")) {
            continue;
        }
        let markdown = fs::read_to_string(path).unwrap();
        let (command_path, flags, positionals) = reference_shape(&markdown, &deviations);
        expected.insert(
            command_path.clone(),
            Shape {
                aliases: reference_aliases(&command_path),
                flags,
                positionals,
            },
        );
    }
    assert_eq!(
        expected.len(),
        58,
        "the complete frozen oracle must be read"
    );

    let mut command = ployz::cli::command();
    command.build();
    let mut actual = BTreeMap::new();
    collect_clap_shapes(&command, "", &mut actual);
    actual.remove("ployz completion").unwrap();
    if deviations.contains("cloud-enroll") {
        actual.remove("ployz cloud").unwrap();
        actual.remove("ployz cloud enroll").unwrap();
    }
    if deviations.contains("project-list-and-remove") {
        actual.remove("ployz project").unwrap();
        actual.remove("ployz project ls").unwrap();
        actual.remove("ployz project rm").unwrap();
    }
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (path, expected_shape) in expected {
        assert_eq!(
            actual.get(&path),
            Some(&expected_shape),
            "shape mismatch for {path}"
        );
    }

    assert_eq!(
        deviations,
        BTreeSet::from([
            "cloud-enroll".to_owned(),
            "compose-diagnostics-source".to_owned(),
            "compose-plugin-scope".to_owned(),
            "compose-prerequisite-errors".to_owned(),
            "deploy-intent-flags".to_owned(),
            "direct-push-reference-boundary".to_owned(),
            "fixed-wireguard-port".to_owned(),
            "images-json-output".to_owned(),
            "local-machine-init-stub".to_owned(),
            "machine-remove-named-data-loss".to_owned(),
            "native-completion".to_owned(),
            "no-nightly-daemon-channel".to_owned(),
            "plain-caddy-config".to_owned(),
            "product-identity".to_owned(),
            "project-list-and-remove".to_owned(),
            "project-name-no-short-flag".to_owned(),
            "qualified-service-container-target".to_owned(),
            "root-version-flag".to_owned(),
            "scriptable-ctx-connection".to_owned(),
            "strict-project-name".to_owned(),
            "volume-remove-auto-confirm-env".to_owned(),
        ])
    );
}

fn deviations(markdown: &str) -> BTreeSet<String> {
    markdown
        .lines()
        .map(|line| {
            line.strip_prefix("- `")
                .and_then(|line| line.split_once('`'))
                .map(|(id, _)| id.to_owned())
                .unwrap_or_else(|| panic!("malformed CLI deviation: {line}"))
        })
        .collect()
}

fn reference_shape(
    markdown: &str,
    deviations: &BTreeSet<String>,
) -> (String, BTreeMap<String, Flag>, Vec<Positional>) {
    let heading = markdown
        .lines()
        .find_map(|line| line.strip_prefix("# uc"))
        .expect("command reference heading");
    let command_path = format!("ployz{heading}");
    let mut flags = BTreeMap::new();
    let mut in_options = false;
    let mut current: Option<(String, Option<char>, String)> = None;

    for line in markdown.lines() {
        if line == "## Options" || line == "## Options inherited from parent commands" {
            finish_flag(&mut flags, current.take());
            in_options = true;
            continue;
        }
        if line.starts_with("## ") {
            finish_flag(&mut flags, current.take());
            in_options = false;
            continue;
        }
        if !in_options {
            continue;
        }
        if let Some((long, short)) = option_name(line) {
            finish_flag(&mut flags, current.take());
            current = Some((long, short, line.trim().to_owned()));
        } else if let Some((_, _, text)) = current.as_mut() {
            text.push(' ');
            text.push_str(line.trim());
        }
    }
    finish_flag(&mut flags, current);
    if command_path == "ployz" && deviations.contains("root-version-flag") {
        flags.insert(
            "version".into(),
            Flag {
                short: Some('V'),
                default: None,
                env: None,
            },
        );
    }
    if command_path == "ployz caddy config" && deviations.contains("plain-caddy-config") {
        flags.remove("no-color");
    }
    if command_path == "ployz volume rm" && deviations.contains("volume-remove-auto-confirm-env") {
        flags.get_mut("yes").expect("volume rm has --yes").env = Some("PLOYZ_AUTO_CONFIRM".into());
    }
    if command_path == "ployz machine rm" && deviations.contains("machine-remove-named-data-loss") {
        flags.get_mut("yes").expect("machine rm has --yes").env = Some("PLOYZ_AUTO_CONFIRM".into());
    }
    if matches!(command_path.as_str(), "ployz images" | "ployz image ls")
        && deviations.contains("images-json-output")
    {
        flags.insert(
            "output".into(),
            Flag {
                short: Some('o'),
                default: None,
                env: None,
            },
        );
    }
    if deviations.contains("deploy-intent-flags") {
        let skip_health = Flag {
            short: None,
            default: None,
            env: None,
        };
        match command_path.as_str() {
            "ployz run" | "ployz service run" | "ployz caddy deploy" => {
                flags.insert("skip-health".into(), skip_health);
                flags.insert(
                    "recreate".into(),
                    Flag {
                        short: None,
                        default: None,
                        env: None,
                    },
                );
            }
            "ployz scale" | "ployz service scale" => {
                flags.insert("skip-health".into(), skip_health);
            }
            _ => {}
        }
    }
    if deviations.contains("project-name-no-short-flag") {
        let project_name = Flag {
            short: None,
            default: None,
            env: Some("COMPOSE_PROJECT_NAME".into()),
        };
        match command_path.as_str() {
            "ployz deploy"
            | "ployz run"
            | "ployz service run"
            | "ployz scale"
            | "ployz service scale"
            | "ployz rm"
            | "ployz service rm" => {
                flags.insert("project-name".into(), project_name);
            }
            _ => {}
        }
    }
    let positionals = reference_positionals(markdown, &command_path, deviations);
    (command_path, flags, positionals)
}

fn option_name(line: &str) -> Option<(String, Option<char>)> {
    let trimmed = if line.starts_with("  -") || line.starts_with("      --") {
        line.trim_start()
    } else {
        return None;
    };
    if let Some(rest) = trimmed.strip_prefix("--") {
        return Some((rename_flag(rest.split_whitespace().next()?), None));
    }
    let short = trimmed.strip_prefix('-')?.chars().next()?;
    let (_, long) = trimmed.split_once(", --")?;
    Some((rename_flag(long.split_whitespace().next()?), Some(short)))
}

fn rename_flag(flag: &str) -> String {
    if flag == "legacy-config" {
        "ployz-config".into()
    } else {
        flag.into()
    }
}

fn finish_flag(
    flags: &mut BTreeMap<String, Flag>,
    current: Option<(String, Option<char>, String)>,
) {
    let Some((long, short, text)) = current else {
        return;
    };
    let env = text
        .split_once("[$")
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(name, _)| name.replacen("LEGACY_", "PLOYZ_", 1));
    let default = reference_default(&text).map(|value| {
        if value == "~/.config/legacy/config.yaml" {
            "~/.config/ployz/config.yaml".into()
        } else {
            value
        }
    });
    assert!(
        flags
            .insert(
                long.clone(),
                Flag {
                    short,
                    default,
                    env
                }
            )
            .is_none(),
        "duplicate --{long}"
    );
}

fn reference_default(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("(default ")?;
    let (value, _) = rest.split_once(')')?;
    if let Some(value) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        return Some(value.into());
    }
    if value.chars().all(|character| character.is_ascii_digit())
        || matches!(value, "SIGTERM" | "compose.yaml")
    {
        return Some(value.into());
    }
    None
}

fn collect_clap_shapes(command: &Command, parent: &str, shapes: &mut BTreeMap<String, Shape>) {
    let path = if parent.is_empty() {
        command.get_name().to_owned()
    } else {
        format!("{parent} {}", command.get_name())
    };
    let aliases = command
        .get_all_aliases()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let flags = command
        .get_arguments()
        .filter_map(|arg| {
            let long = arg.get_long()?;
            let default = if matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse) {
                None
            } else {
                arg.get_default_values()
                    .first()
                    .map(|value| value.to_string_lossy().into_owned())
            };
            Some((
                long.to_owned(),
                Flag {
                    short: arg.get_short(),
                    default,
                    env: arg
                        .get_env()
                        .map(|value| value.to_string_lossy().into_owned()),
                },
            ))
        })
        .collect();
    let mut positionals = command
        .get_arguments()
        .filter_map(|arg| {
            let index = arg.get_index()?;
            let multiple = arg
                .get_num_args()
                .is_some_and(|range| range.max_values() > 1);
            Some((
                index,
                Positional {
                    required: arg.is_required_set(),
                    multiple,
                },
            ))
        })
        .collect::<Vec<_>>();
    positionals.sort_by_key(|(index, _)| *index);
    let positionals = positionals
        .into_iter()
        .map(|(_, positional)| positional)
        .collect();
    shapes.insert(
        path.clone(),
        Shape {
            aliases,
            flags,
            positionals,
        },
    );
    for child in command
        .get_subcommands()
        .filter(|child| child.get_name() != "help")
    {
        collect_clap_shapes(child, &path, shapes);
    }
}

fn reference_aliases(path: &str) -> BTreeSet<String> {
    let aliases: &[&str] = match path {
        "ployz caddy logs" | "ployz machine logs" | "ployz service logs" => &["log"],
        "ployz ctx" => &["context"],
        "ployz ctx connection" => &["conn"],
        "ployz ctx ls" | "ployz image ls" | "ployz machine ls" | "ployz service ls"
        | "ployz volume ls" | "ployz project ls" => &["list"],
        "ployz machine" => &["m"],
        "ployz service" => &["svc"],
        "ployz machine rm" | "ployz service rm" | "ployz volume rm" | "ployz project rm" => {
            &["delete", "remove"]
        }
        _ => &[],
    };
    aliases.iter().map(|alias| (*alias).into()).collect()
}

fn reference_positionals(
    markdown: &str,
    command_path: &str,
    deviations: &BTreeSet<String>,
) -> Vec<Positional> {
    let Some(synopsis) = markdown
        .lines()
        .find(|line| line.starts_with("uc ") && line.ends_with("[flags]"))
    else {
        return Vec::new();
    };
    let command_words = command_path.split_whitespace().count() - 1;
    let arguments = synopsis
        .strip_prefix("uc ")
        .unwrap()
        .split_whitespace()
        .skip(command_words)
        .collect::<Vec<_>>()
        .join(" ");
    let mut positionals: Vec<(String, Positional)> = Vec::new();
    for token in synopsis_tokens(&arguments) {
        if matches!(token.as_str(), "[flags]" | "[FLAGS]" | "[OPTIONS]") {
            continue;
        }
        let optional = outer_brackets(&token);
        let multiple = token.contains("...");
        let name = token
            .trim_matches(['[', ']'])
            .trim_end_matches("...")
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        if multiple
            && positionals
                .last()
                .is_some_and(|(previous, _)| previous == &name)
        {
            positionals.last_mut().unwrap().1.multiple = true;
            continue;
        }
        positionals.push((
            name,
            Positional {
                required: !optional,
                multiple,
            },
        ));
    }
    if command_path == "ployz machine init" && deviations.contains("local-machine-init-stub") {
        positionals
            .first_mut()
            .expect("machine init has a destination positional")
            .1
            .required = false;
    }
    if command_path == "ployz ctx connection" && deviations.contains("scriptable-ctx-connection") {
        positionals.push((
            "connection".into(),
            Positional {
                required: false,
                multiple: false,
            },
        ));
    }
    if command_path == "ployz machine rm" && deviations.contains("machine-remove-named-data-loss") {
        positionals.push((
            "data-loss".into(),
            Positional {
                required: false,
                multiple: true,
            },
        ));
    }
    positionals
        .into_iter()
        .map(|(_, positional)| positional)
        .collect()
}

fn synopsis_tokens(synopsis: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    for character in synopsis.chars() {
        match character {
            '[' => depth += 1,
            ']' => depth -= 1,
            ' ' if depth == 0 => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn outer_brackets(token: &str) -> bool {
    if !token.starts_with('[') {
        return false;
    }
    let mut depth = 0;
    for (index, character) in token.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return index == token.len() - 1;
                }
            }
            _ => {}
        }
    }
    false
}

#[test]
fn ployz_owned_environment_surface_is_frozen() {
    assert_eq!(
        ployz::cli::env::ALL,
        [
            "PLOYZ_AUTO_CONFIRM",
            "COMPOSE_DISABLE_ENV_FILE",
            "COMPOSE_FILE",
            "COMPOSE_PROJECT_NAME",
            "PLOYZ_CONFIG",
            "PLOYZ_CONNECT",
            "PLOYZ_CONTEXT",
            "PLOYZ_DAEMON_VERSION",
            "DEBUG",
            "PLOYZ_FAILED_CONTAINER_LOGS_TAIL",
            "PLOYZ_HEALTH_MONITOR_PERIOD",
            "PLOYZ_SSH_CONTROL_PERSIST",
        ]
    );
}

#[test]
fn native_completion_is_generated_for_every_supported_shell() {
    for shell in [
        Shell::Bash,
        Shell::Elvish,
        Shell::Fish,
        Shell::PowerShell,
        Shell::Zsh,
    ] {
        let mut output = Vec::new();
        generate(shell, &mut ployz::cli::command(), "ployz", &mut output);
        let output = String::from_utf8(output).unwrap();
        assert!(!output.is_empty(), "empty {shell:?} completion");
        assert!(output.contains("ployz"), "unnamed {shell:?} completion");
        assert!(
            !output.contains("LEGACY"),
            "legacy name in {shell:?} completion"
        );
    }
}
