//! Secrets a Conditional Sync brings by name only, and the Destination's values held
//! for them until the merge.

use super::*;

/// production renamed `web` to `frontend` after PR #5 opened: the value is held by
/// the secret's row, whatever either side names its Service.
#[test]
fn a_value_is_held_by_row_whatever_the_destination_names_the_service() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &ployz_store::RenameService {
                environment: at("production"),
                service: ployz_core::ServiceName::parse("web").unwrap(),
                name: ployz_core::ServiceName::parse("frontend").unwrap(),
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "pr-secret" }))],
    );
    let review = offered(&store, &who, None);
    assert_eq!(*review.rows[0].at.row(), row("TOKEN"));
    store.write(&who, &sync(&review, None)).unwrap();
    store.write(&who, &hold("TOKEN", "prod-secret")).unwrap();
    assert_eq!(
        check(&store, &who),
        (true, "1 change goes live with this PR".into())
    );
}

/// PR #5 syncs TOKEN and production's value is held for the merge, but production
/// sets its own first: the check stops waiting, and the merge leaves production's
/// own value, with no hint.
#[test]
fn a_secret_the_destination_sets_itself_leaves_the_held_value_unused() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "pr-secret" }))],
    );
    let review = offered(&store, &who, None);
    store
        .write(&who, &sync(&review, Some(&["web.env.TOKEN"])))
        .unwrap();
    assert_eq!(
        check(&store, &who),
        (false, "Waiting for production's value of TOKEN".into())
    );
    store.write(&who, &hold("TOKEN", "held")).unwrap();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": "own" }))],
    );
    assert_eq!(
        check(&store, &who),
        (true, "1 change goes live with this PR".into())
    );
    publish(&store, &who, "production");
    assert_eq!(push(&store, &who, 4, &[]).admitted.len(), 1);
    observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(false, Some(MERGE), Some(commit(4)), 2)),
    );
    let hints = store
        .read(
            &who,
            &DiffQuery {
                environment: at("production"),
            },
        )
        .unwrap()
        .hints;
    assert!(hints.is_empty(), "{hints:?}");
    set(&store, &who, "production", &[("web.env.MODE", json!("x"))]);
    publish(&store, &who, "production");
    let pushed = push(&store, &who, 5, &[]);
    assert_eq!(
        resolved(&store, &pushed.admitted[0].deployment.id, "TOKEN"),
        json!("own")
    );
}

/// A value held for a merge that never comes drops with the pull request: reopened
/// and synced again, it waits for both secrets again.
#[test]
fn a_held_value_drops_when_the_pull_request_closes_unmerged() {
    let (store, who) = shop();
    let secrets = [
        ("web.env.TOKEN", json!({ "secret": "pr-token" })),
        ("web.env.KEY", json!({ "secret": "pr-key" })),
    ];
    set(&store, &who, "pr-5", &secrets);
    store
        .write(&who, &sync(&offered(&store, &who, None), None))
        .unwrap();
    assert_eq!(
        check(&store, &who),
        (false, "Waiting for production's value of 2 secrets".into())
    );
    store.write(&who, &hold("TOKEN", "held")).unwrap();
    assert_eq!(
        check(&store, &who),
        (false, "Waiting for production's value of KEY".into())
    );
    let closed = observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(false, None, None, 2)),
    );
    assert_eq!(closed.removed.len(), 1, "{closed:?}");
    assert!(env(&store, &who, "production").get("TOKEN").is_none());

    observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(true, None, None, 3)),
    );
    set(&store, &who, "pr-5", &secrets);
    store
        .write(&who, &sync(&offered(&store, &who, None), None))
        .unwrap();
    assert_eq!(
        check(&store, &who),
        (false, "Waiting for production's value of 2 secrets".into())
    );
}

/// Every `saved` document of a Conditional Sync in the Store at `url`.
fn saved_documents(url: &str) -> Vec<String> {
    let sql = "SELECT stored FROM config_conditional_sync";
    match url.strip_prefix("sqlite:") {
        Some(path) => {
            let connection = rusqlite::Connection::open(path).unwrap();
            let mut statement = connection.prepare(sql).unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        }
        None => postgres::Client::connect(url, postgres::NoTls)
            .unwrap()
            .query(sql, &[])
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect(),
    }
}

/// Neither the pull request's secret nor the value given with the Sync is in the
/// Conditional Sync's document.
#[test]
fn a_conditional_sync_keeps_no_secret_value() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let opened = ConfigStore::open(&url, backend::key()).unwrap();
    let (store, who) = shop_in(opened, "production");
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "pr-secret" }))],
    );
    let synced = SyncChanges {
        values: BTreeMap::from([(row("TOKEN").into(), "prod-secret".into())]),
        ..sync(&offered(&store, &who, None), None)
    };
    store.write(&who, &synced).unwrap();
    let saved = saved_documents(&url);
    assert_eq!(saved.len(), 1);
    assert!(!saved[0].contains("ciphertext"), "{}", saved[0]);
}

