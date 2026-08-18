//! Resolve a Cluster-side Project name from Compose project-name precedence.

use std::{
    fmt,
    path::{Component, Path},
};

use clap::{ArgMatches, parser::ValueSource};
use ployz_core::{ProjectName, ValueError};
use thiserror::Error;

/// Why a resolved Project name was chosen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectNameSource {
    CommandLine,
    ComposeProjectName,
    ComposeName,
    ComposeProjectDirectory,
    CurrentDirectory,
    Default,
}

impl fmt::Display for ProjectNameSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CommandLine => "command-line project name",
            Self::ComposeProjectName => "COMPOSE_PROJECT_NAME",
            Self::ComposeName => "top-level Compose name",
            Self::ComposeProjectDirectory => "Compose project directory",
            Self::CurrentDirectory => "current directory",
            Self::Default => "default",
        })
    }
}

/// One Project name plus the precedence level that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProject {
    pub name: ProjectName,
    pub source: ProjectNameSource,
}

/// Inputs for Compose project-name precedence. Absent sources are skipped.
#[derive(Clone, Debug, Default)]
pub struct ProjectNameInput<'a> {
    pub command_line: Option<&'a str>,
    pub compose_project_name: Option<&'a str>,
    pub compose_name: Option<&'a str>,
    pub compose_project_directory: Option<&'a Path>,
    pub current_directory: Option<&'a Path>,
    pub implicit_default: bool,
}

/// A Project name that cannot be used, or a directory with no usable basename.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ProjectError {
    #[error(transparent)]
    InvalidName(#[from] ValueError),
    #[error("Project '{name}' is reserved for Ployz infrastructure")]
    Reserved { name: ProjectName },
    #[error("no Project name source was provided")]
    NoSource,
}

/// Resolve one Project name. The first present source wins; invalid values are
/// not normalised and do not fall through. A directory with no basename is
/// skipped.
///
/// # Errors
///
/// Returns when a present source is not a valid Project Name, or no source
/// produced a name.
pub fn resolve_project_name(input: &ProjectNameInput<'_>) -> Result<ResolvedProject, ProjectError> {
    if let Some(value) = input.command_line {
        return parsed(value, ProjectNameSource::CommandLine);
    }
    if let Some(value) = input.compose_project_name {
        return parsed(value, ProjectNameSource::ComposeProjectName);
    }
    if let Some(value) = input.compose_name {
        return parsed(value, ProjectNameSource::ComposeName);
    }
    if let Some(directory) = input.compose_project_directory
        && let Some(name) = directory_basename(directory)
    {
        return parsed(name, ProjectNameSource::ComposeProjectDirectory);
    }
    if let Some(directory) = input.current_directory
        && let Some(name) = directory_basename(directory)
    {
        return parsed(name, ProjectNameSource::CurrentDirectory);
    }
    if input.implicit_default {
        return parsed("default", ProjectNameSource::Default);
    }
    Err(ProjectError::NoSource)
}

/// Refuse a reserved Project name on deployment and removal commands.
///
/// # Errors
///
/// Returns [`ProjectError::Reserved`] when `name` is `ployz-system`.
pub fn refuse_reserved(name: &ProjectName) -> Result<(), ProjectError> {
    if name.is_reserved() {
        Err(ProjectError::Reserved { name: name.clone() })
    } else {
        Ok(())
    }
}

fn user_project(resolved: ResolvedProject) -> Result<ResolvedProject, ProjectError> {
    refuse_reserved(&resolved.name)?;
    Ok(resolved)
}

/// Fill CLI / `COMPOSE_PROJECT_NAME` from `matches`, then resolve `rest`.
///
/// # Errors
///
/// Returns when a present source is not a valid Project Name, or the name is reserved.
pub(crate) fn resolve_from_matches(
    matches: &ArgMatches,
    rest: ProjectNameInput<'_>,
) -> Result<ResolvedProject, ProjectError> {
    let (command_line, compose_project_name) = cli_sources(matches);
    user_project(resolve_project_name(&ProjectNameInput {
        command_line,
        compose_project_name,
        compose_name: rest.compose_name,
        compose_project_directory: rest.compose_project_directory,
        current_directory: rest.current_directory,
        implicit_default: rest.implicit_default,
    })?)
}

