//! How a Store refusal becomes this command's failure, and the retry line it names.

use clap::ArgMatches;
use ployz_agent_tools::Confirmation;
use ployz_core::{Hint, RpcErrorCode};
use ployz_store::Stale;

use super::super::Error;
use super::{Store, next};
use crate::approval::Asked;
use crate::cli::Rerun;
use crate::cloud::account::StoreCallError;

impl From<StoreCallError> for Error {
    fn from(error: StoreCallError) -> Self {
        match error {
            StoreCallError::Refused(mut error) => {
                if error.code == RpcErrorCode::Conflict {
                    Stale::keep_version(&mut error.details);
                }
                error.into()
            }
            StoreCallError::Cloud(error) => error.into(),
            StoreCallError::Stopped(failure) => failure,
        }
    }
}

impl Store<'_> {
    /// This command's failure for a Store error: an ambiguous Project is fixed by
    /// linking this directory to one, after which the same command runs as typed.
    pub(crate) fn fail(&self, error: impl Into<Refusal>) -> Error {
        let Refusal {
            mut error,
            mut hint,
        } = with_next(
            error,
            |refusal| {
                refusal.code == RpcErrorCode::Ambiguous
                    && ployz_store::lists_projects(&refusal.details)
            },
            || next(self.matches, &["link", "--project", "PROJECT"]),
        );
        if let (None, StoreCallError::Refused(refused)) = (&hint, &mut error)
            && let Some(asked) = Asked::of(refused)
        {
            if self.secrets {
                refused.message = format!(
                    "{}. --approval can't go with --value, so the retry reads each --value \
                     from stdin again. To finish in one run, run it without --json: it asks \
                     yes or no.",
                    refused.message.trim_end_matches('.')
                );
            }
            hint = Some(self.approving(asked.digest()).into());
        }
        Error::from(error).hint(hint)
    }

    /// This command line again, with what [`Self::retrying`] gives it.
    pub(crate) fn again(&self) -> Rerun {
        self.retry_sets
            .iter()
            .fold(Rerun::again(self.root), |again, (id, value)| {
                again.set(id, value)
            })
    }

    /// This command's failure for a Store error; a refusal to delete Volume data
    /// names this command again accepting each Volume it lists, at the version it
    /// reviewed.
    pub(crate) fn accepting(&self, error: impl Into<Refusal>) -> Error {
        let refusal = match error.into() {
            Refusal {
                error: StoreCallError::Refused(error),
                ..
            } if error.code == RpcErrorCode::ConfirmationRequired => {
                let carried = self.carried();
                let confirmation = Confirmation::of(&error.details);
                let mut accept: Vec<String> = Vec::new();
                for name in accepted_names(&confirmation) {
                    if !accept.contains(&name) {
                        accept.push(name);
                    }
                }
                let version = confirmation.version;
                let retry = self.retry(Carried {
                    accept,
                    version: version.or(carried.version),
                    ..carried
                });
                Refusal {
                    error: StoreCallError::Refused(error),
                    hint: Some(retry.into()),
                }
            }
            refusal => refusal,
        };
        self.fail(refusal)
    }

    /// A deploy's refusal about the review it named. A conflict handing back the
    /// fresh review (`details.diff`) names this command again at its version,
    /// without the Volume losses accepted at the stale one: what the fresh review
    /// deletes is for its own refusal to name. When the fresh review's loss
    /// (`details.volume_loss`) is only of Volumes this command already accepts, it
    /// names them again at the version binding that loss, so the line goes
    /// through in one step. Any other conflict names `read`, as
    /// [`with_refresh_hint`] does. A refusal to delete data that this command
    /// already accepts each Volume of is about its version, and says so; a deploy's
    /// loss refusal says nothing but what it deletes, which the retry line names.
    pub(crate) fn at_review(&self, error: impl Into<Refusal>, read: &str) -> Refusal {
        let mut refusal = error.into();
        if let StoreCallError::Refused(error) = &mut refusal.error
            && error.code == RpcErrorCode::ConfirmationRequired
        {
            let carried = self.carried();
            let asked = Confirmation::of(&error.details);
            let accept = accepted_names(&asked);
            if let (Some(typed), Some(bound)) = (&carried.version, &asked.version)
                && typed != bound
                && !accept.is_empty()
                && accept.iter().all(|name| carried.accept.contains(name))
            {
                error.message = format!(
                    "{} {typed} doesn't cover the data this deletes. Retry with the version \
                     below",
                    self.version_flag().1
                );
            }
        }
        let carried = self.carried();
        let next = match &refusal.error {
            StoreCallError::Refused(error) if error.code == RpcErrorCode::Conflict => {
                let stale = Stale::of(&error.details);
                let accept = accepted_names(&stale.loss);
                match stale.loss.version {
                    Some(bound)
                        if !accept.is_empty()
                            && accept.iter().all(|name| carried.accept.contains(name)) =>
                    {
                        Some((bound, accept))
                    }
                    Some(_) | None => stale.version.map(|version| (version, Vec::new())),
                }
            }
            StoreCallError::Refused(_) | StoreCallError::Cloud(_) | StoreCallError::Stopped(_) => {
                None
            }
        };
        if let (Some((_, accept)), StoreCallError::Refused(error)) = (&next, &mut refusal.error)
            && !accept.is_empty()
        {
            error.message = format!(
                "{}. The command below still accepts deleting the data of {}.",
                error.message.trim_end_matches('.'),
                accept.join(", ")
            );
        }
        match next {
            Some((version, accept)) => with_next(
                refusal,
                |_| true,
                || {
                    self.retry(Carried {
                        accept,
                        version: Some(version),
                        ..carried
                    })
                    .line()
                },
            ),
            None => with_refresh_hint(refusal, self.matches, read),
        }
    }

    /// This command again carrying `digest`, the yes it needs; a run that gives secrets
    /// on stdin asks again instead, so its retry names each `--value` row and no digest,
    /// as does a command that takes no `--approval`, or none beside the flags it was given.
    pub(super) fn approving(&self, digest: &str) -> Rerun {
        let path = super::super::command_path(self.root);
        (!self.secrets && super::super::takes_value(path.split(' '), "approval"))
            .then(|| {
                self.retry(Carried {
                    approval: Some(digest.to_owned()),
                    ..self.carried()
                })
            })
            .filter(Rerun::parses)
            .unwrap_or_else(|| self.retry(self.carried()))
    }

    /// The flag this command passes a reviewed version with: `discard`'s and
    /// `history`'s `--version`, or `--expect-version` elsewhere. Read from the
    /// command tree: `ArgMatches` refuses unknown ids only in debug builds.
    fn version_flag(&self) -> (&'static str, &'static str) {
        let path = super::super::command_path(self.root);
        let takes_version = super::super::takes_value(path.split(' '), "version");
        if takes_version {
            ("version", "--version")
        } else {
            ("expect-version", "--expect-version")
        }
    }

    fn carried(&self) -> Carried {
        let one = |id| {
            self.matches
                .try_get_one::<String>(id)
                .ok()
                .flatten()
                .cloned()
        };
        Carried {
            accept: super::super::string_values(self.matches, "accept-volume-loss"),
            version: one(self.version_flag().0),
            approval: one("approval"),
        }
    }

    fn retry(&self, carried: Carried) -> Rerun {
        self.again()
            .set_all(
                "accept-volume-loss",
                carried.accept.iter().map(String::as_str),
            )
            .set_all(self.version_flag().0, carried.version.as_deref())
            .set_all("approval", carried.approval.as_deref())
    }
}

