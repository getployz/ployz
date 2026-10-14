//! What a Dockerfile tells about the Service it builds, read the way Docker reads it.

use std::collections::BTreeSet;

use serde::Serialize;

/// The facts a Dockerfile states about its image: what the final stage exposes and
/// sets, and what the build takes as input. Variables are names only: values may be
/// secrets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DockerfileFacts {
    /// The final stage's `EXPOSE`d ports, in the order it exposes them.
    pub ports: Vec<u16>,
    /// The final stage's `ENV` names.
    pub env: BTreeSet<String>,
    /// The `ARG` names of every stage: each is a build input.
    pub args: BTreeSet<String>,
}

impl DockerfileFacts {
    /// Read `dockerfile`. A stage built `FROM` an earlier stage starts with that
    /// stage's ports and `ENV`.
    // ponytail: what the final stage inherits from its base image isn't seen.
    #[must_use]
    pub fn parse(dockerfile: &str) -> Self {
        let mut facts = Self::default();
        let mut stage = None;
        let mut named: Vec<(String, Vec<u16>, BTreeSet<String>)> = Vec::new();
        for (keyword, arguments) in instructions(dockerfile) {
            match keyword.as_str() {
                "FROM" => {
                    if let Some(name) = stage.take() {
                        named.push((name, facts.ports.clone(), facts.env.clone()));
                    }
                    let mut words = arguments
                        .split_whitespace()
                        .filter(|word| !word.starts_with("--"));
                    let base = words.next().unwrap_or_default();
                    stage = words
                        .next()
                        .filter(|word| word.eq_ignore_ascii_case("AS"))
                        .and_then(|_| words.next())
                        .map(str::to_owned);
                    let earlier = named
                        .iter()
                        .rev()
                        .find(|(name, ..)| name.eq_ignore_ascii_case(base));
                    (facts.ports, facts.env) = earlier
                        .map(|(_, ports, env)| (ports.clone(), env.clone()))
                        .unwrap_or_default();
                }
                "EXPOSE" => {
                    let ports = arguments
                        .split_whitespace()
                        .filter_map(|port| port.split('/').next()?.parse().ok());
                    for port in ports {
                        if !facts.ports.contains(&port) {
                            facts.ports.push(port);
                        }
                    }
                }
                "ENV" => facts.env.extend(env_names(&arguments)),
                "ARG" => facts
                    .args
                    .extend(words(&arguments).into_iter().filter_map(name_of)),
                _ => {}
            }
        }
        facts
    }
}

/// `ENV a=1 b=2` sets each name; the legacy `ENV a 1 2` sets only `a`.
fn env_names(arguments: &str) -> Vec<String> {
    let words = words(arguments);
    match words.first() {
        Some(first) if !first.contains('=') => vec![first.clone()],
        _ => words.into_iter().filter_map(name_of).collect(),
    }
}

/// The name a `NAME=value` or bare `NAME` word declares.
fn name_of(word: String) -> Option<String> {
    let name = word.split('=').next().unwrap_or_default();
    (!name.is_empty()).then(|| name.to_owned())
}

