//! Cluster-side Project names named on the command line.

use clap::ArgMatches;
use ployz_core::{ProjectName, ValueError};
use thiserror::Error;

/// A Project name that cannot be used.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ProjectError {
    #[error(transparent)]
    InvalidName(#[from] ValueError),
    #[error("Project '{name}' is reserved for Ployz infrastructure")]
    Reserved { name: ProjectName },
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

/// The user Project named by `--project-name`, if the command has one.
///
/// # Errors
///
/// Returns when the name is invalid or reserved.
pub(crate) fn explicit(matches: &ArgMatches) -> Result<Option<ProjectName>, ProjectError> {
    let Ok(Some(value)) = matches.try_get_one::<String>("project-name") else {
        return Ok(None);
    };
    let name = ProjectName::parse(value)?;
    refuse_reserved(&name)?;
    Ok(Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