/// Resolve remaining Compose sources against the current directory.
///
/// `compose_dir` is omitted when there is no Compose project, so the plan
/// header can still name current directory as the source.
///
/// # Errors
///
/// Returns when a present source is not a valid Project Name, or the name is reserved.
pub(crate) fn resolve_compose_command(
    matches: &ArgMatches,
    compose_name: Option<&str>,
    compose_dir: Option<&Path>,
) -> Result<ResolvedProject, ProjectError> {
    let cwd = std::env::current_dir().ok();
    let compose_dir = compose_dir.map(|dir| match cwd.as_deref() {
        Some(cwd) => cwd.join(dir),
        None => dir.to_path_buf(),
    });
    resolve_from_matches(
        matches,
        ProjectNameInput {
            compose_name,
            compose_project_directory: compose_dir.as_deref(),
            current_directory: cwd.as_deref(),
            ..ProjectNameInput::default()
        },
    )
}

/// Resolve a Project for `ployz run`. Absent CLI / env sources become `default`.
///
/// # Errors
///
/// Returns when a present source is not a valid Project Name, or the name is reserved.
pub(crate) fn resolve_run_command(matches: &ArgMatches) -> Result<ResolvedProject, ProjectError> {
    resolve_from_matches(
        matches,
        ProjectNameInput {
            implicit_default: true,
            ..ProjectNameInput::default()
        },
    )
}

/// Resolve a Project only when CLI or `COMPOSE_PROJECT_NAME` named one.
///
/// # Errors
///
/// Returns when a present source is not a valid Project Name, or the name is reserved.
pub(crate) fn resolve_explicit(
    matches: &ArgMatches,
) -> Result<Option<ResolvedProject>, ProjectError> {
    let (command_line, compose_project_name) = cli_sources(matches);
    if command_line.is_none() && compose_project_name.is_none() {
        return Ok(None);
    }
    resolve_from_matches(matches, ProjectNameInput::default()).map(Some)
}

fn cli_sources(matches: &ArgMatches) -> (Option<&str>, Option<&str>) {
    let Ok(Some(value)) = matches.try_get_one::<String>("project-name") else {
        return (None, None);
    };
    match matches.value_source("project-name") {
        Some(ValueSource::CommandLine) => (Some(value.as_str()), None),
        Some(ValueSource::EnvVariable) => (None, Some(value.as_str())),
        _ => (None, None),
    }
}

fn parsed(value: &str, source: ProjectNameSource) -> Result<ResolvedProject, ProjectError> {
    Ok(ResolvedProject {
        name: ProjectName::parse(value)?,
        source,
    })
}