/// `arguments` split at whitespace outside quotes, so a quoted value's spaces
/// don't start a word.
fn words(arguments: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut characters = arguments.chars();
    while let Some(character) = characters.next() {
        match (quote, character) {
            (_, '\\') => {
                word.push(character);
                word.extend(characters.next());
            }
            (None, '"' | '\'') => {
                quote = Some(character);
                word.push(character);
            }
            (Some(open), _) if open == character => {
                quote = None;
                word.push(character);
            }
            (None, _) if character.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(character),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Each instruction of `dockerfile` as its upper-cased keyword and its arguments:
/// lines continued with the escape character (`\\`, or a `# escape=` directive's)
/// joined, and comment and blank lines dropped, as Docker reads them.
// ponytail: heredoc bodies read as instructions; a stray `ARG` there only adds a build
// input, a stray `FROM` hides an `EXPOSE`. Use BuildKit's parser if that bites.
fn instructions(dockerfile: &str) -> Vec<(String, String)> {
    let mut escape = '\\';
    let mut lines = dockerfile.lines().peekable();
    // Parser directives are the leading `# key=value` comments.
    while let Some((key, value)) = lines
        .peek()
        .and_then(|line| line.trim().strip_prefix('#')?.split_once('='))
    {
        if key.trim().eq_ignore_ascii_case("escape")
            && let Some(character) = value.trim().chars().next()
        {
            escape = character;
        }
        lines.next();
    }
    let mut instructions = Vec::new();
    // Like BuildKit: continued lines join byte for byte, keeping their leading
    // whitespace and inserting none, so `ARG API_\` + `URL` reads `ARG API_URL`.
    let mut current = String::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let line = if current.is_empty() {
            line.trim_start()
        } else {
            line
        };
        let (text, continued) = match line.trim_end_matches([' ', '\t']).strip_suffix(escape) {
            Some(text) => (text, true),
            None => (line, false),
        };
        current.push_str(text);
        if !continued {
            instructions.extend(instruction(&std::mem::take(&mut current)));
        }
    }
    instructions.extend(instruction(&current));
    instructions
}

fn instruction(text: &str) -> Option<(String, String)> {
    let text = text.trim();
    let (keyword, arguments) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    (!keyword.is_empty()).then(|| (keyword.to_ascii_uppercase(), arguments.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn instructions_join_continuations_and_drop_comments() {
        let instructions = instructions(
            "# syntax=docker/dockerfile:1\n# escape=`\nFROM node\nARG `\n  # a comment\n  API_URL `\n\n  MODE=x\nexpose 80",
        );
        assert_eq!(
            instructions,
            [
                ("FROM".to_owned(), "node".to_owned()),
                ("ARG".to_owned(), "API_URL   MODE=x".to_owned()),
                ("EXPOSE".to_owned(), "80".to_owned()),
            ]
        );
        assert_eq!(
            super::instructions("ARG \\\n API_URL"),
            [("ARG".to_owned(), "API_URL".to_owned())]
        );
        assert_eq!(
            super::instructions("ARG API_\\\nURL\nEXPOSE 80\\\n80"),
            [
                ("ARG".to_owned(), "API_URL".to_owned()),
                ("EXPOSE".to_owned(), "8080".to_owned()),
            ]
        );
    }

    #[test]
    fn the_final_stage_exposes_its_ports_in_order() {
        for (dockerfile, ports) in [
            ("FROM nginx\nEXPOSE 80\nEXPOSE 443 80", vec![80, 443]),
            ("FROM x\n  expose 3000/tcp 9000/udp", vec![3000, 9000]),
            ("FROM x\nEXPOSE \\\n  8080", vec![8080]),
            ("FROM x\nEXPOSE $PORT", vec![]),
            ("FROM x", vec![]),
            (
                "FROM node AS build\nEXPOSE 3000\nFROM nginx\nEXPOSE 80",
                vec![80],
            ),
            ("FROM node AS build\nEXPOSE 3000\nFROM nginx", vec![]),
        ] {
            assert_eq!(
                DockerfileFacts::parse(dockerfile).ports,
                ports,
                "{dockerfile}"
            );
        }
    }

    #[test]
    fn env_names_are_the_final_stages_without_values() {
        let facts = DockerfileFacts::parse(
            "FROM x AS build\nENV BUILD_ONLY=1\nFROM y\nENV A=\"a b\" B='c d' C=x\\ y\nENV LEGACY some value=1",
        );
        assert_eq!(facts.env, names(&["A", "B", "C", "LEGACY"]));
    }

    #[test]
    fn a_stage_from_an_earlier_stage_keeps_its_ports_and_env() {
        let facts = DockerfileFacts::parse(
            "FROM node AS base\nENV PORT=3000\nEXPOSE 3000\nFROM base AS build\nRUN make\nFROM --platform=$BUILDPLATFORM BASE\nENV MODE=prod",
        );
        assert_eq!(facts.ports, [3000]);
        assert_eq!(facts.env, names(&["MODE", "PORT"]));
    }

    #[test]
    fn args_are_every_stages_names_without_defaults() {
        let facts = DockerfileFacts::parse(
            "ARG BASE=node\nFROM $BASE AS build\nARG TOKEN\nFROM x\nARG GREETING=\"hello world\" MODE",
        );
        assert_eq!(facts.args, names(&["BASE", "GREETING", "MODE", "TOKEN"]));
    }
}