/// [`shop`], where acme/docs deploys into production too and its PR #5 (`pr-5-2`)
/// copies web.
fn shop_with_docs() -> (ConfigStore, Actor) {
    let (store, who) = shop();
    let evidence = Trusted {
        repositories: vec![AuthorizedRepository {
            repository: backend::repo_name("acme/docs"),
            repository_id: backend::repo_id(12),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: backend::git_branch("main"),
            branches: Vec::new(),
        }],
        ..Trusted::default()
    };
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ServiceName::parse("docs").unwrap(),
                repository: backend::repo_name("acme/docs"),
                branch: None,
            },
            &evidence,
        )
        .unwrap();
    publish(&store, &who, "production");
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/docs"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy: Some(vec![ployz_store::NodeName::parse("web").unwrap()]),
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    let docs = PullRequest {
        repository_id: backend::repo_id(12),
        ..facts(true, None, None, 1)
    };
    let opened = observe(&store, &who, SystemEvent::PullRequest(docs));
    assert_eq!(opened.admitted.len(), 1);
    (store, who)
}

/// Before either syncs, two PR Environments with the number leave the hint's
/// sender to the user.
#[test]
fn a_value_held_before_the_sync_of_two_repositories_names_no_pr_environment() {
    let (store, who) = shop_with_docs();
    let refused = store
        .write(&who, &hold("TOKEN", "prod-secret"))
        .unwrap_err();
    assert_eq!(
        refused.details["next"],
        json!("ployz env sync --to production --at-merge --project shop --env PR_ENV")
    );
}

#[test]
fn a_value_held_for_a_pull_request_number_two_repositories_share_is_held_for_both() {
    let (store, who) = shop_with_docs();
    let token = [("web.env.TOKEN", json!({ "secret": "pr-secret" }))];
    for pr in ["pr-5", "pr-5-2"] {
        set(&store, &who, pr, &token);
        let query = SyncQuery {
            from: at(pr),
            into: None,
            when: Some(ployz_store::When::AtMerge),
        };
        store
            .write(&who, &sync(&store.read(&who, &query).unwrap(), None))
            .unwrap();
    }
    let passing = |repository: u64| {
        store
            .read(
                &who,
                &PullRequestQuery {
                    repository_id: backend::repo_id(repository),
                    number: backend::pr_number(5),
                },
            )
            .unwrap()
            .passing
    };
    assert_eq!((passing(11), passing(12)), (false, false));
    store.write(&who, &hold("TOKEN", "prod-secret")).unwrap();
    assert_eq!((passing(11), passing(12)), (true, true));
}

/// A value given with a Sync at the merge is held for it, or neither stands; given
/// with a Sync now, it lands in the receiver at once.
#[test]
fn a_value_for_a_secret_lands_with_its_sync_or_neither_does() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "pr-secret" }))],
    );
    let with = |review: &SyncView, key: &str, value: &str| SyncChanges {
        values: BTreeMap::from([(row(key).into(), value.into())]),
        ..sync(review, Some(&[&format!("web.env.{key}")]))
    };
    let review = offered(&store, &who, None);
    // Nothing to hold the value for: the Sync doesn't stand either.
    let mut nope = with(&review, "TOKEN", "prod-secret");
    nope.values = BTreeMap::from([(row("NOPE").into(), "prod-secret".into())]);
    assert_eq!(
        store.write(&who, &nope).unwrap_err().code,
        RpcErrorCode::InvalidArgument
    );
    assert_eq!(
        check(&store, &who),
        (false, "1 change to sync in Ployz".into())
    );
    store
        .write(&who, &with(&review, "TOKEN", "prod-secret"))
        .unwrap();
    assert_eq!(
        check(&store, &who),
        (true, "1 change goes live with this PR".into())
    );

    // A Sync now lands with the value given.
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.KEY", json!({ "secret": "pr-key" }))],
    );
    let now = offered_now(&store, &who, "production");
    store.write(&who, &with(&now, "KEY", "prod-key")).unwrap();
    publish(&store, &who, "production");
    let pushed = push(&store, &who, 4, &[]);
    assert_eq!(
        resolved(&store, &pushed.admitted[0].deployment.id, "KEY"),
        json!("prod-key")
    );
}

/// PR #5 syncs TOKEN with a value held, but production stages its own and doesn't
/// publish it: the check stops waiting, and the merge lands no held value over it,
/// leaving the pull request's value a hint.
#[test]
fn a_secret_the_destination_stages_itself_keeps_the_held_value_out_of_saved_state() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "pr-secret" }))],
    );
    let review = offered(&store, &who, None);
    store
        .write(&who, &sync(&review, Some(&["web.env.TOKEN"])))
        .unwrap();
    store.write(&who, &hold("TOKEN", "held")).unwrap();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": "own" }))],
    );
    assert_eq!(
        check(&store, &who),
        (true, "1 change goes live with this PR".into())
    );
    observe(
        &store,
        &who,
        SystemEvent::PullRequest(facts(false, Some(MERGE), None, 2)),
    );
    let pushed = push(&store, &who, 4, &[MERGE]);
    let hints = store
        .read(
            &who,
            &DiffQuery {
                environment: at("production"),
            },
        )
        .unwrap()
        .hints;
    assert_eq!(
        texts(
            &hints
                .iter()
                .map(|hint| hint.at.to_string())
                .collect::<Vec<_>>()
        ),
        ["web.env.TOKEN"]
    );
    assert_eq!(
        resolved(&store, &pushed.admitted[0].deployment.id, "TOKEN"),
        Value::Null
    );
}