fn directory_basename(path: &Path) -> Option<&str> {
    path.components()
        .rev()
        .find_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(name: &str, source: ProjectNameSource) -> ResolvedProject {
        ResolvedProject {
            name: ProjectName::parse(name).unwrap(),
            source,
        }
    }

    #[test]
    fn precedence_picks_the_first_present_source() {
        let directory = Path::new("/tmp/from-dir");
        let cwd = Path::new("/tmp/from-cwd");
        assert_eq!(
            resolve_project_name(&ProjectNameInput {
                command_line: Some("from-cli"),
                compose_project_name: Some("from-env"),
                compose_name: Some("from-compose"),
                compose_project_directory: Some(directory),
                current_directory: Some(cwd),
                implicit_default: true,
            })
            .unwrap(),
            resolved("from-cli", ProjectNameSource::CommandLine)
        );
        assert_eq!(
            resolve_project_name(&ProjectNameInput {
                compose_project_name: Some("from-env"),
                compose_name: Some("from-compose"),
                compose_project_directory: Some(directory),
                current_directory: Some(cwd),
                implicit_default: true,
                ..ProjectNameInput::default()
            })
            .unwrap(),
            resolved("from-env", ProjectNameSource::ComposeProjectName)
        );
        assert_eq!(
            resolve_project_name(&ProjectNameInput {
                compose_name: Some("from-compose"),
                compose_project_directory: Some(directory),
                current_directory: Some(cwd),
                ..ProjectNameInput::default()
            })
            .unwrap(),
            resolved("from-compose", ProjectNameSource::ComposeName)
        );
        assert_eq!(
            resolve_project_name(&ProjectNameInput {
                compose_project_directory: Some(directory),
                current_directory: Some(cwd),
                ..ProjectNameInput::default()
            })
            .unwrap(),
            resolved("from-dir", ProjectNameSource::ComposeProjectDirectory)
        );
        assert_eq!(
            resolve_project_name(&ProjectNameInput {
                current_directory: Some(cwd),
                ..ProjectNameInput::default()
            })
            .unwrap(),
            resolved("from-cwd", ProjectNameSource::CurrentDirectory)
        );
        assert_eq!(
            resolve_project_name(&ProjectNameInput {
                implicit_default: true,
                ..ProjectNameInput::default()
            })
            .unwrap(),
            resolved("default", ProjectNameSource::Default)
        );
    }

    #[test]
    fn present_invalid_names_are_rejected_without_falling_through() {
        let error = resolve_project_name(&ProjectNameInput {
            command_line: Some("My_App"),
            compose_name: Some("shop"),
            ..ProjectNameInput::default()
        })
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid Project Name \"My_App\": a 1-63 character lowercase DNS label; underscores and uppercase are not accepted"
        );
        let env = resolve_project_name(&ProjectNameInput {
            compose_project_name: Some("SHOP"),
            compose_name: Some("shop"),
            ..ProjectNameInput::default()
        })
        .unwrap_err();
        assert!(env.to_string().contains("SHOP"), "{env}");
        assert!(!env.to_string().contains("shop\""), "{env}");
    }

    #[test]
    fn directory_names_are_not_normalised() {
        let error = resolve_project_name(&ProjectNameInput {
            compose_project_directory: Some(Path::new("/tmp/My_App")),
            current_directory: Some(Path::new("/tmp/shop")),
            ..ProjectNameInput::default()
        })
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid Project Name \"My_App\": a 1-63 character lowercase DNS label; underscores and uppercase are not accepted"
        );
    }

    #[test]
    fn run_without_a_project_uses_default() {
        assert_eq!(
            resolve_project_name(&ProjectNameInput {
                implicit_default: true,
                ..ProjectNameInput::default()
            })
            .unwrap(),
            resolved("default", ProjectNameSource::Default)
        );
    }

    #[test]
    fn reserved_names_parse_and_are_refused() {
        let name = ProjectName::parse("ployz-system").unwrap();
        assert!(name.is_reserved());
        assert_eq!(
            refuse_reserved(&name).unwrap_err().to_string(),
            "Project 'ployz-system' is reserved for Ployz infrastructure"
        );
        refuse_reserved(&ProjectName::parse("shop").unwrap()).unwrap();
    }

    #[test]
    fn source_labels_match_the_plan_header() {
        assert_eq!(
            ProjectNameSource::CommandLine.to_string(),
            "command-line project name"
        );
        assert_eq!(
            ProjectNameSource::ComposeProjectName.to_string(),
            "COMPOSE_PROJECT_NAME"
        );
        assert_eq!(
            ProjectNameSource::ComposeName.to_string(),
            "top-level Compose name"
        );
        assert_eq!(
            ProjectNameSource::ComposeProjectDirectory.to_string(),
            "Compose project directory"
        );
        assert_eq!(
            ProjectNameSource::CurrentDirectory.to_string(),
            "current directory"
        );
        assert_eq!(ProjectNameSource::Default.to_string(), "default");
    }

    #[test]
    fn missing_directory_basename_falls_through_to_the_next_source() {
        assert_eq!(
            resolve_project_name(&ProjectNameInput {
                compose_project_directory: Some(Path::new("/")),
                current_directory: Some(Path::new("/tmp/from-cwd")),
                ..ProjectNameInput::default()
            })
            .unwrap(),
            resolved("from-cwd", ProjectNameSource::CurrentDirectory)
        );
        assert_eq!(
            resolve_project_name(&ProjectNameInput {
                compose_project_directory: Some(Path::new(".")),
                current_directory: Some(Path::new("/tmp/shop/.")),
                ..ProjectNameInput::default()
            })
            .unwrap(),
            resolved("shop", ProjectNameSource::CurrentDirectory)
        );
        assert_eq!(
            resolve_project_name(&ProjectNameInput::default())
                .unwrap_err()
                .to_string(),
            "no Project name source was provided"
        );
    }

    #[test]
    fn relative_dot_directory_has_no_basename_of_its_own() {
        assert!(directory_basename(Path::new(".")).is_none());
        assert_eq!(directory_basename(Path::new("/tmp/shop/.")), Some("shop"));
        assert_eq!(directory_basename(Path::new("/")), None);
    }
}
