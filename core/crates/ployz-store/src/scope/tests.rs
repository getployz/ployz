//! Every way the Store writes Working State, on a Protected and an unprotected
//! Environment, for a caller who does not hold `environments:protect`: which writes
//! validate, which meet the Protected fence, and which leave the Working State alone.

use std::collections::BTreeSet;

use ployz_core::config::{ServiceGitAccess, ServiceGitBranch, ServiceSource};
use ployz_core::{RpcError, ServiceName};
use serde_json::{Value, json};

use super::*;
use crate::{
    Actor, AuthorizedRepository, Command, ConfigStore, CreateGitService, CreateProject,
    OrganizationId, ProjectId, ProjectName, Publish, SealingKey, ServiceLineageId,
    SetEnvironmentProtected, Trusted,
};

#[derive(Clone, Copy, Debug)]
enum Mode {
    Validated,
    Trusted,
    Removed,
}

#[derive(Clone, Copy, Debug)]
enum Edit {
    Source,
    Reserved,
}

fn write(
    tx: &mut dyn Tx,
    environment: &mut Environment,
    mode: Mode,
    caller: &Trusted,
) -> Result<(), RpcError> {
    let evidence = Evidence::Caller(caller);
    match mode {
        Mode::Validated => save_working(tx, environment, caller),
        Mode::Trusted => write_working(
            tx,
            evidence,
            environment,
            WorkingWrite::Trusted(Reason::Publish),
        ),
        Mode::Removed => write_working(tx, evidence, environment, WorkingWrite::Removed),
    }
}

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

/// What a write left behind, read in the transaction that made it.
#[derive(Debug, PartialEq)]
struct Observed {
    refused: Option<(String, Value)>,
    revision: u64,
    branch: String,
    slug: String,
    removed: bool,
}

/// Project `shop` with Default `production` running Git Service `web` from `acme/web`'s
/// `main`, published; Protected, or not.
fn shop(protected: bool) -> (ConfigStore, EnvironmentId) {
    let store = ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    let environment = EnvironmentId::parse(uuid(2)).unwrap();
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: environment.clone(),
            },
        )
        .unwrap();
    let repository = crate::RepositoryName::parse("acme/web").unwrap();
    let evidence = Trusted {
        repositories: vec![AuthorizedRepository {
            repository: repository.clone(),
            repository_id: crate::RepositoryId::parse(11).unwrap(),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: crate::BranchName::parse("main").unwrap(),
            branches: vec![crate::BranchName::parse("dev").unwrap()],
        }],
        ..Trusted::default()
    };
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(3)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                repository,
                branch: None,
            },
            &evidence,
        )
        .unwrap();
    store
        .write(
            &who,
            &Command::Publish(Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    if !protected {
        store
            .write(
                &who,
                &SetEnvironmentProtected {
                    environment: EnvironmentRef::default(),
                    protected: false,
                    seen: None,
                },
            )
            .unwrap();
    }
    (store, environment)
}

/// `edit` made to `production`'s Working State in memory, then written as `mode` by a
/// caller who holds no permission.
fn attempt(protected: bool, mode: Mode, edit: Edit) -> (Observed, Observed) {
    let (store, id) = shop(protected);
    let nobody = Trusted {
        permissions: Some(BTreeSet::new()),
        ..Trusted::default()
    };
    store
        .storage
        .write(|tx| {
            let mut environment = load_by_id(tx, &id)?;
            let before = observe(tx, &id, None)?;
            match (mode, edit) {
                (Mode::Removed, _) => environment.removed.record(&uuid(9), Some("gone")),
                (_, Edit::Source) => {
                    let service = environment.working.services.first_mut().unwrap();
                    let ServiceSource::Git { branch, .. } = &mut service.config.source else {
                        panic!("web is a Git Service");
                    };
                    *branch = ServiceGitBranch::Connected { name: "dev".into() };
                }
                (_, Edit::Reserved) => {
                    environment.working.services.first_mut().unwrap().slug = "setup".into();
                }
            }
            let result = write(tx, &mut environment, mode, &nobody);
            Ok((before, observe(tx, &id, result.err())?))
        })
        .unwrap()
}

fn observe(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    refused: Option<RpcError>,
) -> Result<Observed, RpcError> {
    let environment = load_by_id(tx, id)?;
    let service = environment.working.services.first().unwrap();
    let branch = match &service.config.source {
        ServiceSource::Git {
            branch: ServiceGitBranch::Connected { name },
            ..
        } => name.clone(),
        other @ (ServiceSource::Empty { .. }
        | ServiceSource::Git { .. }
        | ServiceSource::Image { .. }) => panic!("web is a Git Service on a branch: {other:?}"),
    };
    Ok(Observed {
        refused: refused.map(|error| (error.code.to_string(), error.details)),
        revision: environment.summary.revision.0,
        branch,
        slug: service.slug.clone(),
        removed: environment.removed.contains(&uuid(9)),
    })
}

fn forbidden_for_web() -> Option<(String, Value)> {
    Some((
        "forbidden".into(),
        json!({ "permission": "environments:protect", "services": ["web"] }),
    ))
}

#[test]
fn each_write_of_working_state_validates_or_meets_the_fence_as_it_always_did() {
    // (protected, mode, edit) -> what the write left behind, against the state before it.
    let table = [
        // A new source: every write of Working State meets the fence on a Protected
        // Environment, validated or not, and neither stores it when refused.
        (
            true,
            Mode::Validated,
            Edit::Source,
            forbidden_for_web(),
            0,
            "main",
            "web",
            false,
        ),
        (
            true,
            Mode::Trusted,
            Edit::Source,
            forbidden_for_web(),
            0,
            "main",
            "web",
            false,
        ),
        (
            false,
            Mode::Validated,
            Edit::Source,
            None,
            1,
            "dev",
            "web",
            false,
        ),
        (
            false,
            Mode::Trusted,
            Edit::Source,
            None,
            1,
            "dev",
            "web",
            false,
        ),
        // A reserved name: only the validated write refuses it, wherever it writes.
        (
            true,
            Mode::Validated,
            Edit::Reserved,
            Some(("invalid_argument".into(), json!({ "service": "setup" }))),
            0,
            "main",
            "web",
            false,
        ),
        (
            true,
            Mode::Trusted,
            Edit::Reserved,
            None,
            1,
            "main",
            "setup",
            false,
        ),
        (
            false,
            Mode::Validated,
            Edit::Reserved,
            Some(("invalid_argument".into(), json!({ "service": "setup" }))),
            0,
            "main",
            "web",
            false,
        ),
        (
            false,
            Mode::Trusted,
            Edit::Reserved,
            None,
            1,
            "main",
            "setup",
            false,
        ),
        // Removals leave Working State, its revision and the fence alone.
        (
            true,
            Mode::Removed,
            Edit::Source,
            None,
            0,
            "main",
            "web",
            true,
        ),
        (
            false,
            Mode::Removed,
            Edit::Source,
            None,
            0,
            "main",
            "web",
            true,
        ),
    ];
    for (protected, mode, edit, refused, bumped, branch, slug, removed) in table {
        let (before, after) = attempt(protected, mode, edit);
        let expected = Observed {
            refused,
            revision: before.revision + bumped,
            branch: branch.into(),
            slug: slug.into(),
            removed,
        };
        assert_eq!(
            after, expected,
            "protected: {protected}, {mode:?}, {edit:?}"
        );
    }
}