/// The Volume names a refusal's `accept` lists, flat or by Environment: every string
/// of a list, whatever else it holds.
pub(super) fn accepted_names(confirmation: &Confirmation) -> Vec<String> {
    let names = |list: &serde_json::Value| {
        list.as_array()
            .into_iter()
            .flatten()
            .filter_map(|name| name.as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    match &confirmation.written_accept {
        Some(serde_json::Value::Object(by_environment)) => {
            by_environment.values().flat_map(names).collect()
        }
        Some(list) => names(list),
        None => Vec::new(),
    }
}

struct Carried {
    accept: Vec<String>,
    version: Option<String>,
    approval: Option<String>,
}

/// A Store error and the hint this command adds to it.
pub(crate) struct Refusal {
    pub(super) error: StoreCallError,
    pub(super) hint: Option<Hint>,
}

impl From<StoreCallError> for Refusal {
    fn from(error: StoreCallError) -> Self {
        Self { error, hint: None }
    }
}

/// A refused stale write names the `read` command that shows the fresh state.
pub(crate) fn with_refresh_hint(
    error: impl Into<Refusal>,
    matches: &ArgMatches,
    read: &str,
) -> Refusal {
    with_next(
        error,
        // A conflict that already names its next step (no Server: `ployz server add`) keeps it.
        |refusal| refusal.code == RpcErrorCode::Conflict && refusal.details.get("next").is_none(),
        || next(matches, &[read]),
    )
}

/// A refusal `when` picks, with no hint yet, names `next` as the command to run next.
pub(crate) fn with_next(
    error: impl Into<Refusal>,
    when: impl FnOnce(&ployz_core::RpcError) -> bool,
    next: impl FnOnce() -> String,
) -> Refusal {
    let mut refusal = error.into();
    if let (None, StoreCallError::Refused(error)) = (&refusal.hint, &refusal.error)
        && when(error)
    {
        refusal.hint = Some(Hint::Next(next()));
    }
    refusal
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::*;
    use super::*;

    /// The names a refusal's `accept` listed, read from the raw value: every string
    /// of a list, or of each list of an Environment map, and nothing else.
    fn names_listed_in(accept: Option<&serde_json::Value>) -> Vec<String> {
        let names = |list: &serde_json::Value| {
            list.as_array()
                .into_iter()
                .flatten()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        };
        match accept {
            Some(serde_json::Value::Object(by_environment)) => {
                by_environment.values().flat_map(names).collect()
            }
            Some(list) => names(list),
            None => Vec::new(),
        }
    }

    #[test]
    fn the_names_a_refusal_accepts_keep_every_string_whatever_else_its_accept_holds() {
        let table = [
            ("absent", json!({})),
            ("null", json!({ "accept": null })),
            ("a number", json!({ "accept": 3 })),
            ("a flat list", json!({ "accept": ["data", "logs"] })),
            ("a mixed list", json!({ "accept": ["data", 7, "logs"] })),
            (
                "a name no Volume has",
                json!({ "accept": ["data", "Bad Name"] }),
            ),
            (
                "by Environment",
                json!({ "accept": { "production": ["data"], "staging": ["logs"] } }),
            ),
            (
                "by Environment with a mixed list",
                json!({ "accept": { "production": ["data", 7], "staging": 9 } }),
            ),
            ("an empty map", json!({ "accept": {} })),
        ];
        for (name, details) in table {
            assert_eq!(
                refusal::accepted_names(&Confirmation::of(&details)),
                names_listed_in(details.get("accept")),
                "{name}"
            );
        }
    }

    #[test]
    fn a_refused_volume_loss_names_the_exact_retry_keeping_the_approval() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "deploy", "--env", "staging", "--approval", "apr_1"])
            .unwrap();
        let key = SealingKey::new(&[7; 32]).unwrap();
        let local = ConfigStore::open("sqlite::memory:", key).unwrap();
        let store = Store::for_test(
            Backend::Local(
                std::sync::Arc::new(local),
                Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
            ),
            &root,
        );
        let refused = ployz_core::RpcError::confirmation_required(
            "This Deploy permanently deletes the data of data",
        )
        .with_details(json!({ "version": "3:1:0.1", "accept": ["data"] }));
        let error = store.accepting(StoreCallError::Refused(refused));
        let retry = "ployz deploy --env staging --approval apr_1 --accept-volume-loss data --expect-version 3:1:0.1";
        assert_eq!(error.hints(), [crate::cli::retry_hint(retry)]);
        let error = error.report();
        assert_eq!(error.details.get("retry"), Some(&json!(retry)));
        assert_eq!(
            error.message,
            "This Deploy permanently deletes the data of data"
        );
    }

    #[test]
    fn a_skip_deploy_retry_keeps_its_flag_scope_names_and_approval() {
        let typed = [
            "ployz",
            "deploy",
            "--skip-deploy",
            "--project",
            "shop",
            "--env",
            "production",
            "--expect-version",
            "3:1:0.1",
            "--approval",
            "apr_1",
        ];
        let root = crate::cli::command().try_get_matches_from(typed).unwrap();
        let key = SealingKey::new(&[7; 32]).unwrap();
        let store = Store::for_test(
            Backend::Local(
                std::sync::Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
                Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
            ),
            &root,
        );
        let refused = ployz_core::RpcError::confirmation_required(
            "Next deploy deletes the data of pg-data, logs. Accept each by name",
        )
        .with_details(json!({ "version": "3:1:0.1:9f2c", "accept": ["pg-data", "logs"] }));
        let loss = "ployz deploy --skip-deploy --project shop --env production --expect-version 3:1:0.1:9f2c \
                    --approval apr_1 --accept-volume-loss pg-data --accept-volume-loss logs";
        assert_eq!(
            store.accepting(StoreCallError::Refused(refused)).hints(),
            [crate::cli::retry_hint(loss)]
        );
        let approval = store.approving("apr_2").line();
        assert_eq!(
            approval,
            "ployz deploy --skip-deploy --project shop --env production --expect-version 3:1:0.1 --approval apr_2"
        );
        let reparsed = |retry: &str| {
            let root = crate::cli::command()
                .try_get_matches_from(shell_words::split(retry).unwrap())
                .unwrap();
            let leaf = leaf_matches(&root);
            let mut words: Vec<String> = root
                .subcommand_name()
                .into_iter()
                .map(str::to_owned)
                .collect();
            words.push(leaf.get_flag("skip-deploy").to_string());
            for id in ["project", "env", "expect-version", "approval"] {
                words.extend(leaf.get_one::<String>(id).cloned());
            }
            words.extend(crate::handlers::string_values(leaf, "accept-volume-loss"));
            words
        };
        assert_eq!(
            reparsed(loss),
            [
                "deploy",
                "true",
                "shop",
                "production",
                "3:1:0.1:9f2c",
                "apr_1",
                "pg-data",
                "logs"
            ]
        );
        assert_eq!(
            reparsed(&approval),
            ["deploy", "true", "shop", "production", "3:1:0.1", "apr_2"]
        );
    }

    /// An approval retry carries the yes only where the command takes it beside the flags
    /// typed; a line clap would refuse is no retry.
    #[test]
    fn an_approval_retry_carries_the_yes_only_where_it_parses() {
        for (words, retry) in [
            (
                &["env", "protect", "production", "--off"][..],
                "ployz env protect production --off --approval apr_2",
            ),
            (
                &["volume", "set", "data", "--shared-writes=false"],
                "ployz volume set data '--shared-writes=false'",
            ),
        ] {
            let root = crate::cli::command()
                .try_get_matches_from(std::iter::once("ployz").chain(words.iter().copied()))
                .unwrap();
            let key = SealingKey::new(&[7; 32]).unwrap();
            let store = Store::for_test(
                Backend::Local(
                    std::sync::Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
                    Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
                ),
                &root,
            );
            assert_eq!(store.approving("apr_2").line(), retry, "{words:?}");
        }
    }

    #[test]
    fn a_volume_loss_retry_passes_the_version_with_the_flag_its_command_takes() {
        for words in [
            &["deploy"][..],
            &["project", "rm", "shop", "--confirm", "shop"],
            &["env", "rm", "br", "--confirm", "shop/br"],
        ] {
            let root = crate::cli::command()
                .try_get_matches_from(std::iter::once("ployz").chain(words.iter().copied()))
                .unwrap();
            let key = SealingKey::new(&[7; 32]).unwrap();
            let store = Store::for_test(
                Backend::Local(
                    std::sync::Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
                    Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
                ),
                &root,
            );
            let refused = ployz_core::RpcError::confirmation_required(
                "This permanently deletes the data of data",
            )
            .with_details(json!({ "version": "3:1:0.1", "accept": ["data"] }));
            let hints = store.accepting(StoreCallError::Refused(refused)).hints();
            let [Hint::Retry(retry)] = &hints[..] else {
                panic!("{words:?} names no retry");
            };
            let reparsed = crate::cli::command()
                .try_get_matches_from(shell_words::split(retry.command()).unwrap())
                .unwrap_or_else(|error| panic!("{retry}: {error}"));
            assert_eq!(
                leaf_matches(&reparsed)
                    .get_one::<String>("expect-version")
                    .map(String::as_str),
                Some("3:1:0.1"),
                "{retry}"
            );
        }
    }

    #[test]
    fn a_copy_loss_retry_runs_and_carries_clouds_version_back_verbatim() {
        let typed = [
            "ployz",
            "deploy",
            "--project",
            "mirrors",
            "--env",
            "production",
        ];
        let root = crate::cli::command().try_get_matches_from(typed).unwrap();
        let key = SealingKey::new(&[7; 32]).unwrap();
        let store = Store::for_test(
            Backend::Local(
                std::sync::Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
                Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
            ),
            &root,
        );
        let refused = ployz_core::RpcError::confirmation_required(
            "Deploying loses data of data. Accept each by name",
        )
        .with_details(json!({ "version": "5:1:0.1#loss-fzfjky", "accept": ["data"] }));
        let hints = store.accepting(StoreCallError::Refused(refused)).hints();
        let retry = "ployz deploy --project mirrors --env production --accept-volume-loss data \
                     --expect-version '5:1:0.1#loss-fzfjky'";
        assert_eq!(hints, [crate::cli::retry_hint(retry)]);
        let reparsed = crate::cli::command()
            .try_get_matches_from(shell_words::split(retry).unwrap())
            .unwrap_or_else(|error| panic!("{retry}: {error}"));
        let leaf = leaf_matches(&reparsed);
        assert_eq!(
            leaf.get_one::<String>("expect-version").map(String::as_str),
            Some("5:1:0.1#loss-fzfjky")
        );
        assert_eq!(
            crate::handlers::string_values(leaf, "accept-volume-loss"),
            ["data"]
        );
    }

    #[test]
    fn publish_is_not_a_command() {
        assert!(
            crate::cli::command()
                .try_get_matches_from(["ployz", "publish"])
                .is_err()
        );
    }

    #[test]
    fn a_refused_review_names_this_command_at_the_fresh_version() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "deploy", "--env", "staging"])
            .unwrap();
        let key = SealingKey::new(&[7; 32]).unwrap();
        let local = ConfigStore::open("sqlite::memory:", key).unwrap();
        let store = Store::for_test(
            Backend::Local(
                std::sync::Arc::new(local),
                Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
            ),
            &root,
        );
        let next_of = |details| {
            let refusal = store.at_review(
                StoreCallError::Refused(
                    ployz_core::RpcError::conflict(String::from(
                        "This draft includes a pull request: review it and pass its version",
                    ))
                    .with_details(details),
                ),
                "diff",
            );
            Error::from(refusal.error).hint(refusal.hint).hints()
        };
        assert_eq!(
            next_of(json!({ "diff": { "version": "3:1:0.1" } })),
            [Hint::Next(
                "ployz deploy --env staging --expect-version 3:1:0.1".into()
            )]
        );
        assert_eq!(
            next_of(json!({})),
            [Hint::Next("ployz diff --env staging".into())]
        );
    }

    /// A stale version's conflict that hands back the fresh review's loss names it at
    /// the version binding it, when this command already accepts each of its
    /// Volumes; one it doesn't accept leaves the losses behind, as before.
    #[test]
    fn a_stale_version_carries_a_fresh_loss_it_already_accepts() {
        let typed = [
            "ployz",
            "deploy",
            "--accept-volume-loss",
            "big1",
            "--accept-volume-loss",
            "big2",
            "--expect-version",
            "1:1:0.1:deadbeef",
            "--env",
            "staging",
        ];
        let root = crate::cli::command().try_get_matches_from(typed).unwrap();
        let key = SealingKey::new(&[7; 32]).unwrap();
        let store = Store::for_test(
            Backend::Local(
                std::sync::Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
                Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
            ),
            &root,
        );
        let next = |accept: serde_json::Value| {
            let refusal = store.at_review(
                StoreCallError::Refused(
                    ployz_core::RpcError::conflict(String::from(
                        "The Environment changed after this review. Review the latest \
                              changes and try again",
                    ))
                    .with_details(json!({
                        "diff": { "version": "33:4:0.4" },
                        "volume_loss": { "version": "33:4:0.4:cafe", "accept": accept },
                    })),
                ),
                "diff",
            );
            let message = match &refusal.error {
                StoreCallError::Refused(error) => error.message.clone(),
                StoreCallError::Cloud(_) | StoreCallError::Stopped(_) => String::new(),
            };
            (
                message,
                Error::from(refusal.error).hint(refusal.hint).hints(),
            )
        };
        let (message, hints) = next(json!(["big1"]));
        assert_eq!(
            message,
            "The Environment changed after this review. Review the latest changes and try \
             again. The command below still accepts deleting the data of big1."
        );
        assert_eq!(
            hints,
            [Hint::Next(
                "ployz deploy --accept-volume-loss big1 --expect-version 33:4:0.4:cafe --env staging"
                    .into()
            )]
        );
        assert_eq!(
            next(json!(["big1", "big3"])).1,
            [Hint::Next(
                "ployz deploy --expect-version 33:4:0.4 --env staging".into()
            )]
        );
    }

    /// A stale version's next line drops the Volume losses accepted at it, so running
    /// it shows what the fresh review deletes, and its retry line binds that.
    #[test]
    fn a_stale_version_leaves_its_accepted_volume_loss_behind() {
        let typed = [
            "ployz",
            "deploy",
            "--accept-volume-loss",
            "big1",
            "--expect-version",
            "1:1:0.1:deadbeef",
            "--env",
            "staging",
        ];
        let root = crate::cli::command().try_get_matches_from(typed).unwrap();
        let key = SealingKey::new(&[7; 32]).unwrap();
        let store = Store::for_test(
            Backend::Local(
                std::sync::Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
                Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
            ),
            &root,
        );
        let refusal = store.at_review(
            StoreCallError::Refused(
                ployz_core::RpcError::conflict(String::from(
                    "The Environment changed after this review. Review the latest changes \
                          and try again",
                ))
                .with_details(json!({ "diff": { "version": "33:4:0.4" } })),
            ),
            "diff",
        );
        assert_eq!(
            Error::from(refusal.error).hint(refusal.hint).hints(),
            [Hint::Next(
                "ployz deploy --expect-version 33:4:0.4 --env staging".into()
            )]
        );
    }

    /// Accepting every Volume a refusal lists, at a version that doesn't bind their
    /// loss, is refused for the version: naming them again would change nothing.
    #[test]
    fn a_version_short_of_the_loss_is_refused_for_the_version() {
        let accepting = |typed: &[&str], deploy: bool| {
            let root = crate::cli::command().try_get_matches_from(typed).unwrap();
            let key = SealingKey::new(&[7; 32]).unwrap();
            let store = Store::for_test(
                Backend::Local(
                    std::sync::Arc::new(ConfigStore::open("sqlite::memory:", key).unwrap()),
                    Actor::system(OrganizationId::parse(LOCAL_ORGANIZATION).unwrap()),
                ),
                &root,
            );
            let refused = StoreCallError::Refused(
                ployz_core::RpcError::confirmation_required(String::from(
                    "This permanently deletes the data of big1, big2. Accept each by name",
                ))
                .with_details(json!({
                    "version": "33:4:0.4:481da69fb1927724",
                    "accept": ["big1", "big2"],
                })),
            );
            let error = if deploy {
                store.accepting(store.at_review(refused, "diff"))
            } else {
                store.accepting(refused)
            };
            (error.hints(), error.report().message)
        };
        let retry = "ployz deploy --accept-volume-loss big1 --accept-volume-loss big2 \
                     --expect-version 33:4:0.4:481da69fb1927724";
        let all = [
            "ployz",
            "deploy",
            "--accept-volume-loss",
            "big2",
            "--accept-volume-loss",
            "big1",
            "--expect-version",
            "33:4:0.4",
        ];
        let (hints, message) = accepting(&all, true);
        assert_eq!(hints, [crate::cli::retry_hint(retry)]);
        assert_eq!(
            message,
            "--expect-version 33:4:0.4 doesn't cover the data this deletes. Retry with the \
             version below"
        );
        // A removal's refusal lists more than Volumes; it keeps its message.
        let (hints, message) = accepting(&all, false);
        assert_eq!(hints, [crate::cli::retry_hint(retry)]);
        assert_eq!(
            message,
            "This permanently deletes the data of big1, big2. Accept each by name"
        );
        // Naming only some of them is still answered by naming each.
        let (hints, message) = accepting(
            &[
                "ployz",
                "deploy",
                "--accept-volume-loss",
                "big1",
                "--expect-version",
                "33:4:0.4",
            ],
            true,
        );
        assert_eq!(hints, [crate::cli::retry_hint(retry)]);
        assert_eq!(
            message,
            "This permanently deletes the data of big1, big2. Accept each by name"
        );
    }

    #[test]
    fn a_conflict_keeps_the_next_step_the_store_named() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "deploy", "--env", "staging"])
            .unwrap();
        let refused = |details| {
            StoreCallError::Refused(ployz_core::RpcError::conflict("refused").with_details(details))
        };
        let next_of = |error| {
            let refusal = with_refresh_hint(error, leaf_matches(&root), "diff");
            Error::from(refusal.error).hint(refusal.hint).hints()
        };
        assert_eq!(
            next_of(refused(json!({ "next": "ployz server add" }))),
            [Hint::Next("ployz server add".into())]
        );
        assert_eq!(
            next_of(refused(json!({}))),
            [Hint::Next("ployz diff --env staging".into())]
        );
    }
}
